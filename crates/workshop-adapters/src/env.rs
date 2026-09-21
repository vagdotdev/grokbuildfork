//! Minimal child environment for probes and runs.
//!
//! The child gets an allowlist, never the parent's full environment. That
//! keeps Workshop's Direct API keys (and anything else secret-shaped) away
//! from a delegated CLI, and prevents vendor base-URL overrides from
//! silently redirecting a subscription CLI at a proxy.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

/// Variables passed through verbatim when present in the parent.
const PASSTHROUGH: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TEMP",
    "TMP",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // Vendor config-home overrides (locations, not credentials).
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "OPENCODE_CONFIG",
    "OPENCODE_CONFIG_DIR",
];

/// Fixed values that make CLI output machine-friendly.
const FIXED: &[(&str, &str)] = &[("NO_COLOR", "1"), ("TERM", "dumb"), ("CI", "1")];

/// Key fragments that are never allowed, even via `extra`.
const DENY_FRAGMENTS: &[&str] = &["API_KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"];
const DENY_PREFIXES: &[&str] = &[
    "XAI_",
    "GROK_",
    "ANTHROPIC_",
    "OPENAI_",
    "OPENROUTER_",
    "CURSOR_API",
    "AWS_",
    "AZURE_",
    "GOOGLE_",
];

#[derive(Debug, thiserror::Error)]
#[error("refusing to pass `{0}` to a delegated CLI: secret-shaped variable")]
pub struct DeniedEnvVar(pub String);

/// True when a variable name must never reach a delegated CLI.
pub fn is_denied(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    DENY_FRAGMENTS.iter().any(|f| upper.contains(f))
        || DENY_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// Build the child environment from `parent` (normally [`std::env::vars_os`]).
///
/// `extra` entries are added after the allowlist and are rejected if they are
/// secret-shaped, so a caller cannot accidentally leak a key through them.
pub fn minimal_env<I>(
    parent: I,
    extra: &[(String, String)],
) -> Result<BTreeMap<OsString, OsString>, DeniedEnvVar>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut out = BTreeMap::new();
    for (k, v) in parent {
        let Some(key) = k.to_str() else { continue };
        if PASSTHROUGH.contains(&key) && !is_denied(key) {
            out.insert(k, v);
        }
    }
    for (k, v) in FIXED {
        out.insert(OsString::from(k), OsString::from(v));
    }
    for (k, v) in extra {
        if is_denied(k) {
            return Err(DeniedEnvVar(k.clone()));
        }
        out.insert(OsString::from(k), OsString::from(v));
    }
    Ok(out)
}

/// Convenience: [`minimal_env`] over the current process environment.
pub fn minimal_env_from_process(
    extra: &[(String, String)],
) -> Result<BTreeMap<OsString, OsString>, DeniedEnvVar> {
    minimal_env(std::env::vars_os(), extra)
}

/// Apply an environment map to a command, replacing whatever it had.
pub fn apply(cmd: &mut tokio::process::Command, env: &BTreeMap<OsString, OsString>) {
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(OsStr::new(k), OsStr::new(v));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn strips_api_keys_and_vendor_overrides() {
        let env = minimal_env(
            parent(&[
                ("HOME", "/home/u"),
                ("PATH", "/usr/bin"),
                ("ANTHROPIC_API_KEY", "sk-ant"),
                ("ANTHROPIC_BASE_URL", "http://127.0.0.1:3456"),
                ("OPENAI_API_KEY", "sk"),
                ("XAI_API_KEY", "xai"),
                ("CURSOR_API_KEY", "cur"),
                ("GITHUB_TOKEN", "ghp"),
                ("RANDOM_THING", "x"),
                ("CODEX_HOME", "/home/u/.codex"),
            ]),
            &[],
        )
        .unwrap();
        let keys: Vec<&str> = env.keys().map(|k| k.to_str().unwrap()).collect();
        assert_eq!(
            keys,
            vec!["CI", "CODEX_HOME", "HOME", "NO_COLOR", "PATH", "TERM"]
        );
    }

    #[test]
    fn extra_cannot_smuggle_secrets() {
        let err = minimal_env(parent(&[]), &[("MY_TOKEN".into(), "t".into())]).unwrap_err();
        assert_eq!(err.0, "MY_TOKEN");
        assert!(is_denied("openai_api_key"));
        assert!(is_denied("AWS_SECRET_ACCESS_KEY"));
        assert!(!is_denied("CLAUDE_CONFIG_DIR"));
    }
}
