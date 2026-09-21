//! Turn a picker selection into the upstream `[model.<key>]` entry.
//!
//! The field names and semantics mirror `ModelEntryConfig` in
//! `crates/codegen/xai-grok-shell/src/agent/config.rs` (`model`, `base_url`, `name`,
//! `api_backend`, `auth_scheme`, `env_key`, `extra_headers`, `context_window`, …). Secrets never
//! appear here: a saved credential is exposed as an `env_key` name that the M0 overlay sets in the
//! process environment from the broker before the shell reads its config, or the overlay passes
//! the resolved [`crate::sampler::sampler_config_for`] directly for an in-memory turn.

use std::collections::BTreeMap;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use xai_grok_sampler::AuthScheme;
use xai_grok_sampling_types::ApiBackend;

use crate::broker::{CredentialBroker, CredentialRef, ProviderError};
use crate::catalog::CatalogModel;
use crate::manifest::{AuthHeader, manifest};
use crate::sampler::{api_backend_for, auth_scheme_for};

/// Context window used when the catalog does not state one (upstream requires the field).
pub const FALLBACK_CONTEXT_WINDOW: u64 = 128_000;

/// Environment variable the overlay populates from the broker for a saved (keyring/file) secret.
pub fn workshop_env_key(provider_id: &str) -> String {
    format!(
        "WORKSHOP_{}_API_KEY",
        provider_id.to_ascii_uppercase().replace(['-', '.'], "_")
    )
}

/// Where the overlay must obtain the credential before the shell reads `[model.<key>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialInjection {
    /// No credential at all (local servers, Kilo `:free`).
    #[default]
    None,
    /// The user's own environment variable already carries the key.
    ProcessEnv { var: String },
    /// Read from Workshop's broker (keyring or fallback file) and export as `var` for this process.
    FromBroker { provider_id: String, var: String },
}

/// The `[model.<key>]` fields the overlay writes or feeds to the shell. Serializes with upstream's
/// field names so the JSON/TOML form round-trips into `ModelEntryConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntrySpec {
    /// Stable catalog id (`provider:model[:variant]`).
    pub id: String,
    /// Routing slug sent to the API.
    pub model: String,
    pub base_url: String,
    pub name: String,
    pub api_backend: ApiBackend,
    pub auth_scheme: AuthScheme,
    /// Env var names, first set wins (upstream `EnvKeys`). Empty for anonymous/local rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_key: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    pub context_window: NonZeroU64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    /// Upstream-only flag for the xAI proxy; always unset for BYOK endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
    /// Not an upstream field: how the overlay obtains the credential.
    #[serde(skip)]
    pub credential: CredentialInjection,
}

impl ModelEntrySpec {
    /// Config key for `[model.<key>]`: `provider-model` with characters TOML keys accept.
    pub fn config_key(&self) -> String {
        self.id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    }

