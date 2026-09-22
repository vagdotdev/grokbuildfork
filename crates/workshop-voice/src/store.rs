//! The on-disk model store: one stable directory shared by every CLI version, the pinned file
//! verified by size and SHA-256, and a resumable, retrying download used by `/voice` to self-heal
//! (the installer implements the same rules in POSIX sh: `.partial`, resume, checksum before
//! rename, three attempts, mirror first).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::Error;
use crate::manifest::ModelPin;

/// Override for tests and CI; production uses `$WORKSHOP_HOME/voice`.
pub const MODEL_DIR_ENV: &str = "WORKSHOP_VOICE_DIR";
/// Free space required beyond the file itself before a download starts.
const DISK_MARGIN: u64 = 64 * 1024 * 1024;
const ATTEMPTS: usize = 3;

/// `$WORKSHOP_VOICE_DIR`, else `<workshop home>/voice` (`~/.workshop/voice`). Version-independent.
pub fn default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODEL_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    xai_dirs::grok_home().join("voice")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStatus {
    Ready,
    Missing,
    WrongSize { actual: u64 },
    BadChecksum { actual: String },
}

impl ModelStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ModelStatus::Ready)
    }

    pub fn describe(&self) -> String {
        match self {
            ModelStatus::Ready => "ok".into(),
            ModelStatus::Missing => "missing".into(),
            ModelStatus::WrongSize { actual } => format!("wrong size ({actual} bytes)"),
            ModelStatus::BadChecksum { actual } => {
                format!(
                    "checksum mismatch ({}…)",
                    actual.get(..12).unwrap_or(actual)
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub received: u64,
    pub total: u64,
    pub attempt: usize,
}

pub type ProgressFn<'a> = dyn Fn(Progress) + Send + Sync + 'a;

/// One tier's file in the shared voice directory.
#[derive(Debug, Clone)]
pub struct ModelStore {
    dir: PathBuf,
    lock: &'static ModelPin,
}

/// A full hash of a 547 MiB file costs one to two seconds; remember the files that passed so
/// later presses in the same process only pay a `stat`.
static VERIFIED: Mutex<Vec<(PathBuf, u64, Option<SystemTime>)>> = Mutex::new(Vec::new());

impl ModelStore {
    pub fn new(dir: PathBuf, pin: &'static ModelPin) -> Self {
        Self { dir, lock: pin }
    }

    /// Store for a tier id (`turbo` / `small` / `base`) under `dir`.
    pub fn for_tier(dir: &Path, tier: &str) -> Option<Self> {
        crate::manifest::pin(tier).map(|pin| Self::new(dir.to_path_buf(), pin))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn pin(&self) -> &'static ModelPin {
        self.lock
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.lock.file)
    }

    pub fn partial_path(&self) -> PathBuf {
        self.dir.join(format!("{}.partial", self.lock.file))
    }

    /// Size check only (cheap).
    pub fn quick_status(&self) -> ModelStatus {
        match std::fs::metadata(self.path()) {
            Ok(m) if m.is_file() && m.len() == self.lock.size => ModelStatus::Ready,
            Ok(m) if m.is_file() => ModelStatus::WrongSize { actual: m.len() },
            _ => ModelStatus::Missing,
        }
    }

    /// Size plus full SHA-256 against the lock. Blocking (run off the async runtime).
    pub fn status(&self) -> ModelStatus {
        let path = self.path();
        match self.quick_status() {
            ModelStatus::Ready => {}
            other => return other,
        }
        match sha256_file(&path) {
            Ok(actual) if actual == self.lock.sha256 => {
                remember_verified(&path);
                ModelStatus::Ready
            }
            Ok(actual) => ModelStatus::BadChecksum { actual },
            Err(_) => ModelStatus::Missing,
        }
    }

    /// [`Self::status`], skipping the hash when this exact file (path, size, mtime) already passed.
    pub fn status_cached(&self) -> ModelStatus {
        let path = self.path();
        if let Ok(meta) = std::fs::metadata(&path)
            && meta.len() == self.lock.size
            && let Ok(guard) = VERIFIED.lock()
            && guard.iter().any(|(p, len, mtime)| {
                *p == path && *len == meta.len() && *mtime == meta.modified().ok()
            })
        {
            return ModelStatus::Ready;
        }
        self.status()
    }

    /// Drop the verified marker (after the engine rejected the file, say) so the next check hashes again.
    pub fn forget_verified(&self) {
        let path = self.path();
        if let Ok(mut guard) = VERIFIED.lock() {
            guard.retain(|(p, _, _)| *p != path);
        }
    }

    /// Delete a file that failed verification so the next attempt starts clean.
    pub fn remove_bad_model(&self) {
        let _ = std::fs::remove_file(self.path());
        self.forget_verified();
    }

    /// Make the pinned model present and verified, downloading (or resuming) it if needed.
    ///
    /// Tries the release mirror first, then the upstream file, up to three attempts in total.
    /// Writes `<file>.partial`, resumes it with a `Range` request when the server allows,
    /// verifies size and SHA-256, then renames into place. A verified file is never rewritten.
    pub async fn ensure(&self, progress: &ProgressFn<'_>) -> Result<PathBuf, Error> {
        let store = self.clone();
        let status = tokio::task::spawn_blocking(move || store.status_cached())
            .await
            .map_err(|e| Error::Model(format!("verify task: {e}")))?;
        if status.is_ready() {
            return Ok(self.path());
        }
        if !matches!(status, ModelStatus::Missing) {
            tracing::warn!(status = %status.describe(), "voice model failed verification; replacing it");
            self.remove_bad_model();
        }
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|e| Error::Model(format!("create {}: {e}", self.dir.display())))?;

        let urls = self.lock.download_urls();
        let client = xai_grok_extra_ca::build_reqwest_client(|b| {
            b.connect_timeout(Duration::from_secs(20))
                .user_agent(concat!("workshop-voice/", env!("CARGO_PKG_VERSION")))
        })
        .map_err(|e| Error::Download(format!("http client: {e}")))?;

        let mut last_err = String::new();
        for attempt in 1..=ATTEMPTS {
            let url = urls
                .get((attempt - 1) % urls.len())
                .cloned()
                .unwrap_or_else(|| self.lock.upstream_url.clone());
            match self.download_once(&client, &url, attempt, progress).await {
                Ok(()) => return Ok(self.path()),
                Err(e) => {
                    tracing::warn!(attempt, %url, error = %e, "voice model download attempt failed");
                    last_err = format!("{url}: {e}");
                }
            }
        }
        Err(Error::Download(format!(
            "could not download the voice model after {ATTEMPTS} attempts; last error: {last_err}"
        )))
    }

    async fn download_once(
        &self,
        client: &reqwest::Client,
        url: &str,
        attempt: usize,
        progress: &ProgressFn<'_>,
    ) -> Result<(), Error> {
        let total = self.lock.size;
        let partial = self.partial_path();

        // Resume from an existing partial when it is plausibly ours (shorter than the file).
        let mut have = match tokio::fs::metadata(&partial).await {
            Ok(m) if m.len() < total => m.len(),
            Ok(_) => {
                let _ = tokio::fs::remove_file(&partial).await;
                0
            }
            Err(_) => 0,
        };
        check_disk_space(&self.dir, total.saturating_sub(have))?;

        let mut req = client.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Download(format!("request: {e}")))?;
        let status = resp.status();
        let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
        if !(status.is_success() || resumed) {
            return Err(Error::Download(format!("HTTP {status}")));
        }
        if have > 0 && !resumed {
            // Server ignored the range: start over rather than corrupt the partial.
            have = 0;
        }
        if let Some(len) = resp.content_length()
            && have + len != total
            && !resumed
        {
            return Err(Error::Download(format!(
                "server reports {len} bytes, the lock file pins {total}"
            )));
        }

        let mut hasher = Sha256::new();
        let mut file = if have > 0 {
            let existing = tokio::fs::read(&partial)
                .await
                .map_err(|e| Error::Download(format!("read partial: {e}")))?;
            hasher.update(&existing);
            let mut f = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&partial)
                .await
                .map_err(|e| Error::Download(format!("open partial: {e}")))?;
            f.seek(std::io::SeekFrom::End(0))
                .await
                .map_err(|e| Error::Download(format!("seek partial: {e}")))?;
            f
        } else {
            tokio::fs::File::create(&partial)
                .await
                .map_err(|e| Error::Download(format!("create partial: {e}")))?
        };
        progress(Progress {
            received: have,
            total,
            attempt,
        });

        let mut resp = resp;
        let mut received = have;
        loop {
            let chunk = tokio::time::timeout(Duration::from_secs(60), resp.chunk())
                .await
                .map_err(|_| Error::Download("stalled for 60 s".into()))?
                .map_err(|e| Error::Download(format!("read: {e}")))?;
            let Some(chunk) = chunk else { break };
            received += chunk.len() as u64;
            if received > total {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(Error::Download(
                    "server sent more bytes than the lock file pins".into(),
                ));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| Error::Download(format!("write partial: {e}")))?;
            progress(Progress {
                received,
                total,
                attempt,
            });
        }
        file.flush()
            .await
            .map_err(|e| Error::Download(format!("flush partial: {e}")))?;
        drop(file);

        if received != total {
            // Keep the partial: the next attempt resumes it.
            return Err(Error::Download(format!(
                "incomplete: {received} of {total} bytes"
            )));
        }
        let digest = hex(&hasher.finalize());
        if digest != self.lock.sha256 {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(Error::Download(format!(
                "checksum mismatch: got {digest}, lock file pins {}",
                self.lock.sha256
            )));
        }
        tokio::fs::rename(&partial, self.path())
            .await
            .map_err(|e| Error::Download(format!("rename into place: {e}")))?;
        remember_verified(&self.path());
        tracing::info!(path = %self.path().display(), "voice model ready");
        Ok(())
    }
}

