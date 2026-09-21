//! Presence-only detection for the connection picker.
//!
//! Detection answers "is the official CLI installed?" and "is a named API-key
//! env var set?". It never reads a value, never opens another app's auth
//! files or keychain, never spawns the CLI, and never touches the network.
//! Login status (Ready) requires spawning the vendor CLI's own status command,
//! which lands with the adapter milestone; until then a found CLI is "Sign in".

use std::path::{Path, PathBuf};

/// Where a binary was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Not on `PATH` and not in any known install dir.
    Missing,
    /// Found at this path (existence + executable bit only).
    Found(PathBuf),
}

impl Presence {
    pub fn is_found(&self) -> bool {
        matches!(self, Presence::Found(_))
    }
}

/// Subscription CLIs Workshop knows how to spawn. A bare `agent` binary is
/// not Cursor; only `cursor-agent` counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VendorCli {
    Claude,
    Codex,
    CursorAgent,
    OpenCode,
}

impl VendorCli {
    pub const ALL: [VendorCli; 4] = [
        VendorCli::Claude,
        VendorCli::Codex,
        VendorCli::CursorAgent,
        VendorCli::OpenCode,
    ];

    /// Executable name on `PATH`.
    pub fn binary(self) -> &'static str {
        match self {
            VendorCli::Claude => "claude",
            VendorCli::Codex => "codex",
            VendorCli::CursorAgent => "cursor-agent",
            VendorCli::OpenCode => "opencode",
        }
    }

    /// Product name shown in the picker.
    pub fn product(self) -> &'static str {
        match self {
            VendorCli::Claude => "Claude",
            VendorCli::Codex => "Codex",
            VendorCli::CursorAgent => "Cursor",
            VendorCli::OpenCode => "OpenCode",
        }
    }

    /// The official login command Workshop attaches to the user's terminal
    /// (adapter milestone). Shown as guidance today; never captured.
    pub fn login_command(self) -> &'static str {
        match self {
            VendorCli::Claude => "claude auth login",
            VendorCli::Codex => "codex login",
            VendorCli::CursorAgent => "cursor-agent login",
            VendorCli::OpenCode => "opencode auth login",
        }
    }
}

/// API-key env vars whose *presence* the Models tab reports. Values are never
/// read here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyEnv {
    OpenAi,
    Anthropic,
    OpenRouter,
    Xai,
}

impl KeyEnv {
    pub const ALL: [KeyEnv; 4] = [
        KeyEnv::OpenAi,
        KeyEnv::Anthropic,
        KeyEnv::OpenRouter,
        KeyEnv::Xai,
    ];

    pub fn var(self) -> &'static str {
        match self {
            KeyEnv::OpenAi => "OPENAI_API_KEY",
            KeyEnv::Anthropic => "ANTHROPIC_API_KEY",
            KeyEnv::OpenRouter => "OPENROUTER_API_KEY",
            KeyEnv::Xai => "XAI_API_KEY",
        }
    }
}

/// Result of one presence scan.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub clis: Vec<(VendorCli, Presence)>,
    pub keys: Vec<(KeyEnv, bool)>,
}

impl ScanResult {
    pub fn cli(&self, cli: VendorCli) -> Presence {
        self.clis
            .iter()
            .find(|(c, _)| *c == cli)
            .map(|(_, p)| p.clone())
            .unwrap_or(Presence::Missing)
    }

    pub fn key_present(&self, key: KeyEnv) -> bool {
        self.keys
            .iter()
            .find(|(k, _)| *k == key)
            .is_some_and(|(_, present)| *present)
    }
}

/// Known install dirs checked after `PATH`.
fn known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".local/lib/node_modules/.bin"));
        dirs.push(home.join(".opencode/bin"));
        dirs.push(home.join(".codex/bin"));
        #[cfg(target_os = "macos")]
        {
            dirs.push(home.join("Applications/Cursor.app/Contents/Resources/app/bin"));
            dirs.push(home.join("Applications/Claude.app/Contents/Resources/bin"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from(
            "/Applications/Cursor.app/Contents/Resources/app/bin",
        ));
        dirs.push(PathBuf::from("/Applications/Claude.app/Contents/Resources/bin"));
    }
    dirs
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Locate `binary` on `PATH`, then in the known dirs.
pub fn find_binary(binary: &str, home: Option<&Path>) -> Presence {
    if let Ok(path) = which::which(binary) {
        return Presence::Found(path);
    }
    for dir in known_dirs(home) {
        let candidate = dir.join(binary);
        if is_executable(&candidate) {
            return Presence::Found(candidate);
        }
    }
    Presence::Missing
}

/// Presence of `var` in the process environment (non-empty). The value is discarded.
pub fn env_present(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| !v.is_empty())
}

/// Run the presence scan. `home` is the user's home directory (not the Workshop home).
pub fn scan(home: Option<&Path>) -> ScanResult {
    ScanResult {
        clis: VendorCli::ALL
            .iter()
            .map(|cli| (*cli, find_binary(cli.binary(), home)))
            .collect(),
        keys: KeyEnv::ALL
            .iter()
            .map(|key| (*key, env_present(key.var())))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_in_empty_dirs_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        // A name no system has on PATH; the known dirs under a temp home are empty.
        assert_eq!(
            find_binary("workshop-definitely-not-installed-cli", Some(dir.path())),
            Presence::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn executable_in_known_home_dir_is_found() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("workshop-test-vendor-cli");
        std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            find_binary("workshop-test-vendor-cli", Some(dir.path())),
            Presence::Found(bin)
        );
    }

    #[test]
    fn scan_reports_every_cli_and_key_exactly_once() {
        let result = scan(None);
        assert_eq!(result.clis.len(), VendorCli::ALL.len());
        assert_eq!(result.keys.len(), KeyEnv::ALL.len());
    }

    #[test]
    fn a_bare_agent_binary_is_not_cursor() {
        assert_eq!(VendorCli::CursorAgent.binary(), "cursor-agent");
    }
}
