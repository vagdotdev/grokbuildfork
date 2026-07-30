use crate::store::{Credential, CredentialMethod};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

pub fn token_json(credential: &Credential) -> Result<String> {
    let expires_in = credential
        .metadata
        .expires_at_ms
        .map(|expires| expires.saturating_sub(now_ms()) / 1000);
    if credential.metadata.method == CredentialMethod::OAuth && expires_in == Some(0) {
        bail!("stored OAuth credential has expired; sign in again");
    }
    let mut output = serde_json::Map::new();
    output.insert(
        "access_token".to_owned(),
        serde_json::Value::String(credential.access.clone()),
    );
    if let Some(refresh) = credential.refresh.as_ref() {
        output.insert(
            "refresh_token".to_owned(),
            serde_json::Value::String(refresh.clone()),
        );
    }
    if let Some(expires_in) = expires_in {
        output.insert(
            "expires_in".to_owned(),
            serde_json::Value::Number(expires_in.into()),
        );
    }
    serde_json::to_string(&output).context("serialize Grok credential output")
}

pub fn external_token_json(token: &str, lifetime_secs: u64) -> Result<String> {
    if token.trim().is_empty() {
        bail!("credential helper returned an empty token");
    }
    serde_json::to_string(&serde_json::json!({
        "access_token": token,
        "expires_in": lifetime_secs,
    }))
    .context("serialize external credential output")
}

pub fn install_openrouter_model(
    executable: &Path,
    model: &str,
    alias: Option<&str>,
) -> Result<(PathBuf, String)> {
    if model.trim().is_empty() || model.chars().any(char::is_control) {
        bail!("OpenRouter model ID cannot be empty or contain control characters");
    }
    let alias = alias
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_alias(model));
    if alias.chars().any(char::is_control) {
        bail!("model alias cannot contain control characters");
    }

    let config_path = grok_config_path()?;
    let existing = match std::fs::read_to_string(&config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", config_path.display()));
        }
    };
    let mut document = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing
            .parse::<DocumentMut>()
            .with_context(|| format!("parse {}", config_path.display()))?
    };

    let auth_name = "workshop-openrouter";
    let mut auth = Table::new();
    auth["command"] = value(executable.to_string_lossy().into_owned());
    let mut args = Array::new();
    for argument in ["auth", "token", "openrouter"] {
        args.push(argument);
    }
    auth["args"] = Item::Value(Value::Array(args));
    auth["token_ttl_secs"] = value(86_400);
    auth["timeout_secs"] = value(30);
    document["auth_provider"][auth_name] = Item::Table(auth);

    let mut provider = Table::new();
    provider["base_url"] = value("https://openrouter.ai/api/v1");
    provider["api_backend"] = value("chat_completions");
    provider["auth_provider"] = value(auth_name);
    document["model_providers"][auth_name] = Item::Table(provider);

    let mut model_table = Table::new();
    model_table["model"] = value(model);
    model_table["name"] = value(format!("OpenRouter · {model}"));
    model_table["model_provider"] = value(auth_name);
    document["model"][&alias] = Item::Table(model_table);

    write_private_atomic(&config_path, document.to_string().as_bytes())?;
    Ok((config_path, alias))
}

pub fn grok_config_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home).join("config.toml"));
    }
    let base = directories::BaseDirs::new().context("determine home directory")?;
    Ok(base.home_dir().join(".grok").join("config.toml"))
}

fn default_alias(model: &str) -> String {
    let mut alias = String::from("openrouter-");
    let mut last_was_dash = false;
    for character in model.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_') {
            alias.push(character.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            alias.push('-');
            last_was_dash = true;
        }
    }
    while alias.ends_with('-') {
        alias.pop();
    }
    alias
}

fn write_private_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("Grok config path has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set private permissions on {}", parent.display()))?;
    }
    let temp = parent.join(format!(".config.toml.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .with_context(|| format!("create {}", temp.display()))?;
    use std::io::Write;
    file.write_all(contents)
        .with_context(|| format!("write {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temp.display()))?;
    drop(file);
    std::fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CredentialMetadata, CredentialMethod};
    use std::collections::BTreeMap;

    #[test]
    fn grok_output_never_invents_an_expiry_for_api_keys() {
        let credential = Credential {
            metadata: CredentialMetadata {
                method: CredentialMethod::ApiKey,
                expires_at_ms: None,
                extra: BTreeMap::new(),
            },
            access: "secret".to_owned(),
            refresh: None,
        };
        let output: serde_json::Value =
            serde_json::from_str(&token_json(&credential).unwrap()).unwrap();
        assert_eq!(output["access_token"], "secret");
        assert!(output.get("expires_in").is_none());
    }

    #[test]
    fn installs_openrouter_without_overwriting_unrelated_config() {
        let temp = tempfile::tempdir().unwrap();
        let old_home = std::env::var_os("GROK_HOME");
        unsafe {
            std::env::set_var("GROK_HOME", temp.path());
        }
        std::fs::write(
            temp.path().join("config.toml"),
            "[models]\ndefault = \"existing\"\n",
        )
        .unwrap();

        let result = install_openrouter_model(
            Path::new("/usr/local/bin/workshop"),
            "anthropic/claude-test",
            None,
        )
        .unwrap();

        if let Some(old_home) = old_home {
            unsafe {
                std::env::set_var("GROK_HOME", old_home);
            }
        } else {
            unsafe {
                std::env::remove_var("GROK_HOME");
            }
        }
        let contents = std::fs::read_to_string(result.0).unwrap();
        let parsed: toml_edit::DocumentMut = contents.parse().unwrap();
        assert_eq!(parsed["models"]["default"].as_str(), Some("existing"));
        assert_eq!(
            parsed["model"]["openrouter-anthropic-claude-test"]["model"].as_str(),
            Some("anthropic/claude-test")
        );
        assert_eq!(
            parsed["auth_provider"]["workshop-openrouter"]["command"].as_str(),
            Some("/usr/local/bin/workshop")
        );
    }
}

