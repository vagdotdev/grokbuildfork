//! Local secret vault for Workshop provider credentials.
//!
//! Same model as OpenCode: secrets live in a user-only file under the app home
//! (`~/.docking/secrets.json`, mode 0600). No macOS Keychain on the hot path,
//! so the CLI never spams "allow access" dialogs.
//!
//! Config still stores only `keychain:<account>` references (stable name). Those
//! resolve against this local vault.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use zeroize::{Zeroize, Zeroizing};

pub const REFERENCE_PREFIX: &str = "keychain:";

#[derive(Default, Serialize, Deserialize)]
struct SecretFile {
    version: u32,
    /// account -> secret (never logged)
    secrets: HashMap<String, String>,
}

fn memory() -> &'static Mutex<HashMap<String, Zeroizing<String>>> {
    static MEM: OnceLock<Mutex<HashMap<String, Zeroizing<String>>>> = OnceLock::new();
    MEM.get_or_init(|| Mutex::new(HashMap::new()))
}

fn vault_path() -> PathBuf {
    xai_grok_config::grok_home().join("secrets.json")
}

pub fn reference(account: &str) -> String {
    format!("{REFERENCE_PREFIX}{account}")
}

pub fn account_from_reference(value: &str) -> Option<&str> {
    value
        .strip_prefix(REFERENCE_PREFIX)
        .filter(|account| !account.trim().is_empty())
}

pub fn set_secret(account: &str, secret: &[u8]) -> Result<()> {
    validate_account(account)?;
    let text = std::str::from_utf8(secret)
        .context("credential must be valid UTF-8")?
        .to_owned();
    if text.trim().is_empty() {
        bail!("credential cannot be empty");
    }
    let mut file = read_file()?;
    file.secrets.insert(account.to_owned(), text.clone());
    write_file(&file)?;
    // Zeroize file struct secret strings after write by dropping via Zeroizing map.
    for value in file.secrets.values_mut() {
        value.zeroize();
    }
    memory()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(account.to_owned(), Zeroizing::new(text));
    Ok(())
}

/// Store only if missing. No-op when already present (no rewrite churn).
pub fn set_secret_if_absent(account: &str, secret: &[u8]) -> Result<bool> {
    validate_account(account)?;
    if secret_exists(account)? {
        return Ok(false);
    }
    set_secret(account, secret)?;
    Ok(true)
}

pub fn get_secret(account: &str) -> Result<Zeroizing<Vec<u8>>> {
    validate_account(account)?;
    {
        let guard = memory().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = guard.get(account) {
            return Ok(Zeroizing::new(hit.as_bytes().to_vec()));
        }
    }
    let file = read_file()?;
    let Some(value) = file.secrets.get(account) else {
        bail!("credential for {account} is not in the local vault");
    };
    if value.trim().is_empty() {
        bail!("credential for {account} is empty");
    }
    memory()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(account.to_owned(), Zeroizing::new(value.clone()));
    Ok(Zeroizing::new(value.as_bytes().to_vec()))
}

pub fn secret_exists(account: &str) -> Result<bool> {
    validate_account(account)?;
    if memory()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(account)
    {
        return Ok(true);
    }
    Ok(read_file()?.secrets.contains_key(account))
}

pub fn delete_secret(account: &str) -> Result<()> {
    validate_account(account)?;
    memory()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(account);
    let mut file = read_file()?;
    if file.secrets.remove(account).is_none() {
        bail!("credential for {account} is not in the local vault");
    }
    write_file(&file)
}

pub fn delete_secret_if_present(account: &str) -> Result<()> {
    validate_account(account)?;
    memory()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(account);
    let mut file = read_file()?;
    if file.secrets.remove(account).is_some() {
        write_file(&file)?;
    }
    Ok(())
}

pub fn resolve_reference(value: &str) -> Option<String> {
    let account = account_from_reference(value)?;
    // One-time pull from the old Keychain service into the local vault if needed.
    let _ = migrate_legacy_keychain_account(account);
    let secret = get_secret(account).ok()?;
    std::str::from_utf8(&secret)
        .ok()
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

/// Copy a secret from the legacy macOS Keychain service into the local vault
/// once, then never touch Keychain again for that account.
pub fn migrate_legacy_keychain_account(account: &str) -> Result<bool> {
    validate_account(account)?;
    if secret_exists(account)? {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "sh.docking.credentials",
                "-a",
                account,
                "-w",
            ])
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let mut text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !text.is_empty() {
                set_secret(account, text.as_bytes())?;
                text.zeroize();
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Migrate all known accounts from the legacy Keychain into the local vault.
pub fn migrate_legacy_keychain_accounts(accounts: &[&str]) -> usize {
    accounts
        .iter()
        .filter(|account| migrate_legacy_keychain_account(account).unwrap_or(false))
        .count()
}

fn read_file() -> Result<SecretFile> {
    let path = vault_path();
    match fs::read_to_string(&path) {
        Ok(body) => {
            let parsed: SecretFile = serde_json::from_str(&body)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            Ok(parsed)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SecretFile {
            version: 1,
            secrets: HashMap::new(),
        }),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_file(file: &SecretFile) -> Result<()> {
    let path = vault_path();
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid secrets path"))?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    let mut body = serde_json::to_vec_pretty(file)?;
    body.push(b'\n');
    let temp = parent.join(format!(
        ".secrets.{}.{}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut out = options.open(&temp)?;
        out.write_all(&body)?;
        out.sync_all()?;
        fs::rename(&temp, &path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn validate_account(account: &str) -> Result<()> {
    if account.is_empty()
        || !account
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("invalid credential account name: {account:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keychain_reference() {
        assert_eq!(
            account_from_reference("keychain:openrouter"),
            Some("openrouter")
        );
        assert_eq!(account_from_reference("OPENROUTER_API_KEY"), None);
        assert_eq!(account_from_reference("keychain:"), None);
    }

    #[test]
    fn validates_account_names() {
        assert!(validate_account("openai-main_1").is_ok());
        assert!(validate_account("../secret").is_err());
        assert!(validate_account("").is_err());
    }
}
