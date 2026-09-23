//! The `voice-engine` helper, fetched by Workshop itself: a default install ships `workshop` alone,
//! so the first thing the background voice setup does is put the helper in place — the release's
//! `SHA256SUMS` names the archive for this platform, the archive comes from the release mirror,
//! is verified against that digest, the binary inside lands in `<home>/downloads/` and is linked as
//! `<home>/bin/voice-engine` (the installer's own layout, which [`crate::engine::locate_engine`]
//! finds). On macOS the quarantine attribute is cleared, as the installer does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::engine::{self, ENGINE_BIN};
use crate::store::Progress;
use crate::{Error, manifest};

/// `SHA256SUMS` lists `voice-engine-<version>-<platform>.tar.gz` per platform.
const ASSET_PREFIX: &str = "voice-engine-";
const ASSET_SUFFIX: &str = ".tar.gz";

/// The release asset platform of this build (`macos-aarch64`, `linux-x86_64`, …), or `None` where
/// no helper is published.
pub fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

/// The `(sha256, asset name)` of this platform's helper archive in a `SHA256SUMS` listing.
pub fn helper_asset(sums: &str, platform: &str) -> Option<(String, String)> {
    let suffix = format!("-{platform}{ASSET_SUFFIX}");
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let sha = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (sha.len() == 64
            && sha.chars().all(|c| c.is_ascii_hexdigit())
            && name.starts_with(ASSET_PREFIX)
            && name.ends_with(&suffix))
        .then(|| (sha.to_ascii_lowercase(), name.to_owned()))
    })
}

/// The helper for this machine: the one already installed, else fetched from the release mirror.
/// `progress` reports the archive's bytes; the returned path is `<home>/bin/voice-engine`.
pub async fn ensure_helper(
    home: &Path,
    progress: &crate::store::ProgressFn<'_>,
) -> Result<PathBuf, Error> {
    if let Some(found) = engine::locate_engine() {
        return Ok(found);
    }
    let base = manifest::release_mirror_base().ok_or_else(|| {
        Error::Config("this build has no release to fetch the voice helper from".into())
    })?;
    let platform = platform()
        .ok_or_else(|| Error::Config("no voice helper is published for this platform".into()))?;
    let client = xai_grok_extra_ca::build_reqwest_client(|b| {
        b.connect_timeout(Duration::from_secs(20))
            .user_agent(concat!("workshop-voice/", env!("CARGO_PKG_VERSION")))
    })
    .map_err(|e| Error::Download(format!("http client: {e}")))?;

    let sums_url = format!("{base}/SHA256SUMS");
    let sums = client
        .get(&sums_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| Error::Download(format!("{sums_url}: {e}")))?
        .text()
        .await
        .map_err(|e| Error::Download(format!("{sums_url}: {e}")))?;
    let (expected, asset) = helper_asset(&sums, platform).ok_or_else(|| {
        Error::Download(format!(
            "the release lists no voice helper for {platform} in SHA256SUMS"
        ))
    })?;

    let downloads = home.join("downloads");
    let bindir = home.join("bin");
    for dir in [&downloads, &bindir] {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| Error::Download(format!("create {}: {e}", dir.display())))?;
    }
    let archive = downloads.join(format!("{asset}.tmp"));
    let asset_url = format!("{base}/{asset}");
    let digest = download_to(&client, &asset_url, &archive, progress).await?;
    if digest != expected {
        let _ = tokio::fs::remove_file(&archive).await;
        return Err(Error::Download(format!(
            "checksum mismatch for {asset}: got {digest}, SHA256SUMS lists {expected}"
        )));
    }

    // `voice-engine-<version>-<platform>` beside the CLI's own versioned file, linked from bin/.
    let stem = asset.trim_end_matches(ASSET_SUFFIX).to_owned();
    let versioned = downloads.join(&stem);
    let extracted = tokio::task::spawn_blocking({
        let archive = archive.clone();
        let versioned = versioned.clone();
        move || extract_helper(&archive, &versioned)
    })
    .await
    .map_err(|e| Error::Download(format!("extract task: {e}")))??;
    let _ = tokio::fs::remove_file(&archive).await;
    if !extracted {
        return Err(Error::Download(format!(
            "{asset} does not contain a {ENGINE_BIN} binary"
        )));
    }
    clear_quarantine(&versioned);

    let link = bindir.join(ENGINE_BIN);
    link_helper(&versioned, &link)?;
    let version = engine::engine_version(&link)?;
    tracing::info!(path = %link.display(), %version, "voice helper installed");
    Ok(link)
}

