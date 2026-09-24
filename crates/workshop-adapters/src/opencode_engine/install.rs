//! Auto-provision `opencode` with the vendor's own installer.
//!
//! Workshop never bundles or modifies OpenCode. When the CLI is missing and
//! the user agrees, the official script (`https://opencode.ai/install`) is run
//! with a pinned `--version` and `--no-modify-path`, then the resulting binary
//! must identify itself and report exactly the requested version before it is
//! used. The script honours `$HOME/.opencode/bin` as its install dir, so a
//! Workshop-owned location is chosen by pointing `HOME` at
//! `$WORKSHOP_HOME/tools/opencode` for the installer process only.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use workshop_detect::process::strip_ansi;
use workshop_detect::{DetectConfig, Detection, Identity, Vendor};

use crate::adapter::Adapter;
use crate::vendors::OpenCodeAdapter;

pub const OFFICIAL_INSTALLER_URL: &str = "https://opencode.ai/install";

/// Where the installer puts the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallTarget {
    /// The vendor default, `~/.opencode/bin/opencode`, on the user's real `HOME`.
    VendorDefault,
    /// A Workshop-owned tree: `<dir>/.opencode/bin/opencode` (the installer
    /// runs with `HOME=<dir>`). Use [`workshop_tools_dir`] for the default.
    Home(PathBuf),
}

/// Byte-progress callback for the installer: called with the number of bytes the installer has
/// written under its `HOME` so far (the vendor script downloads the archive there before it
/// unpacks it), every [`PROGRESS_POLL`] while the download grows.
#[derive(Clone)]
pub struct InstallProgress(std::sync::Arc<dyn Fn(u64) + Send + Sync>);

impl InstallProgress {
    pub fn new(f: impl Fn(u64) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(f))
    }
}

impl std::fmt::Debug for InstallProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InstallProgress(..)")
    }
}

/// How often the installer's download directory is measured for [`InstallProgress`].
pub const PROGRESS_POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// Exact version to install; defaults to the adapter's tested pin.
    pub version: String,
    pub target: InstallTarget,
    pub timeout: Duration,
    /// Environment for the installer (`PATH`, proxies); `None` = minimal env
    /// from this process.
    pub env: Option<BTreeMap<OsString, OsString>>,
    /// Download progress in bytes, reported while the vendor script runs; `None` = silent.
    pub progress: Option<InstallProgress>,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            version: OpenCodeAdapter.version_pin().max_tested.to_string(),
            target: InstallTarget::Home(workshop_tools_dir().join("opencode")),
            timeout: Duration::from_secs(300),
            env: None,
            progress: None,
        }
    }
}

/// Total size of the regular files under `root` (the installer's `HOME`), 0 when unreadable.
pub fn tree_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

/// Human-readable byte count for a progress line: `12.3 MB`, `840 KB`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{} KB", bytes / 1_000)
    } else {
        format!("{bytes} B")
    }
}

impl InstallTarget {
    /// The installer's `$HOME` for this target.
    fn installer_home(&self) -> Option<PathBuf> {
        match self {
            InstallTarget::VendorDefault => std::env::var_os("HOME").map(PathBuf::from),
            InstallTarget::Home(dir) => Some(dir.clone()),
        }
    }

    /// Directory the installer writes `opencode` into.
    pub fn bin_dir(&self) -> Option<PathBuf> {
        self.installer_home()
            .map(|h| h.join(".opencode").join("bin"))
    }
}

/// `$WORKSHOP_HOME/tools`, defaulting to `~/.workshop/tools`.
pub fn workshop_tools_dir() -> PathBuf {
    let home = std::env::var_os("WORKSHOP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".workshop")))
        .unwrap_or_else(|| PathBuf::from(".workshop"));
    home.join("tools")
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("installer needs `bash` and `curl` on PATH")]
    MissingTools,
    #[error("no HOME available to place the install")]
    NoHome,
    #[error("installer failed{}: {stderr}", match exit_code { Some(c) => format!(" (exit {c})"), None => String::new() })]
    InstallerFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("installer did not exit within {0:?}")]
    Timeout(Duration),
    #[error("installed binary at `{path}` failed verification: {reason}")]
    Unverified { path: PathBuf, reason: String },
    #[error("installed `{path}` reports version {actual}, expected {expected}")]
    VersionMismatch {
        path: PathBuf,
        actual: String,
        expected: String,
    },
    #[error(transparent)]
    Env(#[from] crate::env::DeniedEnvVar),
    #[error("a binary named opencode exists at `{path}` but is not OpenCode: {reason}")]
    Impostor { path: PathBuf, reason: String },
}

