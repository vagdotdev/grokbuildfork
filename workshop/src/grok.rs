use crate::store::{Credential, CredentialMethod};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
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
    context_window: u64,
) -> Result<(PathBuf, String)> {
    let config_path = grok_config_path()?;
    install_openrouter_model_at(&config_path, executable, model, alias, context_window)
}

fn install_openrouter_model_at(
    config_path: &Path,
    executable: &Path,
    model: &str,
    alias: Option<&str>,
    context_window: u64,
) -> Result<(PathBuf, String)> {
    if model.trim().is_empty() || model.chars().any(char::is_control) {
        bail!("OpenRouter model ID cannot be empty or contain control characters");
    }
    let context_window =
        i64::try_from(context_window).context("model context window exceeds TOML integer range")?;
    if context_window <= 0 {
        bail!("model context window must be greater than zero");
    }
    let alias = alias
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_alias(model));
    if alias.chars().any(char::is_control) {
        bail!("model alias cannot contain control characters");
    }

    let lock = lock_config(config_path)?;
    let existing = match std::fs::read_to_string(config_path) {
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
    let executable = executable.to_string_lossy().into_owned();
    let auth_providers = table_section(&mut document, "auth_provider")?;
    let auth = named_table(auth_providers, auth_name, "auth_provider")?;
    if auth
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|command| command != executable)
    {
        bail!(
            "Grok config already defines [auth_provider.{auth_name}] with another command; refusing to overwrite it"
        );
    }
    auth["command"] = value(executable);
    let mut args = Array::new();
    for argument in ["auth", "token", "openrouter"] {
        args.push(argument);
    }
    auth["args"] = Item::Value(Value::Array(args));
    auth["token_ttl_secs"] = value(86_400);
    auth["timeout_secs"] = value(30);

    let model_providers = table_section(&mut document, "model_providers")?;
    let provider = named_table(model_providers, auth_name, "model_providers")?;
    for credential_or_route in [
        "api_base_url",
        "api_key",
        "env_key",
        "extra_headers",
        "query_params",
        "env_http_headers",
        "auth",
    ] {
        provider.remove(credential_or_route);
    }
    provider["base_url"] = value("https://openrouter.ai/api/v1");
    provider["api_backend"] = value("chat_completions");
    provider["auth_provider"] = value(auth_name);

    let models = table_section(&mut document, "model")?;
    if let Some(existing) = models.get(&alias) {
        let existing_table = existing
            .as_table()
            .with_context(|| format!("Grok config [model.{alias}] entry is not a table"))?;
        let existing_model = existing_table.get("model").and_then(Item::as_str);
        if existing_model != Some(model) {
            bail!(
                "Grok model alias {alias:?} already exists for {}; choose a different --alias",
                existing_model.unwrap_or("an unrecognized model entry")
            );
        }
        if existing_table.get("model_provider").and_then(Item::as_str) != Some(auth_name) {
            bail!(
                "Grok model alias {alias:?} is not owned by Workshop; choose a different --alias"
            );
        }
    }
    let model_table = named_table(models, &alias, "model")?;
    for credential_or_route in [
        "base_url",
        "api_base_url",
        "api_key",
        "env_key",
        "auth_provider",
        "auth_scheme",
        "api_backend",
        "extra_headers",
        "query_params",
        "env_http_headers",
    ] {
        model_table.remove(credential_or_route);
    }
    model_table["model"] = value(model);
    model_table["name"] = value(format!("OpenRouter · {model}"));
    model_table["model_provider"] = value(auth_name);
    model_table["context_window"] = value(context_window);

    let rendered = document.to_string();
    let write_result = write_private_atomic(config_path, rendered.as_bytes(), Some(&existing));
    let unlock_result =
        FileExt::unlock(&lock).with_context(|| format!("unlock {}", config_path.display()));
    write_result?;
    unlock_result?;
    Ok((config_path.to_owned(), alias))
}

pub fn grok_config_path() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        return Ok(PathBuf::from(home).join("config.toml"));
    }
    let base = directories::BaseDirs::new().context("determine home directory")?;
    Ok(base.home_dir().join(".grok").join("config.toml"))
}

fn table_section<'a>(document: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table> {
    if document.get(name).is_none() {
        document[name] = Item::Table(Table::new());
    }
    document[name]
        .as_table_mut()
        .with_context(|| format!("Grok config [{name}] section is not a table"))
}

