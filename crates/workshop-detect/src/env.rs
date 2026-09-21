//! Minimal, credential-free environment for vendor CLI children.
//!
//! Vendor CLIs get an allowlisted environment: enough to find their own config and network
//! settings, and nothing that looks like a credential. Workshop's Direct API keys and any vendor
//! token a user exported in their shell are never forwarded; the CLI must rely on its own login.

use std::ffi::{OsStr, OsString};

/// Variables forwarded verbatim when present.
pub const PASSTHROUGH_EXACT: &[&str] = &[
    // Process basics
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    // Output stability
    "NO_COLOR",
    // TLS / proxies
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // XDG base dirs (vendor CLIs locate their own state through these)
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    // Vendor config-directory overrides (locations, not secrets)
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_DIR",
    // Windows basics
    "SYSTEMROOT",
    "SystemRoot",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "COMSPEC",
    "PATHEXT",
];

/// Prefixes forwarded when present (locale only).
pub const PASSTHROUGH_PREFIX: &[&str] = &["LC_"];

/// Names that are always dropped, and that [`minimal_env`] refuses to accept as extras.
pub const CREDENTIAL_NAMES: &[&str] = &[
    "CURSOR_API_KEY",
    "CURSOR_AUTH_TOKEN",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENROUTER_API_KEY",
    "OPENCODE_API_KEY",
    "XAI_API_KEY",
    "GROK_CODE_XAI_API_KEY",
];

/// Suffixes that mark a variable as credential-like regardless of vendor.
pub const CREDENTIAL_SUFFIXES: &[&str] = &[
    "_API_KEY",
    "_AUTH_TOKEN",
    "_ACCESS_TOKEN",
    "_REFRESH_TOKEN",
    "_SECRET",
    "_PASSWORD",
];

/// Prefixes that are Workshop-owned or first-party and never belong in a vendor child.
pub const NEVER_PREFIXES: &[&str] = &["WORKSHOP_", "GROK_", "XAI_"];

/// True when a variable name looks like a credential or a Workshop/first-party setting.
pub fn is_credential_like(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    CREDENTIAL_NAMES.iter().any(|n| *n == upper)
        || CREDENTIAL_SUFFIXES.iter().any(|s| upper.ends_with(s))
        || NEVER_PREFIXES.iter().any(|p| upper.starts_with(p))
        || upper == "TOKEN"
        || upper == "API_KEY"
}

fn is_allowlisted(name: &str) -> bool {
    PASSTHROUGH_EXACT.contains(&name) || PASSTHROUGH_PREFIX.iter().any(|p| name.starts_with(p))
}

/// Error returned when a caller tries to forward a credential-like variable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("refusing to forward credential-like environment variable {0}")]
pub struct CredentialInEnv(pub String);

/// Build the child environment from the current process environment plus `extra`.
///
/// Only allowlisted names are copied. `extra` is intended for test fixtures (for example the
/// directory a fake CLI keeps its state in) and is rejected if any name is credential-like.
pub fn minimal_env(
    extra: &[(OsString, OsString)],
) -> Result<Vec<(OsString, OsString)>, CredentialInEnv> {
    let mut out: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(k, _)| {
            k.to_str()
                .is_some_and(|name| is_allowlisted(name) && !is_credential_like(name))
        })
        .collect();
    // Colour codes only make parsing harder.
    if !out.iter().any(|(k, _)| k == OsStr::new("NO_COLOR")) {
        out.push((OsString::from("NO_COLOR"), OsString::from("1")));
    }
    for (k, v) in extra {
        let name = k.to_string_lossy();
        if is_credential_like(&name) {
            return Err(CredentialInEnv(name.into_owned()));
        }
        out.retain(|(existing, _)| existing != k);
        out.push((k.clone(), v.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_are_recognised() {
        for name in CREDENTIAL_NAMES {
            assert!(is_credential_like(name), "{name}");
        }
        assert!(is_credential_like("MY_VENDOR_API_KEY"));
        assert!(is_credential_like("something_access_token"));
        assert!(is_credential_like("WORKSHOP_HOME"));
        assert!(is_credential_like("GROK_HOME"));
        assert!(!is_credential_like("PATH"));
        assert!(!is_credential_like("CODEX_HOME"));
        assert!(!is_credential_like("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn minimal_env_drops_credentials_even_if_allowlisted_by_mistake() {
        // SAFETY: tests in this module are the only readers/writers of these names, and Rust runs
        // them in one process; the names are unique to this test.
        unsafe {
            std::env::set_var("WORKSHOP_DETECT_TEST_API_KEY", "sk-canary");
            std::env::set_var("OPENAI_API_KEY", "sk-canary-openai");
        }
        let env = minimal_env(&[]).unwrap();
        let names: Vec<String> = env
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n == "WORKSHOP_DETECT_TEST_API_KEY"));
        assert!(!names.iter().any(|n| n == "OPENAI_API_KEY"));
        assert!(
            env.iter()
                .all(|(_, v)| !v.to_string_lossy().contains("sk-canary"))
        );
        assert!(names.iter().any(|n| n == "NO_COLOR"));
        unsafe {
            std::env::remove_var("WORKSHOP_DETECT_TEST_API_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    #[test]
    fn extras_cannot_smuggle_credentials() {
        let err =
            minimal_env(&[(OsString::from("CURSOR_API_KEY"), OsString::from("x"))]).unwrap_err();
        assert_eq!(err, CredentialInEnv("CURSOR_API_KEY".into()));
        let ok = minimal_env(&[(
            OsString::from("FAKE_CLI_STATE_DIR"),
            OsString::from("/tmp/x"),
        )])
        .unwrap();
        assert!(
            ok.iter()
                .any(|(k, v)| k == "FAKE_CLI_STATE_DIR" && v == "/tmp/x")
        );
    }
}