    /// The `[model.<key>]` table as TOML (no secrets; `env_key` names only).
    pub fn to_toml(&self) -> String {
        let mut out = format!("[model.{}]\n", quote_key(&self.config_key()));
        out.push_str(&format!("model = {}\n", toml_str(&self.model)));
        out.push_str(&format!("base_url = {}\n", toml_str(&self.base_url)));
        out.push_str(&format!("name = {}\n", toml_str(&self.name)));
        out.push_str(&format!(
            "api_backend = {}\n",
            toml_str(match self.api_backend {
                ApiBackend::ChatCompletions => "chat_completions",
                ApiBackend::Responses => "responses",
                ApiBackend::Messages => "messages",
            })
        ));
        out.push_str(&format!(
            "auth_scheme = {}\n",
            toml_str(match self.auth_scheme {
                AuthScheme::Bearer => "bearer",
                AuthScheme::XApiKey => "x_api_key",
            })
        ));
        match self.env_key.len() {
            0 => {}
            1 => out.push_str(&format!("env_key = {}\n", toml_str(&self.env_key[0]))),
            _ => out.push_str(&format!(
                "env_key = [{}]\n",
                self.env_key
                    .iter()
                    .map(|k| toml_str(k))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
        out.push_str(&format!("context_window = {}\n", self.context_window));
        if let Some(m) = self.max_completion_tokens {
            out.push_str(&format!("max_completion_tokens = {m}\n"));
        }
        if !self.extra_headers.is_empty() {
            out.push_str(&format!(
                "\n[model.{}.extra_headers]\n",
                quote_key(&self.config_key())
            ));
            for (k, v) in &self.extra_headers {
                out.push_str(&format!("{} = {}\n", quote_key(k), toml_str(v)));
            }
        }
        out
    }
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn quote_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        k.to_string()
    } else {
        toml_str(k)
    }
}

/// Resolve `model` into a `[model.<key>]` spec using the broker's view of the credential.
pub fn resolve_model_entry(
    model: &CatalogModel,
    broker: &CredentialBroker,
) -> Result<ModelEntrySpec, ProviderError> {
    let m = manifest(&model.provider_id)
        .ok_or_else(|| ProviderError::UnknownProvider(model.provider_id.clone()))?;
    let (env_key, credential) = if m.auth == AuthHeader::None {
        (Vec::new(), CredentialInjection::None)
    } else {
        match broker.credential_ref(&model.provider_id)? {
            CredentialRef::None if !m.requires_credential() => {
                (Vec::new(), CredentialInjection::None)
            }
            CredentialRef::None => {
                return Err(ProviderError::NoCredential {
                    provider: model.provider_id.clone(),
                });
            }
            CredentialRef::Env { var } => {
                (vec![var.clone()], CredentialInjection::ProcessEnv { var })
            }
            CredentialRef::Keyring | CredentialRef::File => {
                let var = workshop_env_key(&model.provider_id);
                (
                    vec![var.clone()],
                    CredentialInjection::FromBroker {
                        provider_id: model.provider_id.clone(),
                        var,
                    },
                )
            }
        }
    };
    let extra_headers: BTreeMap<String, String> = m.required_headers.iter().cloned().collect();
    let context_window = NonZeroU64::new(model.context_window.unwrap_or(FALLBACK_CONTEXT_WINDOW))
        .unwrap_or_else(|| NonZeroU64::new(FALLBACK_CONTEXT_WINDOW).expect("non-zero"));
    Ok(ModelEntrySpec {
        id: model.key(),
        model: model.model_id.clone(),
        base_url: model.base_url.clone(),
        name: model.display_name.clone(),
        api_backend: api_backend_for(model.protocol),
        auth_scheme: auth_scheme_for(m.auth),
        env_key,
        extra_headers,
        context_window,
        max_completion_tokens: None,
        stream_tool_calls: None,
        credential,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Catalog, custom_row};
    use crate::secrets::MemorySecretStore;
    use std::sync::Arc;

    fn broker(tmp: &tempfile::TempDir) -> CredentialBroker {
        CredentialBroker::new(
            Arc::new(MemorySecretStore::default()),
            tmp.path().join("connections.json"),
        )
    }

    #[test]
    fn kilo_free_needs_no_credential_and_renders_upstream_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = broker(&tmp);
        let cat = Catalog::builtin();
        let row = cat.get("kilo:kilo-auto/free").unwrap();
        let spec = resolve_model_entry(row, &broker).unwrap();
        assert_eq!(spec.model, "kilo-auto/free");
        assert_eq!(spec.base_url, "https://api.kilo.ai/api/gateway");
        assert_eq!(spec.api_backend, ApiBackend::ChatCompletions);
        assert_eq!(spec.auth_scheme, AuthScheme::Bearer);
        assert!(spec.env_key.is_empty());
        assert_eq!(spec.credential, CredentialInjection::None);
        assert_eq!(spec.context_window.get(), FALLBACK_CONTEXT_WINDOW);
        let toml = spec.to_toml();
        assert!(toml.starts_with("[model.kilo-kilo-auto-free]\n"), "{toml}");
        assert!(toml.contains("model = \"kilo-auto/free\""));
        assert!(toml.contains("api_backend = \"chat_completions\""));
        assert!(
            !toml.contains("env_key"),
            "anonymous rows have no key: {toml}"
        );
        // The JSON form uses upstream's field names.
        let json = serde_json::to_value(&spec).unwrap();
        for field in [
            "model",
            "base_url",
            "name",
            "api_backend",
            "auth_scheme",
            "context_window",
        ] {
            assert!(json.get(field).is_some(), "{field}");
        }
        assert_eq!(json["api_backend"], "chat_completions");
        assert_eq!(json["auth_scheme"], "bearer");
    }

    #[test]
    fn saved_key_becomes_a_workshop_env_key_never_a_value() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = broker(&tmp);
        broker.save_api_key("anthropic", "sk-ant-secret").unwrap();
        let row = custom_row("anthropic", "claude-sonnet-4-5").unwrap();
        let spec = resolve_model_entry(&row, &broker).unwrap();
        assert_eq!(spec.env_key, vec!["WORKSHOP_ANTHROPIC_API_KEY"]);
        assert_eq!(
            spec.credential,
            CredentialInjection::FromBroker {
                provider_id: "anthropic".into(),
                var: "WORKSHOP_ANTHROPIC_API_KEY".into()
            }
        );
        assert_eq!(spec.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(spec.api_backend, ApiBackend::Messages);
        assert_eq!(
            spec.extra_headers
                .get("anthropic-version")
                .map(String::as_str),
            Some("2023-06-01")
        );
        let toml = spec.to_toml();
        assert!(!toml.contains("sk-ant-secret"));
        assert!(toml.contains("env_key = \"WORKSHOP_ANTHROPIC_API_KEY\""));
        assert!(toml.contains("[model.anthropic-claude-sonnet-4-5.extra_headers]\nanthropic-version = \"2023-06-01\""), "{toml}");
        assert!(!toml.contains("api_key ="), "secrets never land in config");
    }

    #[test]
    fn env_key_from_the_users_shell_is_kept_as_is() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = broker(&tmp);
        broker.use_env_key("google").unwrap();
        let row = Catalog::builtin()
            .get("google:gemini-3.5-flash")
            .unwrap()
            .clone();
        let spec = resolve_model_entry(&row, &broker).unwrap();
        assert_eq!(spec.env_key, vec!["GEMINI_API_KEY"]);
        assert_eq!(
            spec.credential,
            CredentialInjection::ProcessEnv {
                var: "GEMINI_API_KEY".into()
            }
        );
        assert_eq!(
            spec.base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }

    #[test]
    fn unconnected_keyed_provider_is_an_error_and_local_is_anonymous() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = broker(&tmp);
        let row = Catalog::builtin()
            .get("openrouter:openrouter/free")
            .unwrap()
            .clone();
        assert!(matches!(
            resolve_model_entry(&row, &broker),
            Err(ProviderError::NoCredential { .. })
        ));
        let mut local = custom_row("ollama", "qwen2.5-coder:7b").unwrap();
        local.context_window = Some(32_768);
        let spec = resolve_model_entry(&local, &broker).unwrap();
        assert_eq!(spec.credential, CredentialInjection::None);
        assert_eq!(spec.context_window.get(), 32_768);
        assert_eq!(spec.config_key(), "ollama-qwen2-5-coder-7b");
        assert!(
            spec.to_toml()
                .contains("base_url = \"http://127.0.0.1:11434/v1\"")
        );
    }
}
