//! macOS Gatekeeper quarantine handling for the installed `opencode` binary. (Engine state and
//! the server log live in the host — this crate never reads file contents, see the no-theft gate.)

use std::path::Path;

/// The macOS quarantine attribute on `path` (`Some(value)` when set); `None` elsewhere or when
/// absent. A quarantined binary is what Gatekeeper refuses to run from a non-terminal spawn.
pub fn quarantine_flag(path: &Path) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Remove the macOS quarantine attribute from a freshly installed binary (best-effort; no-op
/// elsewhere). Mirrors what `scripts/install.sh` does for the `workshop` binary itself.
pub fn clear_quarantine(path: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let _ = std::process::Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(path)
        .output();
}
