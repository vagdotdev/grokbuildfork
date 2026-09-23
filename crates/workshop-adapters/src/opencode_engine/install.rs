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

use crate::adapter::{Adapter, PinStatus};
use crate::detect::{DetectOptions, Detection, InstalledCli, detect, verify_binary};
use crate::probe::run_probe_until;
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

/// Byte-progress callback for the installer: called with the size of the archive the vendor
/// script is downloading (into `$TMPDIR`, which Workshop points under the installer's `HOME`),
/// every [`PROGRESS_POLL`] while it grows.
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

/// An install whose files have not grown for this long is stopped: a dead or captive network,
/// not a slow one. Slow links keep going as long as bytes arrive.
pub const INSTALL_STALL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct InstallOptions {
    /// Exact version to install; defaults to the adapter's tested pin.
    pub version: String,
    pub target: InstallTarget,
    /// Ceiling for the whole install, however steadily bytes arrive.
    pub timeout: Duration,
    /// Stop when the installer's files stop growing for this long ([`INSTALL_STALL`]).
    pub stall: Duration,
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
            timeout: Duration::from_secs(1800),
            stall: INSTALL_STALL,
            env: None,
            progress: None,
        }
    }
}

/// Sizes under `root`: every regular file (what the stall watchdog watches) and the vendor
/// archive being downloaded (`opencode-<target>.zip` / `.tar.gz`, what the user is shown), 0 when
/// unreadable.
fn install_bytes(root: &Path) -> (u64, u64) {
    let (mut total, mut archive) = (0, 0);
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
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("opencode-")
                    && (name.ends_with(".zip") || name.ends_with(".tar.gz"))
                {
                    archive += meta.len();
                }
            }
        }
    }
    (total, archive)
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
    #[error("the download stopped: nothing arrived for {}s", .0.as_secs())]
    Stalled(Duration),
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
pub async fn install_opencode(opts: &InstallOptions) -> Result<InstalledCli, InstallError> {
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
    // The script downloads into `${TMPDIR:-/tmp}/opencode_install_$$`; pointing TMPDIR under its
    // HOME makes that download measurable (progress, stall watchdog) and the final `mv` a rename.
    let tmp = home.join(".tmp");
    let _ = std::fs::create_dir_all(&tmp);
    env.insert(OsString::from("TMPDIR"), tmp.clone().into_os_string());
    let watchdog = {
        let (root, progress, stall) = (home.clone(), opts.progress.clone(), opts.stall);
        async move {
            let (mut last_total, mut last_archive) = (u64::MAX, 0u64);
            let mut grew_at = tokio::time::Instant::now();
            let mut ticks = tokio::time::interval(PROGRESS_POLL);
            loop {
                ticks.tick().await;
                let (total, archive) = install_bytes(&root);
                if total != last_total {
                    last_total = total;
                    grew_at = tokio::time::Instant::now();
                }
                if archive != last_archive {
                    last_archive = archive;
                    if let Some(progress) = &progress {
                        (progress.0)(archive);
                    }
                }
                if grew_at.elapsed() >= stall {
                    return format!("nothing arrived for {}s", stall.as_secs());
                }
            }
        }
    };
    let output = run_probe_until(
        &bash,
        &["-c", &script],
        &env,
        Some(&home),
        opts.timeout,
        watchdog,
    )
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    let output = output.map_err(|e| match e {
        crate::probe::ProbeError::Timeout { timeout, .. } => InstallError::Timeout(timeout),
        crate::probe::ProbeError::Stopped { .. } => InstallError::Stalled(opts.stall),
        other => InstallError::InstallerFailed {
            exit_code: None,
            stderr: other.to_string(),
        },
    })?;
    if !output.success() {
        return Err(InstallError::InstallerFailed {
            exit_code: output.exit_code,
            stderr: crate::probe::strip_ansi(&output.stderr)
                .chars()
                .take(800)
                .collect(),
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
        let combined = crate::probe::strip_ansi(&format!("{}\n{}", output.stdout, output.stderr));
        let tail: String = combined
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("no output")
            .chars()
            .take(400)
            .collect();
        return Err(InstallError::InstallerFailed {
            exit_code: output.exit_code,
            stderr: format!("finished without writing `{}`: {tail}", path.display()),
        });
    }
    // macOS: a binary carrying `com.apple.quarantine` is refused by Gatekeeper when a
    // non-Terminal parent spawns it (SIGKILL, no stderr). `curl | bash` + `unzip` normally leaves
    // no attribute, but some configurations do; strip it before the first spawn the way
    // scripts/install.sh strips it for `workshop` itself. Idempotent, no-op off macOS.
    super::state::clear_quarantine(&path);
    let cli = verify_binary(
        &OpenCodeAdapter,
        &path,
        &verify_env,
        Duration::from_secs(60),
    )
    .await
    .map_err(|reason| InstallError::Unverified {
        path: path.clone(),
        reason,
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

/// Detect `opencode` without installing: PATH, the vendor's known dirs, and the Workshop tools
/// tree (a previous auto-install is found first). `install` only names the target dir to search.
///
/// A copy of the user's own that Workshop cannot use (older than supported — a Homebrew formula
/// a few releases behind — or one that fails to run, like a quarantined download) never blocks
/// the first answer: the Workshop-managed copy is used instead, and `NotInstalled` tells the
/// caller to install it when there is none yet.
pub async fn detect_opencode(
    detect_opts: &DetectOptions,
    install: Option<&InstallOptions>,
) -> Detection {
    let managed_home = install
        .and_then(|i| i.target.installer_home())
        .unwrap_or_else(|| workshop_tools_dir().join("opencode"));
    let managed_bin = InstallTarget::Home(managed_home.clone())
        .bin_dir()
        .expect("a Home target always has a bin dir");
    let mut opts = detect_opts.clone();
    let mut known = opts
        .known_dirs
        .clone()
        .unwrap_or_else(|| crate::detect::default_known_dirs(opts.home_dir().as_deref()));
    known.insert(0, managed_bin.clone());
    opts.known_dirs = Some(known);
    let found = detect(&OpenCodeAdapter, &opts).await;
    match &found {
        Detection::Installed(cli) if cli.pin != PinStatus::OlderThanSupported => return found,
        // Our own copy predates the current pin (Workshop was updated): reinstall the pinned one.
        Detection::Installed(cli) if cli.path.starts_with(&managed_home) => {
            return Detection::NotInstalled;
        }
        // Our own copy does not run: the caller repairs it.
        Detection::Unverified { path, .. } if path.starts_with(&managed_home) => return found,
        Detection::NotInstalled => return found,
        _ => {}
    }
    let managed_only = DetectOptions {
        path_env: Some(OsString::new()),
        home: Some(managed_home),
        known_dirs: Some(vec![managed_bin]),
        ..detect_opts.clone()
    };
    match detect(&OpenCodeAdapter, &managed_only).await {
        Detection::Installed(cli) if cli.pin != PinStatus::OlderThanSupported => {
            Detection::Installed(cli)
        }
        Detection::Unverified { path, reason } => Detection::Unverified { path, reason },
        _ => Detection::NotInstalled,
    }
}

/// Detect `opencode`, optionally installing it when absent. Also searches the
/// Workshop tools tree so a previous auto-install is found first.
pub async fn ensure_opencode(
    detect_opts: &DetectOptions,
    install: Option<&InstallOptions>,
) -> Result<InstalledCli, InstallError> {
    match detect_opencode(detect_opts, install).await {
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

    #[cfg(unix)]
    fn write_exec(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A stand-in `opencode` that identifies like the real CLI at `version`.
    #[cfg(unix)]
    fn fake_opencode(path: &Path, version: &str) {
        write_exec(
            path,
            &format!(
                "#!/bin/sh\ncase \"$1\" in --version) echo {version};; *) echo 'opencode run [message..]' >&2;; esac\n"
            ),
        );
    }

    #[cfg(unix)]
    struct Layout {
        _tmp: tempfile::TempDir,
        user_bin: PathBuf,
        managed: PathBuf,
        detect: DetectOptions,
    }

    #[cfg(unix)]
    fn layout() -> Layout {
        let tmp = tempfile::tempdir().unwrap();
        let user_bin = tmp.path().join("user-bin");
        std::fs::create_dir_all(&user_bin).unwrap();
        let managed = tmp.path().join("workshop/tools/opencode");
        let env: BTreeMap<OsString, OsString> =
            [(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))].into();
        let detect = DetectOptions {
            path_env: Some(user_bin.clone().into_os_string()),
            home: Some(tmp.path().join("home")),
            known_dirs: Some(vec![]),
            probe_env: Some(env),
            ..DetectOptions::default()
        };
        Layout {
            _tmp: tmp,
            user_bin,
            managed,
            detect,
        }
    }

    #[cfg(unix)]
    fn managed_target(l: &Layout) -> InstallOptions {
        InstallOptions {
            target: InstallTarget::Home(l.managed.clone()),
            ..InstallOptions::default()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_user_copy_on_path_yields_to_the_managed_copy() {
        let l = layout();
        fake_opencode(&l.user_bin.join("opencode"), "1.17.0");
        fake_opencode(&l.managed.join(".opencode/bin/opencode"), "1.18.31");
        let install = managed_target(&l);
        match detect_opencode(&l.detect, Some(&install)).await {
            Detection::Installed(cli) => {
                assert!(cli.path.starts_with(&l.managed), "{}", cli.path.display());
                assert_eq!(cli.version, "1.18.31");
            }
            other => panic!("expected the managed copy, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stale_user_copy_without_a_managed_one_asks_for_the_install() {
        let l = layout();
        fake_opencode(&l.user_bin.join("opencode"), "1.17.0");
        let install = managed_target(&l);
        assert_eq!(
            detect_opencode(&l.detect, Some(&install)).await,
            Detection::NotInstalled
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_user_copy_that_does_not_run_yields_to_an_install() {
        let l = layout();
        write_exec(&l.user_bin.join("opencode"), "#!/bin/sh\nkill -9 $$\n");
        let install = managed_target(&l);
        assert_eq!(
            detect_opencode(&l.detect, Some(&install)).await,
            Detection::NotInstalled
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_outdated_managed_copy_is_reinstalled_and_a_newer_user_copy_is_kept() {
        let l = layout();
        fake_opencode(&l.managed.join(".opencode/bin/opencode"), "1.17.0");
        let install = managed_target(&l);
        assert_eq!(
            detect_opencode(&l.detect, Some(&install)).await,
            Detection::NotInstalled
        );
        fake_opencode(&l.user_bin.join("opencode"), "1.19.0");
        match detect_opencode(&l.detect, Some(&install)).await {
            Detection::Installed(cli) => assert!(cli.path.starts_with(&l.user_bin)),
            other => panic!("a newer user copy is used as is, got {other:?}"),
        }
    }

    /// `curl` that prints `script` whatever it is asked for, so `curl … | bash` runs `script`.
    #[cfg(unix)]
    fn installer_env(dir: &Path, script: &str) -> BTreeMap<OsString, OsString> {
        let bin = dir.join("fake-bin");
        write_exec(
            &bin.join("curl"),
            &format!("#!/bin/sh\ncat <<'SCRIPT'\n{script}\nSCRIPT\n"),
        );
        let path = format!("{}:/usr/bin:/bin", bin.display());
        [(OsString::from("PATH"), OsString::from(path))].into()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_download_is_measured_under_the_installer_home_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("payload/opencode");
        fake_opencode(&fake, "1.18.31");
        let script = format!(
            "d=\"${{TMPDIR:-/tmp}}/opencode_install_$$\"; mkdir -p \"$d\"\n\
             case \"$TMPDIR\" in \"$HOME\"/*) touch \"$HOME/tmpdir-under-home\";; esac\n\
             for i in 1 2 3 4; do head -c 200000 /dev/zero >> \"$d/opencode-linux-x64.tar.gz\"; sleep 0.7; done\n\
             mkdir -p \"$HOME/.opencode/bin\"; cp {} \"$HOME/.opencode/bin/opencode\"; rm -rf \"$d\"",
            fake.display()
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let home = tmp.path().join("tools/opencode");
        let opts = InstallOptions {
            target: InstallTarget::Home(home.clone()),
            env: Some(installer_env(tmp.path(), &script)),
            progress: Some(InstallProgress::new({
                let seen = seen.clone();
                move |b| seen.lock().unwrap().push(b)
            })),
            ..InstallOptions::default()
        };
        let cli = install_opencode(&opts)
            .await
            .expect("fake install succeeds");
        assert!(cli.path.starts_with(&home));
        assert!(
            home.join("tmpdir-under-home").exists(),
            "the vendor script downloads under the installer HOME, not /tmp"
        );
        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.len() >= 3
                && seen.windows(2).all(|w| w[0] < w[1])
                && seen[seen.len() - 1] >= 600_000,
            "progress climbs while the archive grows: {seen:?}"
        );
        assert!(
            !home.join(".tmp").exists(),
            "the download dir is cleaned up"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_download_that_stops_arriving_is_stopped_not_waited_on_for_the_ceiling() {
        let tmp = tempfile::tempdir().unwrap();
        let script = "d=\"${TMPDIR:-/tmp}/opencode_install_$$\"; mkdir -p \"$d\"\n\
                      head -c 1000 /dev/zero > \"$d/opencode-linux-x64.tar.gz\"; sleep 300";
        let opts = InstallOptions {
            target: InstallTarget::Home(tmp.path().join("tools/opencode")),
            env: Some(installer_env(tmp.path(), script)),
            stall: Duration::from_secs(2),
            ..InstallOptions::default()
        };
        let started = std::time::Instant::now();
        let err = install_opencode(&opts).await.unwrap_err();
        assert!(matches!(err, InstallError::Stalled(_)), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            err.to_string(),
            "the download stopped: nothing arrived for 2s"
        );
    }
}
