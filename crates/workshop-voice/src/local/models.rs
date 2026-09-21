//! Whisper model catalog, on-disk layout (`$WORKSHOP_HOME/models`), and first-use download.
//!
//! Files are the ggml conversions published by the whisper.cpp project on Hugging Face.
//! Every catalog entry pins a SHA-256; a download that does not match is deleted and reported,
//! never loaded. Nothing here runs unless the user (or the pager, on their behalf) asks for a
//! model — there is no telemetry and no background network activity.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::backend::VoiceError;

/// Hugging Face repo that hosts the ggml files (`resolve/main/<file>`).
const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// `$WORKSHOP_VOICE_MODELS_DIR` beats `$WORKSHOP_HOME/models` beats `~/.workshop/models`.
pub const MODELS_DIR_ENV: &str = "WORKSHOP_VOICE_MODELS_DIR";
pub const WORKSHOP_HOME_ENV: &str = "WORKSHOP_HOME";

/// The models Workshop offers. Sizes are the ggml file sizes on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperModel {
    /// 75 MB, English only. Fastest; acceptable for short commands, weak on names.
    TinyEn,
    /// 142 MB, English only. Recommended CPU default for dictation.
    BaseEn,
    /// 466 MB, English only. Noticeably better punctuation/names; ~3× base cost.
    SmallEn,
    /// 547 MB, multilingual, 5-bit quantized large-v3-turbo (4 decoder layers). Best accuracy
    /// Workshop ships; needs Metal/GPU or a fast desktop CPU for interactive use.
    LargeV3TurboQ5,
}

impl WhisperModel {
    pub const ALL: &'static [WhisperModel] = &[
        WhisperModel::TinyEn,
        WhisperModel::BaseEn,
        WhisperModel::SmallEn,
        WhisperModel::LargeV3TurboQ5,
    ];

    /// Config / picker id.
    pub fn id(self) -> &'static str {
        match self {
            WhisperModel::TinyEn => "tiny.en",
            WhisperModel::BaseEn => "base.en",
            WhisperModel::SmallEn => "small.en",
            WhisperModel::LargeV3TurboQ5 => "large-v3-turbo-q5_0",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        let id = id.trim();
        Self::ALL.iter().copied().find(|m| m.id() == id)
    }

    pub fn file_name(self) -> String {
        format!("ggml-{}.bin", self.id())
    }

    pub fn url(self) -> String {
        format!("{HF_BASE}/{}", self.file_name())
    }

    /// Exact file size in bytes (also used as a cheap corruption check before hashing).
    pub fn size_bytes(self) -> u64 {
        match self {
            WhisperModel::TinyEn => 77_704_715,
            WhisperModel::BaseEn => 147_964_211,
            WhisperModel::SmallEn => 487_614_201,
            WhisperModel::LargeV3TurboQ5 => 574_041_195,
        }
    }

    /// Hex SHA-256 of the published file (recorded from the download used for the prototype
    /// measurements; see `internal/voice-stt-repoint-spec.md`).
    pub fn sha256(self) -> &'static str {
        match self {
            WhisperModel::TinyEn => {
                "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"
            }
            WhisperModel::BaseEn => {
                "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
            }
            WhisperModel::SmallEn => {
                "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"
            }
            WhisperModel::LargeV3TurboQ5 => {
                "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"
            }
        }
    }

    /// English-only checkpoints must be decoded with `language = "en"`.
    pub fn english_only(self) -> bool {
        matches!(
            self,
            WhisperModel::TinyEn | WhisperModel::BaseEn | WhisperModel::SmallEn
        )
    }

    pub fn path_in(self, models_dir: &Path) -> PathBuf {
        models_dir.join(self.file_name())
    }

    /// Present on disk with the right size. (The hash is verified at download time; a size check
    /// here keeps startup cheap while still catching truncated files.)
    pub fn is_present_in(self, models_dir: &Path) -> bool {
        std::fs::metadata(self.path_in(models_dir))
            .map(|m| m.is_file() && m.len() == self.size_bytes())
            .unwrap_or(false)
    }
}

/// Workshop's model directory. Never `~/.grok`.
pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(MODELS_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os(WORKSHOP_HOME_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join("models");
    }
    xai_dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".workshop")
        .join("models")
}

/// Models already on disk, largest last.
pub fn present_models(models_dir: &Path) -> Vec<WhisperModel> {
    WhisperModel::ALL
        .iter()
        .copied()
        .filter(|m| m.is_present_in(models_dir))
        .collect()
}

/// Download progress callback: `(bytes_so_far, total_bytes)`.
pub type Progress = dyn Fn(u64, u64) + Send + Sync;

