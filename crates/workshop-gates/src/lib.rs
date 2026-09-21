//! Shared helpers for the Workshop gate tests (`tests/gate*.rs`).
//!
//! The gates are integration tests so each one links the real upstream crate
//! and asserts on the compiled default, not on a grep count. See
//! `docs/workshop/adr/0001-overlay-sync.md` for why they must stay red on a
//! pristine upstream snapshot.

#![deny(clippy::indexing_slicing)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Serialises tests that mutate the process environment. `std::env` is
/// process-global and the test harness runs tests on threads.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Env override that restores the previous values on drop (panics included).
pub struct HermeticEnv {
    _lock: MutexGuard<'static, ()>,
    restore: Vec<(&'static str, Option<std::ffi::OsString>)>,
    /// Fresh Workshop home; dropped (deleted) with the guard.
    pub home: tempfile::TempDir,
}

impl HermeticEnv {
    /// A production-shaped environment: no `GROK_OAUTH2_*` / `GROK_OIDC_*`
    /// override, no local-auth stand-in, no first-party key, and both home
    /// env vars pointed at an empty temp dir so no marker or `auth.json` leaks in.
    pub fn production() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = tempfile::tempdir().expect("tempdir");
        let mut env = Self {
            _lock: lock,
            restore: Vec::new(),
            home,
        };
        for key in [
            "GROK_OAUTH2_ISSUER",
            "GROK_OAUTH2_CLIENT_ID",
            "GROK_OAUTH2_SCOPES",
            "GROK_OAUTH2_PRINCIPAL_TYPE",
            "GROK_OAUTH2_PRINCIPAL_ID",
            "GROK_OIDC_ISSUER",
            "GROK_OIDC_CLIENT_ID",
            "GROK_LOCAL_AUTH",
            "GROK_AUTH_PROVIDER_COMMAND",
            "GROK_AUTH_PROVIDER_LABEL",
            "GROK_DISABLE_API_KEY_AUTH",
            "XAI_API_KEY",
            "GROK_CODE_XAI_API_KEY",
            "GROK_AUTH",
            "GROK_AUTH_PATH",
            "GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL",
            "GROK_PRODUCTION_ASSET_SERVER_URL",
            "GROK_PRODUCTION_WS_URL",
            "GROK_PRODUCTION_GATEWAY_WS_URL",
            "GROK_PRODUCTION_WS_ORIGIN",
        ] {
            env.remove(key);
        }
        let home_str = env.home.path().to_string_lossy().into_owned();
        env.set(workshop_branding::HOME_ENV, &home_str);
        env.set(workshop_branding::LEGACY_HOME_ENV, &home_str);
        env
    }

    pub fn set(&mut self, key: &'static str, value: &str) {
        self.restore.push((key, std::env::var_os(key)));
        // SAFETY: serialised by ENV_LOCK; tests are the only callers.
        unsafe { std::env::set_var(key, value) };
    }

    pub fn remove(&mut self, key: &'static str) {
        self.restore.push((key, std::env::var_os(key)));
        // SAFETY: serialised by ENV_LOCK; tests are the only callers.
        unsafe { std::env::remove_var(key) };
    }
}

impl Drop for HermeticEnv {
    fn drop(&mut self) {
        for (key, prev) in self.restore.drain(..).rev() {
            // SAFETY: still under ENV_LOCK (the guard field drops after this).
            match prev {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

/// Repository root (two levels above this crate's manifest).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/workshop-gates sits two levels below the repo root")
        .to_path_buf()
}

/// Rust source with `//` line comments and `/* */` block comments removed, so
/// a comment that *names* a forbidden thing does not trip a source gate.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_block = 0usize;
    let mut in_str = false;
    let mut in_line = false;
    while let Some(c) = chars.next() {
        if in_line {
            if c == '\n' {
                in_line = false;
                out.push('\n');
            }
            continue;
        }
        if in_block > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_block += 1;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                in_line = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block = 1;
            }
            _ => out.push(c),
        }
    }
    out
}

/// Source with trailing `#[cfg(test)]` modules removed. Test modules may use
/// forbidden hosts as negative examples; they are not compiled into the binary.
pub fn strip_test_modules(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => src.get(..idx).unwrap_or(src),
        None => src,
    }
}

/// Comments and test modules removed: what a default-path source gate scans.
pub fn scannable_source(src: &str) -> String {
    strip_comments(strip_test_modules(src))
}

/// `true` when `url` names an xAI / Grok production host.
pub fn url_has_forbidden_host(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        // Not a URL: fall back to a substring check so a malformed default
        // still cannot smuggle a forbidden host through.
        let lower = url.to_ascii_lowercase();
        return lower.contains("x.ai") || lower.contains("grok.com");
    };
    parsed
        .host_str()
        .is_some_and(workshop_branding::is_forbidden_default_host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_keeps_strings_and_drops_comments() {
        let src = "let a = \"https://x.ai // not a comment\"; // https://auth.x.ai\n/* grok.com */ let b = 1;";
        let stripped = strip_comments(src);
        assert!(stripped.contains("https://x.ai // not a comment"));
        assert!(!stripped.contains("auth.x.ai"));
        assert!(!stripped.contains("grok.com"));
        assert!(stripped.contains("let b = 1;"));
    }

    #[test]
    fn strip_test_modules_cuts_at_cfg_test() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests { const X: &str = \"https://x.ai/cli\"; }";
        assert_eq!(strip_test_modules(src), "fn a() {}\n");
        assert!(!scannable_source(src).contains("x.ai"));
    }

    #[test]
    fn forbidden_host_detection() {
        assert!(url_has_forbidden_host("https://auth.x.ai"));
        assert!(url_has_forbidden_host("wss://code.grok.com/ws/code-agent"));
        assert!(!url_has_forbidden_host("https://api.workshop.invalid/v1"));
        assert!(!url_has_forbidden_host("http://127.0.0.1:11434/v1"));
    }
}
