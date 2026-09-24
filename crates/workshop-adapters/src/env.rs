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
    // The user's own sudo askpass helper (a program path, not a credential): a command that
    // needs a password reaches it, as it does in Grok Build's shell tool.
    "SUDO_ASKPASS",
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

/// Variables Workshop sets on the `opencode serve` engine itself; the parent's values must never
/// leak in, so the engine passthrough drops them. The server password and the permission policy are
/// security-relevant (a user's ambient value must not override the host's), and the inline config is
/// how Workshop installs its shell and prompt.
const ENGINE_MANAGED: &[&str] = &[
    "OPENCODE_PERMISSION",
    "OPENCODE_CONFIG_CONTENT",
    "OPENCODE_SERVER_PASSWORD",
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

/// The environment for the OpenCode engine, whose commands are the user's own commands.
///
/// Unlike [`minimal_env`] (the strict allowlist for vendor-CLI probes), this passes the parent's
/// environment through untouched except for the same secret-shaped deny list, and adds none of the
/// CI-style [`FIXED`] values. Grok Build's shell tool inherits the user's environment untouched
/// (`xai-grok-tools/src/computer/local/terminal.rs`, `apply_child_env`); this is the engine's
/// equivalent. In particular `DISPLAY`, `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`,
/// `XAUTHORITY` and `SSH_AUTH_SOCK` reach the engine's commands, so a GUI the model launches can
/// open on the desktop, and `CI` / `TERM=dumb` / `NO_COLOR` are not forced onto them.
///
/// `extra` entries are added after the passthrough and are rejected if they are secret-shaped, the
/// same as [`minimal_env`].
pub fn engine_env<I>(
    parent: I,
    extra: &[(String, String)],
) -> Result<BTreeMap<OsString, OsString>, DeniedEnvVar>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut out = BTreeMap::new();
    for (k, v) in parent {
        match k.to_str() {
            // A secret-shaped name never reaches the engine, exactly as for a probe; nor does a
            // variable Workshop sets on the engine itself (the parent must not override the server
            // password, the permission policy, or the inline config).
            Some(key) if is_denied(key) || ENGINE_MANAGED.contains(&key) => continue,
            // Non-UTF-8 names cannot be secret-shaped (the deny list is ASCII) and Grok Build
            // inherits them too; pass them through rather than dropping the user's environment.
            _ => {
                out.insert(k, v);
            }
        }
    }
    for (k, v) in extra {
        if is_denied(k) {
            return Err(DeniedEnvVar(k.clone()));
        }
        out.insert(OsString::from(k), OsString::from(v));
    }
    Ok(out)
}

/// Convenience: [`engine_env`] over the current process environment.
pub fn engine_env_from_process(
    extra: &[(String, String)],
) -> Result<BTreeMap<OsString, OsString>, DeniedEnvVar> {
    engine_env(std::env::vars_os(), extra)
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
                ("SUDO_ASKPASS", "/usr/bin/ssh-askpass"),
            ]),
            &[],
        )
        .unwrap();
        let keys: Vec<&str> = env.keys().map(|k| k.to_str().unwrap()).collect();
        assert_eq!(
            keys,
            vec![
                "CI",
                "CODEX_HOME",
                "HOME",
                "NO_COLOR",
                "PATH",
                "SUDO_ASKPASS",
                "TERM"
            ]
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

    #[test]
    fn engine_env_passes_the_desktop_through_and_forces_no_ci_style() {
        let env = engine_env(
            parent(&[
                ("HOME", "/home/u"),
                ("PATH", "/usr/bin"),
                ("DISPLAY", ":1"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
                ("XAUTHORITY", "/home/u/.Xauthority"),
                ("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock"),
                ("TERM", "xterm-256color"),
                ("RANDOM_THING", "x"),
                // Still secret-shaped: never reaches the engine, exactly as for a probe.
                ("ANTHROPIC_API_KEY", "sk-ant"),
                ("GITHUB_TOKEN", "ghp"),
                // Host-owned engine controls: the parent's values must never leak in.
                ("OPENCODE_PERMISSION", "{\"bash\":\"allow\"}"),
                ("OPENCODE_SERVER_PASSWORD", "hunter2"),
                ("OPENCODE_CONFIG_CONTENT", "{}"),
            ]),
            &[],
        )
        .unwrap();
        let get = |k: &str| env.get(&OsString::from(k)).and_then(|v| v.to_str());
        // The desktop reaches the engine, so a GUI the model launches can open on the desktop.
        assert_eq!(get("DISPLAY"), Some(":1"));
        assert_eq!(get("WAYLAND_DISPLAY"), Some("wayland-0"));
        assert_eq!(
            get("DBUS_SESSION_BUS_ADDRESS"),
            Some("unix:path=/run/user/1000/bus")
        );
        assert_eq!(get("XAUTHORITY"), Some("/home/u/.Xauthority"));
        assert_eq!(get("SSH_AUTH_SOCK"), Some("/run/user/1000/ssh-agent.sock"));
        // The user's own terminal type is kept; no CI-style values are forced on.
        assert_eq!(get("TERM"), Some("xterm-256color"));
        assert_eq!(get("RANDOM_THING"), Some("x"));
        assert!(!env.contains_key(&OsString::from("CI")));
        assert!(!env.contains_key(&OsString::from("NO_COLOR")));
        // Secrets are still stripped.
        assert!(!env.contains_key(&OsString::from("ANTHROPIC_API_KEY")));
        assert!(!env.contains_key(&OsString::from("GITHUB_TOKEN")));
        // Host-owned engine controls never leak in from the parent.
        assert!(!env.contains_key(&OsString::from("OPENCODE_PERMISSION")));
        assert!(!env.contains_key(&OsString::from("OPENCODE_SERVER_PASSWORD")));
        assert!(!env.contains_key(&OsString::from("OPENCODE_CONFIG_CONTENT")));
    }

    #[test]
    fn engine_env_extra_cannot_smuggle_secrets() {
        let err = engine_env(parent(&[]), &[("MY_TOKEN".into(), "t".into())]).unwrap_err();
        assert_eq!(err.0, "MY_TOKEN");
    }
}