/// Stream `url` into `dest`, reporting bytes; returns the hex SHA-256 of what was written.
async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    progress: &crate::store::ProgressFn<'_>,
) -> Result<String, Error> {
    let mut resp = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| Error::Download(format!("{url}: {e}")))?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| Error::Download(format!("create {}: {e}", dest.display())))?;
    let mut hasher = Sha256::new();
    let mut received = 0u64;
    progress(Progress {
        received,
        total,
        attempt: 1,
    });
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(60), resp.chunk())
            .await
            .map_err(|_| Error::Download("stalled for 60 s".into()))?
            .map_err(|e| Error::Download(format!("read: {e}")))?;
        let Some(chunk) = chunk else { break };
        received += chunk.len() as u64;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| Error::Download(format!("write {}: {e}", dest.display())))?;
        progress(Progress {
            received,
            total: total.max(received),
            attempt: 1,
        });
    }
    file.flush()
        .await
        .map_err(|e| Error::Download(format!("flush {}: {e}", dest.display())))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Write the archive's `voice-engine` member to `dest` (executable); `Ok(false)` when absent.
fn extract_helper(archive: &Path, dest: &Path) -> Result<bool, Error> {
    let file = std::fs::File::open(archive)
        .map_err(|e| Error::Download(format!("open {}: {e}", archive.display())))?;
    let mut tarball = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = tarball
        .entries()
        .map_err(|e| Error::Download(format!("read archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| Error::Download(format!("read archive entry: {e}")))?;
        let is_helper = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n == ENGINE_BIN))
            .unwrap_or(false);
        if !is_helper || !entry.header().entry_type().is_file() {
            continue;
        }
        let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
        {
            let mut out = std::fs::File::create(&tmp)
                .map_err(|e| Error::Download(format!("create {}: {e}", tmp.display())))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| Error::Download(format!("write {}: {e}", tmp.display())))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| Error::Download(format!("chmod {}: {e}", tmp.display())))?;
        }
        std::fs::rename(&tmp, dest)
            .map_err(|e| Error::Download(format!("rename into {}: {e}", dest.display())))?;
        return Ok(true);
    }
    Ok(false)
}

/// macOS marks downloaded files with `com.apple.quarantine`; the installer clears it, and so do
/// we — a helper that Gatekeeper blocks is no helper. Elsewhere this is a no-op.
fn clear_quarantine(path: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let mut cmd = std::process::Command::new("xattr");
    cmd.arg("-d")
        .arg("com.apple.quarantine")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    xai_tty_utils::detach_std_command(&mut cmd);
    // Short-lived; waited on here. Failure (no attribute, no xattr) is fine.
    #[allow(clippy::disallowed_methods)]
    let _ = cmd.status();
}

/// `<home>/bin/voice-engine` → `../downloads/<versioned>`, replaced atomically.
fn link_helper(versioned: &Path, link: &Path) -> Result<(), Error> {
    let target = PathBuf::from("..")
        .join("downloads")
        .join(versioned.file_name().unwrap_or_default());
    let tmp = link.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &tmp)
        .map_err(|e| Error::Download(format!("link {}: {e}", tmp.display())))?;
    #[cfg(not(unix))]
    std::fs::copy(versioned, &tmp)
        .map_err(|e| Error::Download(format!("copy {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, link)
        .map_err(|e| Error::Download(format!("rename into {}: {e}", link.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_asset_is_picked_per_platform() {
        let sums = "\
0000000000000000000000000000000000000000000000000000000000000001  workshop-0.2.2-linux-x86_64.tar.gz
0000000000000000000000000000000000000000000000000000000000000002  voice-engine-0.2.2-linux-x86_64.tar.gz
0000000000000000000000000000000000000000000000000000000000000003 *voice-engine-0.2.2-macos-aarch64.tar.gz
";
        assert_eq!(
            helper_asset(sums, "linux-x86_64"),
            Some((
                "0000000000000000000000000000000000000000000000000000000000000002".into(),
                "voice-engine-0.2.2-linux-x86_64.tar.gz".into()
            ))
        );
        assert_eq!(
            helper_asset(sums, "macos-aarch64").map(|(_, n)| n),
            Some("voice-engine-0.2.2-macos-aarch64.tar.gz".into())
        );
        assert!(helper_asset(sums, "windows-x86_64").is_none());
        assert!(helper_asset("garbage\n", "linux-x86_64").is_none());
    }

    #[test]
    fn extract_writes_the_helper_member_executable() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&archive).unwrap(),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            let body = b"#!/bin/sh\necho voice-engine 9.9\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "voice-engine", &body[..])
                .unwrap();
            let mut other = tar::Header::new_gnu();
            other.set_size(3);
            other.set_mode(0o644);
            other.set_cksum();
            tar.append_data(&mut other, "README", &b"hi\n"[..]).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
        }
        let dest = dir.path().join("voice-engine-9.9-linux-x86_64");
        assert!(extract_helper(&archive, &dest).unwrap());
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "#!/bin/sh\necho voice-engine 9.9\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&dest).unwrap().permissions().mode() & 0o111,
                0
            );
        }
        let link = dir.path().join("bin").join(ENGINE_BIN);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        link_helper(&dest, &link).unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            PathBuf::from("../downloads/voice-engine-9.9-linux-x86_64")
        );
    }
}
