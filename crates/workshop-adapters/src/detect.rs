//! Locate and positively identify vendor CLIs.
//!
//! Order: `PATH`, then shared known install dirs, then vendor-specific dirs.
//! A binary counts as installed only after its own `--version` / `--help`
//! output identifies it as that vendor's CLI. An unrelated `agent` on `PATH`
//! is therefore never mistaken for Cursor.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AdapterId, PinStatus};
use crate::probe::run_probe;

/// Controls where detection looks. Every input is overridable so tests never
/// depend on the host machine.
#[derive(Clone, Debug)]
pub struct DetectOptions {
    /// `PATH` to search; `None` uses the process `PATH`.
    pub path_env: Option<OsString>,
    /// Home directory for `~` expansion; `None` uses `HOME` / `USERPROFILE`.
    pub home: Option<PathBuf>,
    /// Shared known dirs; `None` uses [`default_known_dirs`].
    pub known_dirs: Option<Vec<PathBuf>>,
    /// Environment for identity probes; `None` uses the minimal env built from
    /// the current process.
    pub probe_env: Option<BTreeMap<OsString, OsString>>,
    pub probe_timeout: Duration,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            path_env: None,
            home: None,
            known_dirs: None,
            probe_env: None,
            probe_timeout: Duration::from_secs(15),
        }
    }
}

impl DetectOptions {
    pub fn home_dir(&self) -> Option<PathBuf> {
        self.home.clone().or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
        })
    }
}

/// A vendor CLI that passed identity verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledCli {
    pub adapter: AdapterId,
    pub path: PathBuf,
    pub version: String,
    pub pin: PinStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Detection {
    Installed(InstalledCli),
    /// A same-named executable exists but did not identify as this vendor.
    Unverified {
        path: PathBuf,
        reason: String,
    },
    NotInstalled,
}

impl Detection {
    pub fn installed(&self) -> Option<&InstalledCli> {
        match self {
            Detection::Installed(cli) => Some(cli),
            _ => None,
        }
    }
}

/// Shared install locations checked after `PATH`.
pub fn default_known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/usr/local/bin")];
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    if let Ok(entries) = std::fs::read_dir("/opt/homebrew/opt") {
        let mut formula_bins: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("bin"))
            .filter(|p| p.is_dir())
            .collect();
        formula_bins.sort();
        dirs.extend(formula_bins);
    }
    dirs
}

fn path_dirs(opts: &DetectOptions) -> Vec<PathBuf> {
    let raw = opts
        .path_env
        .clone()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    std::env::split_paths(&raw)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    let _ = path;
    None
}

/// Existing executables that could be this adapter's CLI, in search order,
/// with symlink aliases of the same file (e.g. `agent` -> `cursor-agent`)
/// collapsed to the first hit.
pub fn candidate_paths(adapter: &dyn Adapter, opts: &DetectOptions) -> Vec<PathBuf> {
    let home = opts.home_dir();
    let mut dirs = path_dirs(opts);
    dirs.extend(
        opts.known_dirs
            .clone()
            .unwrap_or_else(|| default_known_dirs(home.as_deref())),
    );
    if let Some(home) = &home {
        dirs.extend(adapter.extra_install_dirs(home));
    }

    let mut seen_paths = std::collections::HashSet::new();
    let mut seen_files = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for name in adapter.binary_names() {
            let candidate = dir.join(name);
            if !seen_paths.insert(candidate.clone()) || !is_executable_file(&candidate) {
                continue;
            }
            if let Some(id) = file_identity(&candidate)
                && !seen_files.insert(id)
            {
                continue;
            }
            out.push(candidate);
        }
    }
    out
}

/// Detect one adapter's CLI.
pub async fn detect(adapter: &dyn Adapter, opts: &DetectOptions) -> Detection {
    let env = match &opts.probe_env {
        Some(env) => env.clone(),
        None => match crate::env::minimal_env_from_process(&[]) {
            Ok(env) => env,
            Err(e) => {
                return Detection::Unverified {
                    path: PathBuf::new(),
                    reason: e.to_string(),
                };
            }
        },
    };

    let mut first_unverified: Option<(PathBuf, String)> = None;
    for path in candidate_paths(adapter, opts) {
        let mut outputs = Vec::new();
        let mut failure = None;
        for args in adapter.identity_probes() {
            match run_probe(&path, args, &env, None, opts.probe_timeout).await {
                Ok(out) => outputs.push(out),
                Err(e) => {
                    failure = Some(e.to_string());
                    break;
                }
            }
        }
        let reason = match failure {
            Some(reason) => reason,
            None => match adapter.identify(&outputs) {
                Some(version) => {
                    let pin = adapter.version_pin().classify(&version);
                    tracing::debug!(adapter = %adapter.id(), path = %path.display(), %version, ?pin, "identified vendor cli");
                    return Detection::Installed(InstalledCli {
                        adapter: adapter.id(),
                        path,
                        version,
                        pin,
                    });
                }
                None => format!(
                    "`{}` did not identify itself as {}",
                    path.display(),
                    adapter.id().display_name()
                ),
            },
        };
        tracing::debug!(adapter = %adapter.id(), path = %path.display(), %reason, "candidate rejected");
        first_unverified.get_or_insert((path, reason));
    }

    match first_unverified {
        Some((path, reason)) => Detection::Unverified { path, reason },
        None => Detection::NotInstalled,
    }
}
