//! Write a picker selection into `$WORKSHOP_HOME/config.toml` as the active `[model.<key>]`.
//!
//! The entry uses upstream `ModelEntryConfig` field names (via
//! [`workshop_providers::ModelEntrySpec`]). Secrets never land here: saved keys are referenced by
//! their `WORKSHOP_<PROVIDER>_API_KEY` env name, which the host exports from the broker before the
//! shell reloads its model list. Rows that need no credential (local servers, Kilo `:free`) get the
//! shell's anonymous sentinel so the non-interactive method is advertised and **no**
//! `Authorization` header is ever sent.

use std::path::Path;

use workshop_providers::{CredentialInjection, ModelEntrySpec};

/// Mirrors `xai_grok_shell::agent::config::WORKSHOP_ANONYMOUS_API_KEY` (the shell is not a
/// dependency of this crate; the gates pin the two constants equal).
pub const ANONYMOUS_API_KEY_SENTINEL: &str = "workshop-anonymous";

#[derive(Debug, thiserror::Error)]
pub enum ConfigWriteError {
    #[error("read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// The `[model.<key>]` table for `spec` as a TOML value (no secrets).
pub fn model_table(spec: &ModelEntrySpec) -> toml::Table {
    let mut t = toml::Table::new();
    t.insert("model".into(), toml::Value::String(spec.model.clone()));
    t.insert(
        "base_url".into(),
        toml::Value::String(spec.base_url.clone()),
    );
    t.insert("name".into(), toml::Value::String(spec.name.clone()));
    t.insert(
        "api_backend".into(),
        toml::Value::String(
            match spec.api_backend {
                xai_grok_sampling_types::ApiBackend::ChatCompletions => "chat_completions",
                xai_grok_sampling_types::ApiBackend::Responses => "responses",
                xai_grok_sampling_types::ApiBackend::Messages => "messages",
            }
            .into(),
        ),
    );
    t.insert(
        "auth_scheme".into(),
        toml::Value::String(
            match spec.auth_scheme {
                xai_grok_sampler::AuthScheme::Bearer => "bearer",
                xai_grok_sampler::AuthScheme::XApiKey => "x_api_key",
            }
            .into(),
        ),
    );
    match &spec.credential {
        CredentialInjection::None => {
            t.insert(
                "api_key".into(),
                toml::Value::String(ANONYMOUS_API_KEY_SENTINEL.into()),
            );
        }
        CredentialInjection::ProcessEnv { var } | CredentialInjection::FromBroker { var, .. } => {
            t.insert("env_key".into(), toml::Value::String(var.clone()));
        }
    }
    t.insert(
        "context_window".into(),
        toml::Value::Integer(spec.context_window.get() as i64),
    );
    if let Some(m) = spec.max_completion_tokens {
        t.insert(
            "max_completion_tokens".into(),
            toml::Value::Integer(m as i64),
        );
    }
    if !spec.extra_headers.is_empty() {
        let mut h = toml::Table::new();
        for (k, v) in &spec.extra_headers {
            h.insert(k.clone(), toml::Value::String(v.clone()));
        }
        t.insert("extra_headers".into(), toml::Value::Table(h));
    }
    t
}

/// Write a keyless placeholder model (neutral loopback base URL, anonymous sentinel) and make it
/// the shell default; return its config key. Engine/Adapter connections route turns through
/// workshop-adapters, not this model, but the shell needs a model + the non-interactive auth method
/// to open a session. A turn never reaches the placeholder: the pager intercepts prompts first.
pub fn activate_placeholder_session(path: &Path) -> Result<String, ConfigWriteError> {
    let spec = ModelEntrySpec {
        id: "workshop:connection".to_owned(),
        model: "workshop-connection".to_owned(),
        // The neutral sentinel host from patch 0003 (`neutral-production-endpoints`).
        base_url: "http://127.0.0.1:1".to_owned(),
        name: "Workshop connection".to_owned(),
        api_backend: xai_grok_sampling_types::ApiBackend::ChatCompletions,
        auth_scheme: xai_grok_sampler::AuthScheme::Bearer,
        env_key: Vec::new(),
        extra_headers: std::collections::BTreeMap::new(),
        context_window: std::num::NonZeroU64::new(8192).expect("nonzero"),
        max_completion_tokens: None,
        stream_tool_calls: None,
        credential: CredentialInjection::None,
    };
    activate_model(path, &spec)
}

/// Merge `spec` into the TOML document at `path` under `[model.<key>]`, set top-level
/// `[models] default = "<key>"`, and write it back atomically (0600). Other tables are untouched.
/// Returns the config key.
pub fn activate_model(path: &Path, spec: &ModelEntrySpec) -> Result<String, ConfigWriteError> {
    let display = path.display().to_string();
    let existing = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ConfigWriteError::Read {
                path: display,
                source,
            });
        }
    };
    let mut doc: toml::Table = if existing.trim().is_empty() {
        toml::Table::new()
    } else {
        existing.parse().map_err(|source| ConfigWriteError::Parse {
            path: display.clone(),
            source,
        })?
    };
    let key = spec.config_key();
    let models = doc
        .entry("model")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !models.is_table() {
        *models = toml::Value::Table(toml::Table::new());
    }
    if let Some(models) = models.as_table_mut() {
        models.insert(key.clone(), toml::Value::Table(model_table(spec)));
    }
    // The shell's active model is `[models] default` (`ModelsConfig::default`), which
    // `x.ai/internal/reload_models` re-reads; a top-level `default` key is not consulted.
    let models_cfg = doc
        .entry("models")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !models_cfg.is_table() {
        *models_cfg = toml::Value::Table(toml::Table::new());
    }
    if let Some(models_cfg) = models_cfg.as_table_mut() {
        models_cfg.insert("default".into(), toml::Value::String(key.clone()));
    }
    let rendered = toml::to_string_pretty(&doc).unwrap_or_else(|_| doc.to_string());
    let header =
        "# Written by the Workshop connection picker (/auth). Secrets are never stored here.\n";
    let body = if rendered.starts_with('#') {
        rendered
    } else {
        format!("{header}{rendered}")
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigWriteError::Write {
            path: display.clone(),
            source,
        })?;
    }
    workshop_providers::atomic_write_private(path, body.as_bytes()).map_err(|e| {
        ConfigWriteError::Write {
            path: display,
            source: std::io::Error::other(e.to_string()),
        }
    })?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use workshop_providers::{Catalog, CredentialBroker, MemorySecretStore, resolve_model_entry};

    fn broker(tmp: &tempfile::TempDir) -> CredentialBroker {
        CredentialBroker::new(
            Arc::new(MemorySecretStore::default()),
            tmp.path().join("connections.json"),
        )
    }

    #[test]
    fn kilo_free_writes_anonymous_sentinel_and_default() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = Catalog::builtin();
        let row = cat.get("kilo:kilo-auto/free").unwrap();
        let spec = resolve_model_entry(row, &broker(&tmp)).unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[features]\ntelemetry = false\n").unwrap();
        let key = activate_model(&path, &spec).unwrap();
        assert_eq!(key, "kilo-kilo-auto-free");
        let doc: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["models"]["default"].as_str(),
            Some("kilo-kilo-auto-free"),
            "the shell reads [models] default, not a top-level key"
        );
        assert!(doc.get("default").is_none());
        let m = &doc["model"]["kilo-kilo-auto-free"];
        assert_eq!(m["model"].as_str(), Some("kilo-auto/free"));
        assert_eq!(
            m["base_url"].as_str(),
            Some("https://api.kilo.ai/api/gateway")
        );
        assert_eq!(m["api_key"].as_str(), Some(ANONYMOUS_API_KEY_SENTINEL));
        assert!(m.get("env_key").is_none());
        assert_eq!(
            doc["features"]["telemetry"].as_bool(),
            Some(false),
            "other tables kept"
        );
    }

    #[test]
    fn saved_key_is_referenced_by_env_name_only() {
        let tmp = tempfile::tempdir().unwrap();
        let b = broker(&tmp);
        b.save_api_key("anthropic", "sk-ant-secret").unwrap();
        let row =
            workshop_providers::catalog::custom_row("anthropic", "claude-sonnet-4-5").unwrap();
        let spec = resolve_model_entry(&row, &b).unwrap();
        let path = tmp.path().join("config.toml");
        activate_model(&path, &spec).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sk-ant-secret"));
        assert!(text.contains("env_key = \"WORKSHOP_ANTHROPIC_API_KEY\""));
        assert!(text.contains("anthropic-version"));
        assert!(!text.contains(ANONYMOUS_API_KEY_SENTINEL));
    }
}