fn which(env: &BTreeMap<OsString, OsString>, name: &str) -> Option<PathBuf> {
    let path = env.get(&OsString::from("PATH"))?;
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// `PATH` without any directory that already holds `binary`. The vendor
/// installer exits early ("already installed") when it finds a same-version
/// `opencode` on `PATH`, which would defeat a Workshop-owned copy.
fn path_without(path: &OsString, binary: &str) -> OsString {
    let kept = std::env::split_paths(path).filter(|d| !d.join(binary).is_file());
    std::env::join_paths(kept).unwrap_or_else(|_| path.clone())
}

/// Run the official installer and verify the result.
pub async fn install_opencode(opts: &InstallOptions) -> Result<Identity, InstallError> {
    let mut env = match &opts.env {
        Some(env) => crate::env::minimal_env(env.clone(), &[])?,
        None => crate::env::minimal_env_from_process(&[])?,
    };
    let bash = which(&env, "bash").ok_or(InstallError::MissingTools)?;
    which(&env, "curl").ok_or(InstallError::MissingTools)?;
    let home = opts.target.installer_home().ok_or(InstallError::NoHome)?;
    std::fs::create_dir_all(&home).map_err(|e| InstallError::InstallerFailed {
        exit_code: None,
        stderr: format!("create {}: {e}", home.display()),
    })?;
    env.insert(OsString::from("HOME"), home.clone().into_os_string());
    if matches!(opts.target, InstallTarget::Home(_))
        && let Some(path) = env.get(&OsString::from("PATH")).cloned()
    {
        env.insert(OsString::from("PATH"), path_without(&path, "opencode"));
    }

    let version = opts.version.trim_start_matches('v').to_string();
    // The vendor's documented one-liner, plus a pinned version and no shell
    // rc edits. Quoting is fixed; the version is validated as a plain
    // dotted-numeric token so it cannot smuggle shell syntax.
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        || version.is_empty()
    {
        return Err(InstallError::InstallerFailed {
            exit_code: None,
            stderr: format!("refusing to pass unusual version string `{version}` to the installer"),
        });
    }
    // `pipefail`: a curl failure (offline, DNS, proxy) must fail the pipeline instead of feeding
    // bash an empty script that "succeeds" without installing anything.
    let script = format!(
        "set -o pipefail; curl -fsSL {OFFICIAL_INSTALLER_URL} | bash -s -- --version {version} --no-modify-path"
    );
    tracing::info!(%version, home = %home.display(), "running official opencode installer");
    // Byte progress: the script writes the archive under its HOME as it downloads, so the size
    // of that tree is the honest number to show while the user waits.
    let progress_poll = opts.progress.clone().map(|progress| {
        let root = home.clone();
        tokio::spawn(async move {
            let mut last = 0u64;
            let mut ticks = tokio::time::interval(PROGRESS_POLL);
            loop {
                ticks.tick().await;
                let bytes = tree_bytes(&root);
                if bytes != last {
                    last = bytes;
                    (progress.0)(bytes);
                }
            }
        })
    });
    // The same bounded, process-grouped runner the detection stack uses for its probes.
    let installer_env: Vec<(OsString, OsString)> =
        env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let (bash_path, script_owned, home_dir, timeout) =
        (bash.clone(), script.clone(), home.clone(), opts.timeout);
    let output = tokio::task::spawn_blocking(move || {
        workshop_detect::process::run(
            &bash_path,
            &["-c", &script_owned],
            Some(&home_dir),
            &installer_env,
            timeout,
        )
    })
    .await;
    if let Some(poll) = progress_poll {
        poll.abort();
    }
    let output = match output {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(InstallError::InstallerFailed {
                exit_code: None,
                stderr: e.to_string(),
            });
        }
        Err(e) => {
            return Err(InstallError::InstallerFailed {
                exit_code: None,
                stderr: format!("installer task failed: {e}"),
            });
        }
    };
    if output.timed_out {
        return Err(InstallError::Timeout(opts.timeout));
    }
    if !output.success() {
        return Err(InstallError::InstallerFailed {
            exit_code: output.code,
            stderr: strip_ansi(&output.stderr).chars().take(800).collect(),
        });
    }

    // Verify with the caller's env (not the installer HOME): the binary must
    // stand on its own.
    let verify_env = match &opts.env {
        Some(env) => crate::env::minimal_env(env.clone(), &[])?,
        None => crate::env::minimal_env_from_process(&[])?,
    };
    let mut path = home.join(".opencode").join("bin").join("opencode");
    if !path.is_file() && output.stdout.contains("already installed") {
        // Vendor-default target and the user's PATH already has this exact
        // version: the installer deliberately wrote nothing. Use that copy.
        path = which(&verify_env, "opencode").ok_or_else(|| InstallError::Unverified {
            path: path.clone(),
            reason: "installer reported an existing install but none is on PATH".to_string(),
        })?;
    }
    if !path.is_file() {
        let combined = strip_ansi(&format!("{}\n{}", output.stdout, output.stderr));
        let tail: String = combined
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("no output")
            .chars()
            .take(400)
            .collect();
        return Err(InstallError::InstallerFailed {
            exit_code: output.code,
            stderr: format!("finished without writing `{}`: {tail}", path.display()),
        });
    }
    // macOS: a binary carrying `com.apple.quarantine` is refused by Gatekeeper when a
    // non-Terminal parent spawns it (SIGKILL, no stderr). `curl | bash` + `unzip` normally leaves
    // no attribute, but some configurations do; strip it before the first spawn the way
    // scripts/install.sh strips it for `workshop` itself. Idempotent, no-op off macOS.
    super::state::clear_quarantine(&path);
    // The detection stack's own identity check, with the caller's locations on top of the
    // minimal environment.
    let identify_cfg = DetectConfig {
        timeout: Duration::from_secs(60),
        extra_env: verify_env.into_iter().collect(),
        ..DetectConfig::default()
    };
    let verify_path = path.clone();
    let cli = tokio::task::spawn_blocking(move || {
        workshop_detect::identify(Vendor::OpenCode, &verify_path, &identify_cfg)
    })
    .await
    .map_err(|e| InstallError::Unverified {
        path: path.clone(),
        reason: format!("verification task failed: {e}"),
    })?
    .map_err(|e| InstallError::Unverified {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    if cli.version != version {
        return Err(InstallError::VersionMismatch {
            path,
            actual: cli.version,
            expected: version,
        });
    }
    Ok(cli)
}

/// Detect `opencode` without installing: PATH, the Workshop tools tree (a previous auto-install
/// is found before any other copy), then the vendor's known dirs — through the detection stack
/// the picker uses. `install` only names the target dir to search.
pub async fn detect_opencode(
    detect_cfg: &DetectConfig,
    install: Option<&InstallOptions>,
) -> Detection {
    let mut cfg = detect_cfg.clone();
    let tools_bin = match install {
        Some(InstallOptions { target, .. }) => target.bin_dir(),
        None => InstallTarget::Home(workshop_tools_dir().join("opencode")).bin_dir(),
    };
    if let Some(bin) = tools_bin {
        cfg.preferred_dirs.insert(0, bin);
    }
    crate::detect(&OpenCodeAdapter, &cfg).await
}

/// Detect `opencode`, optionally installing it when absent. Also searches the
/// Workshop tools tree so a previous auto-install is found first.
pub async fn ensure_opencode(
    detect_cfg: &DetectConfig,
    install: Option<&InstallOptions>,
) -> Result<Identity, InstallError> {
    match detect_opencode(detect_cfg, install).await {
        Detection::Installed(cli) => Ok(cli),
        Detection::Unverified { path, reason } => Err(InstallError::Impostor { path, reason }),
        Detection::NotInstalled => match install {
            Some(install) => install_opencode(install).await,
            None => Err(InstallError::InstallerFailed {
                exit_code: None,
                stderr: "opencode is not installed and auto-install was not offered".to_string(),
            }),
        },
    }
}

/// True when `path` looks like a Workshop-owned auto-install.
pub fn is_workshop_managed(path: &Path) -> bool {
    path.starts_with(workshop_tools_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_without_drops_only_dirs_holding_the_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let with = tmp.path().join("with");
        let without = tmp.path().join("without");
        std::fs::create_dir_all(&with).unwrap();
        std::fs::create_dir_all(&without).unwrap();
        std::fs::write(with.join("opencode"), "").unwrap();
        let path = std::env::join_paths([&with, &without]).unwrap();
        let filtered = path_without(&path, "opencode");
        let dirs: Vec<PathBuf> = std::env::split_paths(&filtered).collect();
        assert_eq!(dirs, vec![without]);
    }

    #[test]
    fn workshop_tools_dir_prefers_workshop_home() {
        // Only checks the shape; the env var itself is process-global.
        let dir = workshop_tools_dir();
        assert!(dir.ends_with("tools"));
    }
}
