//! Locate candidate vendor binaries on `PATH` and in the known install directories.
//!
//! Locating is presence-only. A candidate becomes a vendor CLI only after [`crate::identify`]
//! confirms it.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::model::Vendor;

/// Where detection looks and how it runs children. `Default` reads the real `PATH` and `HOME`.
#[derive(Debug, Clone)]
pub struct DetectConfig {
    /// Override for `PATH` (tests point this at a fixture directory).
    pub search_path: Option<OsString>,
    /// Override for the home directory used to expand known install dirs.
    pub home: Option<PathBuf>,
    /// Directories scanned right after `PATH`, before the known dirs: a Workshop-owned install
    /// (the engine's `tools/opencode` tree) wins over a copy elsewhere.
    pub preferred_dirs: Vec<PathBuf>,
    /// Also scan the known install directories after `PATH`.
    pub include_known_dirs: bool,
    /// Extra directories scanned after the known dirs.
    pub extra_dirs: Vec<PathBuf>,
    /// Per-child timeout for `--version`, `--help`, and status commands.
    pub timeout: Duration,
    /// Budget for one rail's model-list probe ([`crate::models::subscription_models`]).
    pub models_timeout: Duration,
    /// How long a `claude` child that outlived its budget may keep running before it is killed.
    /// Never shorten this in production: see [`crate::process::VendorSlot`].
    pub claude_kill_grace: Duration,
    /// Extra environment for children (fixtures only; credential-like names are rejected).
    pub extra_env: Vec<(OsString, OsString)>,
    /// Run the official status command. `false` is a presence-only scan (the user declined the
    /// deeper probe); login state is then `None`.
    pub check_login: bool,
    /// Override for the directories checked for the Cursor desktop app.
    pub cursor_app_paths: Option<Vec<PathBuf>>,
}

impl Default for DetectConfig {
    fn default() -> Self {
        Self {
            search_path: None,
            home: None,
            preferred_dirs: Vec::new(),
            include_known_dirs: true,
            extra_dirs: Vec::new(),
            timeout: Duration::from_secs(8),
            models_timeout: Duration::from_secs(6),
            // Traycer's OAUTH_REFRESH_SAFE_TEARDOWN_GRACE_MS (`ephemeral-probe.ts`).
            claude_kill_grace: Duration::from_secs(30),
            extra_env: Vec::new(),
            check_login: true,
            cursor_app_paths: None,
        }
    }
}

impl DetectConfig {
    /// Hermetic configuration for tests: only `search_path` and `extra_dirs` are scanned.
    pub fn hermetic(search_path: impl Into<OsString>, home: impl Into<PathBuf>) -> Self {
        Self {
            search_path: Some(search_path.into()),
            home: Some(home.into()),
            include_known_dirs: false,
            cursor_app_paths: Some(Vec::new()),
            ..Self::default()
        }
    }

    /// Kill grace for a vendor child that outlived its budget: [`Self::claude_kill_grace`] for
    /// Claude, none for the others (their group is killed at the deadline).
    pub fn kill_grace(&self, vendor: Vendor) -> Duration {
        match vendor {
            Vendor::Claude => self.claude_kill_grace,
            Vendor::Codex | Vendor::Cursor | Vendor::OpenCode => Duration::ZERO,
        }
    }

    pub fn home_dir(&self) -> Option<PathBuf> {
        self.home
            .clone()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
    }

    fn path_entries(&self) -> Vec<PathBuf> {
        let raw = self
            .search_path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        std::env::split_paths(&raw)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    }

    /// Directories checked for the Cursor desktop app (presence only).
    pub fn cursor_app_dirs(&self) -> Vec<PathBuf> {
        if let Some(p) = &self.cursor_app_paths {
            return p.clone();
        }
        let mut dirs = vec![
            PathBuf::from("/Applications/Cursor.app"),
            PathBuf::from("/opt/Cursor"),
            PathBuf::from("/opt/cursor"),
            PathBuf::from("/usr/share/cursor"),
        ];
        if let Some(home) = self.home_dir() {
            dirs.push(home.join("Applications/Cursor.app"));
            dirs.push(home.join(".local/share/cursor"));
        }
        dirs
    }
}