fn named_table<'a>(section: &'a mut Table, key: &str, section_name: &str) -> Result<&'a mut Table> {
    if !section.contains_key(key) {
        section.insert(key, Item::Table(Table::new()));
    }
    section
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .with_context(|| format!("Grok config [{section_name}.{key}] entry is not a table"))
}

fn lock_config(config_path: &Path) -> Result<std::fs::File> {
    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let lock_path = parent.join(".workshop-config.lock");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    FileExt::lock_exclusive(&lock).with_context(|| format!("lock {}", lock_path.display()))?;
    Ok(lock)
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

fn write_private_atomic(path: &Path, contents: &[u8], expected: Option<&str>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
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
    if let Some(expected) = expected {
        let current = match std::fs::read_to_string(path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                return Err(error).with_context(|| format!("re-read {}", path.display()));
            }
        };
        if current != expected {
            let _ = std::fs::remove_file(&temp);
            bail!(
                "{} changed while Workshop was editing it; no changes were written, retry the command",
                path.display()
            );
        }
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error).with_context(|| format!("replace {}", path.display()));
    }
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
        let config = temp.path().join("config.toml");
        std::fs::write(&config, "[models]\ndefault = \"existing\"\n").unwrap();

        let result = install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "anthropic/claude-test",
            None,
            131_072,
        )
        .unwrap();

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
        assert_eq!(
            parsed["model"]["openrouter-anthropic-claude-test"]["context_window"].as_integer(),
            Some(131_072)
        );
    }

    #[test]
    fn rejects_malformed_managed_section_without_panicking() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, "auth_provider = \"not-a-table\"\n").unwrap();

        let error = install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            None,
            100_000,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("not a table"));
    }

    #[test]
    fn refuses_to_overwrite_an_existing_model_alias() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/first",
            Some("favorite"),
            100_000,
        )
        .unwrap();

        let error = install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/second",
            Some("favorite"),
            200_000,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("already exists"));
        let contents = std::fs::read_to_string(config).unwrap();
        assert!(contents.contains("author/first"));
        assert!(!contents.contains("author/second"));
    }

    #[test]
    fn reconfigure_preserves_non_routing_model_fields() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            Some("favorite"),
            100_000,
        )
        .unwrap();
        let mut document: DocumentMut = std::fs::read_to_string(&config).unwrap().parse().unwrap();
        document["model"]["favorite"]["max_completion_tokens"] = value(12_345);
        std::fs::write(&config, document.to_string()).unwrap();

        install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            Some("favorite"),
            200_000,
        )
        .unwrap();

        let parsed: DocumentMut = std::fs::read_to_string(config).unwrap().parse().unwrap();
        assert_eq!(
            parsed["model"]["favorite"]["max_completion_tokens"].as_integer(),
            Some(12_345)
        );
        assert_eq!(
            parsed["model"]["favorite"]["context_window"].as_integer(),
            Some(200_000)
        );
    }

    #[test]
    fn reconfigure_clears_old_route_and_credential_fields() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            Some("favorite"),
            100_000,
        )
        .unwrap();
        let mut document: DocumentMut = std::fs::read_to_string(&config).unwrap().parse().unwrap();
        document["model"]["favorite"]["base_url"] = value("https://old.example/v1");
        document["model"]["favorite"]["api_key"] = value("old-model-secret");
        document["model"]["favorite"]["extra_headers"]["X-Old-Secret"] = value("old-header-secret");
        document["model_providers"]["workshop-openrouter"]["env_http_headers"]["X-Token"] =
            value("OLD_TOKEN");
        std::fs::write(&config, document.to_string()).unwrap();

        install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            Some("favorite"),
            200_000,
        )
        .unwrap();

        let contents = std::fs::read_to_string(config).unwrap();
        assert!(!contents.contains("old.example"));
        assert!(!contents.contains("old-model-secret"));
        assert!(!contents.contains("old-header-secret"));
        assert!(!contents.contains("OLD_TOKEN"));
    }

    #[test]
    fn refuses_to_adopt_an_unrelated_same_model_alias() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.toml");
        std::fs::write(
            &config,
            "[model.favorite]\nmodel = \"author/model\"\nbase_url = \"https://other.example/v1\"\nauth_provider = \"other\"\n",
        )
        .unwrap();

        let error = install_openrouter_model_at(
            &config,
            Path::new("/usr/local/bin/workshop"),
            "author/model",
            Some("favorite"),
            100_000,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("not owned by Workshop"));
        let contents = std::fs::read_to_string(config).unwrap();
        assert!(contents.contains("https://other.example/v1"));
        assert!(!contents.contains("workshop-openrouter"));
    }
}