fn remember_verified(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path)
        && let Ok(mut guard) = VERIFIED.lock()
    {
        guard.retain(|(p, _, _)| p != path);
        guard.push((path.to_path_buf(), meta.len(), meta.modified().ok()));
    }
}

fn check_disk_space(dir: &Path, needed: u64) -> Result<(), Error> {
    // The directory may not exist yet; measure its nearest existing ancestor.
    let mut probe = dir.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => return Ok(()),
        }
    }
    match fs2::available_space(&probe) {
        Ok(avail) if avail < needed + DISK_MARGIN => Err(Error::Download(format!(
            "not enough disk space in {}: {} MiB free, {} MiB needed for the voice model",
            dir.display(),
            avail / (1024 * 1024),
            (needed + DISK_MARGIN) / (1024 * 1024)
        ))),
        _ => Ok(()),
    }
}

/// Hex SHA-256 of a file (blocking; 1 MiB reads).
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(buf.get(..n).unwrap_or_default());
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_prefers_env_then_workshop_home() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        crate::test_support::with_env(
            &[
                (MODEL_DIR_ENV, Some("/tmp/voice-x")),
                ("WORKSHOP_HOME", Some("/tmp/wh")),
            ],
            || assert_eq!(default_dir(), PathBuf::from("/tmp/voice-x")),
        );
        crate::test_support::with_env(
            &[
                (MODEL_DIR_ENV, None),
                ("WORKSHOP_HOME", Some("/tmp/wh-voice-test")),
            ],
            || {
                let d = default_dir();
                assert!(d.ends_with("voice"), "{d:?}");
                assert!(!d.to_string_lossy().contains(".grok"));
            },
        );
    }

    #[test]
    fn status_reports_missing_wrong_size_and_bad_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::for_tier(dir.path(), "base").unwrap();
        assert_eq!(store.status(), ModelStatus::Missing);
        std::fs::write(store.path(), b"nope").unwrap();
        assert_eq!(store.status(), ModelStatus::WrongSize { actual: 4 });
        // Right size, wrong bytes: a sparse file of the pinned length.
        let f = std::fs::File::create(store.path()).unwrap();
        f.set_len(store.pin().size).unwrap();
        drop(f);
        assert!(matches!(store.status(), ModelStatus::BadChecksum { .. }));
        assert_eq!(
            store.quick_status(),
            ModelStatus::Ready,
            "quick status is size-only"
        );
        store.remove_bad_model();
        assert_eq!(store.status(), ModelStatus::Missing);
    }

    #[test]
    fn sha256_of_known_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("abc");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