/// Ensure `model` is on disk under `models_dir`, downloading and verifying it if needed.
///
/// Writes to `<file>.part` and renames on success so a crash never leaves a half model at the
/// final path. Returns the final path.
pub async fn ensure_model(
    model: WhisperModel,
    models_dir: &Path,
    progress: Option<&Progress>,
) -> Result<PathBuf, VoiceError> {
    let final_path = model.path_in(models_dir);
    if model.is_present_in(models_dir) {
        return Ok(final_path);
    }
    tokio::fs::create_dir_all(models_dir).await.map_err(|e| {
        VoiceError::Config(format!("create models dir {}: {e}", models_dir.display()))
    })?;

    let part_path = models_dir.join(format!("{}.part", model.file_name()));
    let url = model.url();
    tracing::info!(model = model.id(), %url, "downloading whisper model");

    // Workspace TLS policy (webpki roots + GROK_EXTRA_CA_BUNDLE) via the sanctioned builder
    let client = xai_grok_extra_ca::build_reqwest_client(|b| {
        b.connect_timeout(Duration::from_secs(20))
            .user_agent("workshop-voice/0.1")
    })
    .map_err(|e| VoiceError::Config(format!("http client: {e}")))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| VoiceError::Config(format!("download {}: {e}", model.file_name())))?;
    let total = resp.content_length().unwrap_or(model.size_bytes());

    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| VoiceError::Config(format!("create {}: {e}", part_path.display())))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| VoiceError::Config(format!("download {}: {e}", model.file_name())))?
    {
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| VoiceError::Config(format!("write {}: {e}", part_path.display())))?;
        received += chunk.len() as u64;
        if let Some(cb) = progress {
            cb(received, total);
        }
    }
    file.flush()
        .await
        .map_err(|e| VoiceError::Config(format!("flush {}: {e}", part_path.display())))?;
    drop(file);

    let digest = hex(&hasher.finalize());
    if received != model.size_bytes() || digest != model.sha256() {
        let _ = tokio::fs::remove_file(&part_path).await;
        return Err(VoiceError::Config(format!(
            "downloaded {} failed verification (size {received}, sha256 {digest}); \
             expected size {} sha256 {}. The file was deleted; retry or check your network.",
            model.file_name(),
            model.size_bytes(),
            model.sha256()
        )));
    }
    tokio::fs::rename(&part_path, &final_path)
        .await
        .map_err(|e| VoiceError::Config(format!("rename to {}: {e}", final_path.display())))?;
    tracing::info!(model = model.id(), path = %final_path.display(), "whisper model ready");
    Ok(final_path)
}

/// SHA-256 of an existing file (for `workshop voice doctor` and the catalog test).
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
    fn ids_round_trip_and_files_are_ggml() {
        for m in WhisperModel::ALL {
            assert_eq!(WhisperModel::parse(m.id()), Some(*m));
            assert!(m.file_name().starts_with("ggml-"));
            assert!(m.url().starts_with(HF_BASE));
            assert_eq!(m.sha256().len(), 64);
        }
        assert_eq!(WhisperModel::parse(" base.en "), Some(WhisperModel::BaseEn));
        assert_eq!(WhisperModel::parse("large"), None);
    }

    #[test]
    fn models_dir_prefers_explicit_env_then_workshop_home() {
        // Serialize env mutation with the other tests in this module via a lock.
        let _g = ENV_LOCK.lock().unwrap();
        temp_env(
            &[
                (MODELS_DIR_ENV, Some("/tmp/x-models")),
                (WORKSHOP_HOME_ENV, Some("/tmp/wh")),
            ],
            || {
                assert_eq!(models_dir(), PathBuf::from("/tmp/x-models"));
            },
        );
        temp_env(
            &[(MODELS_DIR_ENV, None), (WORKSHOP_HOME_ENV, Some("/tmp/wh"))],
            || {
                assert_eq!(models_dir(), PathBuf::from("/tmp/wh/models"));
            },
        );
        temp_env(&[(MODELS_DIR_ENV, None), (WORKSHOP_HOME_ENV, None)], || {
            let dir = models_dir();
            assert!(dir.ends_with(".workshop/models"), "{dir:?}");
            assert!(!dir.to_string_lossy().contains(".grok"));
        });
    }

    #[test]
    fn presence_requires_exact_size() {
        let dir = std::env::temp_dir().join(format!("wv-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = WhisperModel::TinyEn.path_in(&dir);
        std::fs::write(&p, b"not a model").unwrap();
        assert!(!WhisperModel::TinyEn.is_present_in(&dir));
        assert!(present_models(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            // SAFETY: tests in this module hold ENV_LOCK; no other thread reads these vars concurrently.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        f();
        for (k, v) in saved {
            unsafe {
                match v {
                    Some(v) => std::env::set_var(&k, v),
                    None => std::env::remove_var(&k),
                }
            }
        }
    }
}