/// Known install directories, in scan order, expanded against `home`.
pub fn known_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    // /opt/homebrew/opt/*/bin
    if let Ok(entries) = std::fs::read_dir("/opt/homebrew/opt") {
        let mut opt: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path().join("bin"))
            .filter(|p| p.is_dir())
            .collect();
        opt.sort();
        dirs.extend(opt);
    }
    // Vendor-owned install locations that are not always on PATH.
    dirs.push(home.join(".claude/local"));
    dirs.push(home.join(".opencode/bin"));
    // `npm install -g` with a user prefix (Codex, Claude Code before the native installer).
    dirs.push(home.join(".npm-global/bin"));
    // macOS app bundles that ship a bin folder.
    dirs.push(PathBuf::from(
        "/Applications/Cursor.app/Contents/Resources/app/bin",
    ));
    dirs.push(home.join("Applications/Cursor.app/Contents/Resources/app/bin"));
    dirs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    Path,
    KnownDir,
}

/// An executable with a vendor-looking name. Not yet verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub name: &'static str,
    pub source: CandidateSource,
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

fn executable_variants(dir: &Path, name: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut v = vec![dir.join(name)];
        for ext in ["exe", "cmd", "bat"] {
            v.push(dir.join(format!("{name}.{ext}")));
        }
        v
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(name)]
    }
}

/// All candidate executables for `vendor`, `PATH` first (in `PATH` order, each vendor name in
/// preference order per directory), then `preferred_dirs`, then known dirs, then `extra_dirs`.
/// Duplicates that resolve to the same file are dropped.
pub fn locate(vendor: Vendor, cfg: &DetectConfig) -> Vec<Candidate> {
    let mut dirs: Vec<(PathBuf, CandidateSource)> = cfg
        .path_entries()
        .into_iter()
        .map(|d| (d, CandidateSource::Path))
        .collect();
    dirs.extend(
        cfg.preferred_dirs
            .iter()
            .cloned()
            .map(|d| (d, CandidateSource::KnownDir)),
    );
    if cfg.include_known_dirs
        && let Some(home) = cfg.home_dir()
    {
        dirs.extend(
            known_dirs(&home)
                .into_iter()
                .map(|d| (d, CandidateSource::KnownDir)),
        );
    }
    dirs.extend(
        cfg.extra_dirs
            .iter()
            .cloned()
            .map(|d| (d, CandidateSource::KnownDir)),
    );

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for (dir, source) in dirs {
        for name in vendor.binary_names() {
            for path in executable_variants(&dir, name) {
                if !is_executable_file(&path) {
                    continue;
                }
                let canonical = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if seen.insert(canonical) {
                    out.push(Candidate { path, name, source });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_exec(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn known_dirs_include_plan_list() {
        let dirs = known_dirs(Path::new("/home/u"));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        assert!(dirs.contains(&PathBuf::from("/home/u/.local/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from(
            "/Applications/Cursor.app/Contents/Resources/app/bin"
        )));
    }

    #[cfg(unix)]
    #[test]
    fn path_order_then_known_dirs_and_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        make_exec(&a, "agent");
        let real = make_exec(&b, "cursor-agent");
        // Symlink in ~/.local/bin to the same file must dedup.
        std::os::unix::fs::symlink(&real, home.join(".local/bin/cursor-agent")).unwrap();
        // Non-executable file is ignored.
        std::fs::write(a.join("cursor-agent"), "not exec").unwrap();

        let path = std::env::join_paths([&a, &b]).unwrap();
        let cfg = DetectConfig {
            search_path: Some(path),
            home: Some(home.clone()),
            include_known_dirs: true,
            ..DetectConfig::default()
        };
        let found = locate(Vendor::Cursor, &cfg);
        let paths: Vec<&Path> = found.iter().map(|c| c.path.as_path()).collect();
        assert_eq!(paths, vec![a.join("agent").as_path(), real.as_path()]);
        assert_eq!(found[0].source, CandidateSource::Path);
        assert_eq!(found[0].name, "agent");
        assert_eq!(found[1].name, "cursor-agent");
    }

    #[cfg(unix)]
    #[test]
    fn hermetic_config_ignores_known_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        make_exec(&home.join(".local/bin"), "codex");
        let cfg = DetectConfig::hermetic(tmp.path().join("empty"), &home);
        assert!(locate(Vendor::Codex, &cfg).is_empty());
    }
}
