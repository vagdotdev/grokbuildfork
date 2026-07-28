//! Cold-start machine scan: detect and vault credentials already on this PC.
//!
//! Sources (import when readable):
//! - Environment API keys
//! - OpenCode `auth.json`
//! - Codex `~/.codex/auth.json` (API key and/or ChatGPT OAuth)
//! - Claude Code Keychain (`Claude Code-credentials`)
//! - macOS Keychain internet passwords for known API hosts
//! - Known generic Keychain API-key accounts
//!
//! Presence-only (shown, not silently decrypted):
//! - Claude / Codex / OpenCode “Safe Storage” blobs
//! - Claude Code / Codex install dirs when no secret could be read
//!
//! Secrets never leave Keychain references in config and never appear in logs.

use crate::provider_cmd::{self, AutoDockReport};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::PathBuf;
use zeroize::{Zeroize, Zeroizing};

/// Opt out of every cold-start dock path.
const OPT_OUT_ENV: &str = "DOCKING_NO_AUTO_IMPORT";

/// Run the full machine scan and vault anything newly readable.
/// Runs at most once per process. After providers are already docked, skips
/// foreign Keychain harvest (the source of repeated macOS security prompts).
pub fn auto_dock_on_startup() -> Option<AutoDockReport> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static RAN: AtomicBool = AtomicBool::new(false);
    if RAN.swap(true, Ordering::SeqCst) {
        return None;
    }
    if std::env::var_os(OPT_OUT_ENV).is_some() {
        return None;
    }

    let already_docked = provider_cmd::list_providers()
        .map(|rows| !rows.is_empty())
        .unwrap_or(false);

    let mut imported = 0usize;
    let mut presence = BTreeSet::new();
    let mut errors = Vec::new();

    // OpenCode-style: only env vars + local auth files. Never touch the OS
    // Keychain on the hot path (that is what caused constant allow-access dialogs).
    match dock_env_keys() {
        Ok(n) => imported += n,
        Err(error) => errors.push(format!("env: {error}")),
    }
    match dock_opencode() {
        Ok((n, seen)) => {
            imported += n;
            if seen {
                presence.insert("OpenCode");
            }
        }
        Err(error) => errors.push(format!("opencode: {error}")),
    }
    match dock_codex() {
        Ok((n, seen)) => {
            imported += n;
            if seen {
                presence.insert("Codex");
            }
        }
        Err(error) => errors.push(format!("codex: {error}")),
    }

    // Presence-only (no secret unlock).
    if path_exists_home(".claude") || already_docked {
        if path_exists_home(".claude") {
            presence.insert("Claude Code");
        }
    }
    for label in presence_only_hits() {
        presence.insert(label);
    }

    if imported == 0 && presence.is_empty() && errors.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if imported > 0 {
        parts.push(format!(
            "Docked {imported} credential(s) into the local vault"
        ));
    }
    if !presence.is_empty() {
        parts.push(format!(
            "Detected: {}",
            presence.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if !errors.is_empty() {
        parts.push(format!("scan notes: {}", errors.join("; ")));
    }
    if imported > 0 {
        parts.push("OAuth stays adapter-pending until enabled. /providers".to_owned());
    }

    Some(AutoDockReport {
        imported,
        skipped: 0,
        message: parts.join(". "),
    })
}

fn dock_env_keys() -> Result<usize> {
    let mut n = 0usize;
    for (provider, env_name) in [
        ("openrouter", "OPENROUTER_API_KEY"),
        ("openai", "OPENAI_API_KEY"),
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("xai", "XAI_API_KEY"),
        ("xai", "GROK_API_KEY"),
        ("groq", "GROQ_API_KEY"),
        ("deepseek", "DEEPSEEK_API_KEY"),
        ("mistral", "MISTRAL_API_KEY"),
        ("gemini", "GEMINI_API_KEY"),
        ("gemini", "GOOGLE_API_KEY"),
        ("together", "TOGETHER_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
        ("perplexity", "PERPLEXITY_API_KEY"),
    ] {
        if let Ok(value) = std::env::var(env_name) {
            let mut value = value;
            if !value.trim().is_empty() {
                provider_cmd::vault_api_key(provider, "env", &value)?;
                n += 1;
            }
            value.zeroize();
        }
    }
    Ok(n)
}

fn dock_opencode() -> Result<(usize, bool)> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/opencode/auth.json");
    if !path.is_file() {
        return Ok((0, false));
    }
    let body = Zeroizing::new(std::fs::read_to_string(&path)?);
    let value: serde_json::Value = serde_json::from_str(&body)?;
    let Some(map) = value.as_object() else {
        return Ok((0, true));
    };
    let mut n = 0usize;
    for (raw_id, entry) in map {
        let auth_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        match auth_type {
            "api" => {
                if let Some(key) = ["key", "apiKey", "token"]
                    .iter()
                    .find_map(|field| entry.get(*field).and_then(|v| v.as_str()))
                {
                    provider_cmd::vault_api_key(raw_id, "opencode", key)?;
                    n += 1;
                }
            }
            "oauth" => {
                let secret = Zeroizing::new(serde_json::to_string(entry)?);
                provider_cmd::vault_oauth_json(raw_id, "opencode", &secret)?;
                n += 1;
            }
            _ => {}
        }
    }
    Ok((n, true))
}

fn dock_codex() -> Result<(usize, bool)> {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex/auth.json");
    if !path.is_file() {
        return Ok((0, path_exists_home(".codex")));
    }
    let body = Zeroizing::new(std::fs::read_to_string(&path)?);
    let value: serde_json::Value = serde_json::from_str(&body)?;
    let mut n = 0usize;

    if let Some(key) = value
        .get("OPENAI_API_KEY")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
    {
        provider_cmd::vault_api_key("openai", "codex", key)?;
        n += 1;
    }

    if value.get("tokens").is_some() {
        let secret = Zeroizing::new(serde_json::to_string(&value)?);
        // ChatGPT / Codex subscription — vault under codex + openai for discovery.
        provider_cmd::vault_oauth_json("codex", "codex", &secret)?;
        provider_cmd::vault_oauth_json("openai", "codex", &secret)?;
        n += 2;
    }

    Ok((n, true))
}

fn presence_only_hits() -> Vec<&'static str> {
    let mut hits = Vec::new();
    if path_exists_home(".claude") {
        hits.push("Claude Code install");
    }
    if path_exists_home(".codex") {
        hits.push("Codex install");
    }
    if dirs::home_dir()
        .map(|h| h.join(".local/share/opencode").is_dir())
        .unwrap_or(false)
    {
        hits.push("OpenCode install");
    }
    if std::env::var_os("GITHUB_TOKEN").is_some()
        || std::env::var_os("GH_TOKEN").is_some()
        || path_exists_home(".config/github-copilot")
        || path_exists_home(".config/gh")
    {
        hits.push("GitHub/Copilot tooling");
    }
    hits
}

fn path_exists_home(rel: &str) -> bool {
    dirs::home_dir()
        .map(|home| home.join(rel).exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_disables_scan() {
        // Safety: test-only env mutation in a unit test process.
        unsafe { std::env::set_var(OPT_OUT_ENV, "1") };
        assert!(auto_dock_on_startup().is_none());
        unsafe { std::env::remove_var(OPT_OUT_ENV) };
    }
}
