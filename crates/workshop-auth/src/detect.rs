//! Presence-only detection of official vendor CLIs.
//!
//! Looks for the exact binary name on `PATH` and in a fixed list of well-known install directories.
//! It never executes the binary, never reads its config or credentials, and never treats a
//! differently named binary (e.g. a bare `agent`) as a match.

use std::path::{Path, PathBuf};

/// Well-known install directories checked after `PATH`, in order.
pub fn known_install_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    if let Some(home) = xai_dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".local/lib/node_modules/.bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".bun/bin"));
        #[cfg(target_os = "macos")]
        {
            dirs.push(home.join("Applications/Cursor.app/Contents/Resources/app/bin"));
            dirs.push(home.join("Applications/Claude.app/Contents/Resources/bin"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/Applications/Cursor.app/Contents/Resources/app/bin"));
        dirs.push(PathBuf::from("/Applications/Claude.app/Contents/Resources/bin"));
    }
    dirs
}

fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
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

/// Find `name` (exact file name) on `PATH`, then in [`known_install_dirs`]. Presence only.
pub fn find_official_binary(name: &str) -> Option<PathBuf> {
    find_in_dirs(name, path_dirs().into_iter().chain(known_install_dirs()))
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn find_in_dirs(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    // Refuse anything that is not a bare file name so a crafted PATH cannot turn this into a path probe.
    if name.is_empty() || name.contains(['/', '\\']) {
        return None;
    }
    for dir in dirs {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if is_executable_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
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

    #[cfg(unix)]
    #[test]
    fn finds_exact_name_only() {
        let tmp = tempfile::tempdir().unwrap();
        make_exec(tmp.path(), "agent");
        let dirs = || vec![tmp.path().to_path_buf()];
        assert!(find_in_dirs("cursor-agent", dirs()).is_none(), "a bare `agent` is not Cursor");
        make_exec(tmp.path(), "cursor-agent");
        assert_eq!(
            find_in_dirs("cursor-agent", dirs()),
            Some(tmp.path().join("cursor-agent"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_executables_and_paths() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("claude"), "not executable").unwrap();
        let dirs = || vec![tmp.path().to_path_buf()];
        assert!(find_in_dirs("claude", dirs()).is_none());
        assert!(find_in_dirs("../claude", dirs()).is_none());
        assert!(find_in_dirs("", dirs()).is_none());
    }
}
