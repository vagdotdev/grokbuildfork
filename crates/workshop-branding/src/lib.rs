//! Workshop identity constants.
//!
//! Only user-visible surfaces change. Crate names, proto packages, Rust module
//! names, and `GROK_*` operator env vars stay as upstream ships them so the
//! overlay keeps replaying onto new `xai-org/grok-build` snapshots
//! (see `docs/workshop/adr/0001-overlay-sync.md`).

#![deny(clippy::indexing_slicing)]

/// User-visible product name.
pub const PRODUCT_NAME: &str = "Workshop";

/// The one shipped binary (`xai-grok-pager-bin` `[[bin]] name`).
pub const BINARY_NAME: &str = "workshop";

/// Home directory name under `$HOME`.
pub const HOME_DIR_NAME: &str = ".workshop";

/// Env var that overrides the home directory.
pub const HOME_ENV: &str = "WORKSHOP_HOME";

/// Upstream's home override. Honoured with a warning as a migration alias for
/// at most two pre-GA releases (plan §5), then dropped.
pub const LEGACY_HOME_ENV: &str = "GROK_HOME";

/// Legacy home directory names a migration preview may import from (never `auth.json`).
pub const LEGACY_HOME_DIR_NAMES: &[&str] = &[".grok", ".docking"];

/// Per-project directory name.
pub const PROJECT_DIR_NAME: &str = ".workshop";

/// `~/.workshop/config.toml`, as shown in hints and errors.
pub const CONFIG_PATH_HINT: &str = "~/.workshop/config.toml";

/// `~/.workshop/auth.json`, as shown in hints and errors.
pub const AUTH_PATH_HINT: &str = "~/.workshop/auth.json";

/// Shown when a code path that upstream routed to xAI infrastructure is reached
/// without a configured connection.
pub const NO_CONNECTION_HINT: &str =
    "No connection configured. Run `workshop` and press `l` to connect a local model, an API key, or a subscription CLI.";

/// Reserved placeholder host suffix (RFC 6761 `.invalid` never resolves).
/// Every inherited xAI endpoint default is repointed here so an accidental
/// call fails at DNS instead of reaching x.ai / grok.com.
pub const PLACEHOLDER_HOST_SUFFIX: &str = "workshop.invalid";

/// Returns `true` when `host` is an xAI / Grok production host that Workshop
/// must never contact by default.
pub fn is_forbidden_default_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["x.ai", "grok.com"]
        .iter()
        .any(|apex| host == *apex || host.ends_with(&format!(".{apex}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_hosts_match_apex_and_subdomains_only() {
        for host in [
            "x.ai",
            "auth.x.ai",
            "accounts.x.ai",
            "api.x.ai",
            "grok.com",
            "cli-chat-proxy.grok.com",
            "ASSETS.GROK.COM",
        ] {
            assert!(is_forbidden_default_host(host), "{host}");
        }
        for host in [
            "workshop.invalid",
            "api.workshop.invalid",
            "notx.ai",
            "grok.community",
            "127.0.0.1",
            "localhost",
        ] {
            assert!(!is_forbidden_default_host(host), "{host}");
        }
    }
}
