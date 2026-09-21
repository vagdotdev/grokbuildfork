//! Mapping onto the existing sampler types. `xai-grok-sampler` is used as-is.

use indexmap::IndexMap;
use xai_grok_sampler::{AuthScheme, SamplerConfig};
use xai_grok_sampling_types::ApiBackend;

use crate::broker::{CredentialHandle, ProviderError};
use crate::catalog::CatalogModel;
use crate::manifest::{AuthHeader, Protocol, manifest};

pub fn api_backend_for(protocol: Protocol) -> ApiBackend {
    match protocol {
        Protocol::ChatCompletions => ApiBackend::ChatCompletions,
        Protocol::Responses => ApiBackend::Responses,
        Protocol::Messages => ApiBackend::Messages,
    }
}

/// The sampler has no "no auth" scheme; local rows get `Bearer` with `api_key: None`, which the
/// sampler sends as no `Authorization` header.
pub fn auth_scheme_for(auth: AuthHeader) -> AuthScheme {
    match auth {
        AuthHeader::Bearer | AuthHeader::None => AuthScheme::Bearer,
        AuthHeader::XApiKey => AuthScheme::XApiKey,
    }
}

/// Build a [`SamplerConfig`] for `model`, releasing the credential only after the handle has
/// authorized the model's endpoint host. Required headers (e.g. `anthropic-version`) are injected
/// here because the sampler applies `extra_headers` verbatim and never derives them.
pub fn sampler_config_for(
    model: &CatalogModel,
    credential: &CredentialHandle,
) -> Result<SamplerConfig, ProviderError> {
    if credential.provider_id() != model.provider_id {
        return Err(ProviderError::HostNotAllowed {
            provider: credential.provider_id().to_string(),
            host: model.base_url.clone(),
        });
    }
    let endpoint = model.endpoint();
    let api_key = credential.authorize(&endpoint)?.map(str::to_string);

    let mut extra_headers = IndexMap::new();
    if let Some(m) = manifest(&model.provider_id) {
        for (name, value) in m.required_headers {
            extra_headers.insert((*name).to_string(), (*value).to_string());
        }
    }

    Ok(SamplerConfig {
        api_key,
        base_url: model.base_url.clone(),
        model: model.model_id.clone(),
        api_backend: api_backend_for(model.protocol),
        auth_scheme: auth_scheme_for(credential.auth()),
        extra_headers,
        ..SamplerConfig::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::CredentialBroker;
    use crate::catalog::custom_row;
    use crate::secrets::MemorySecretStore;
    use std::sync::Arc;

    #[test]
    fn protocol_maps_onto_api_backend_variants() {
        assert_eq!(api_backend_for(Protocol::ChatCompletions), ApiBackend::ChatCompletions);
        assert_eq!(api_backend_for(Protocol::Responses), ApiBackend::Responses);
        assert_eq!(api_backend_for(Protocol::Messages), ApiBackend::Messages);
        assert_eq!(auth_scheme_for(AuthHeader::Bearer), AuthScheme::Bearer);
        assert_eq!(auth_scheme_for(AuthHeader::XApiKey), AuthScheme::XApiKey);
    }

    #[test]
    fn anthropic_config_carries_version_header_and_x_api_key() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = CredentialBroker::new(Arc::new(MemorySecretStore::default()), tmp.path().join("c.json"));
        broker.save_api_key("anthropic", "sk-ant-canary").unwrap();
        let handle = broker.resolve("anthropic").unwrap();
        let model = custom_row("anthropic", "claude-sonnet-4-5").unwrap();
        let cfg = sampler_config_for(&model, &handle).unwrap();
        assert_eq!(cfg.api_backend, ApiBackend::Messages);
        assert_eq!(cfg.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(cfg.base_url, "https://api.anthropic.com/v1");
        assert_eq!(cfg.model, "claude-sonnet-4-5");
        assert_eq!(cfg.api_key.as_deref(), Some("sk-ant-canary"));
        assert_eq!(cfg.extra_headers.get("anthropic-version").map(String::as_str), Some("2023-06-01"));
    }

    #[test]
    fn openai_and_local_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = CredentialBroker::new(Arc::new(MemorySecretStore::default()), tmp.path().join("c.json"));
        broker.save_api_key("openai", "sk-openai").unwrap();
        let model = custom_row("openai", "gpt-5").unwrap();
        let cfg = sampler_config_for(&model, &broker.resolve("openai").unwrap()).unwrap();
        assert_eq!(cfg.api_backend, ApiBackend::Responses);
        assert_eq!(cfg.auth_scheme, AuthScheme::Bearer);
        assert_eq!(cfg.api_key.as_deref(), Some("sk-openai"));

        broker.add_local("ollama").unwrap();
        let model = custom_row("ollama", "llama3").unwrap();
        let cfg = sampler_config_for(&model, &broker.resolve("ollama").unwrap()).unwrap();
        assert_eq!(cfg.api_backend, ApiBackend::ChatCompletions);
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.base_url, "http://127.0.0.1:11434/v1");
        assert!(cfg.extra_headers.is_empty());
    }

    #[test]
    fn a_credential_for_one_provider_never_configures_another() {
        let tmp = tempfile::tempdir().unwrap();
        let broker = CredentialBroker::new(Arc::new(MemorySecretStore::default()), tmp.path().join("c.json"));
        broker.save_api_key("openai", "sk-openai-canary").unwrap();
        let openai = broker.resolve("openai").unwrap();
        let openrouter_model = custom_row("openrouter", "openai/gpt-5").unwrap();
        assert!(matches!(
            sampler_config_for(&openrouter_model, &openai),
            Err(ProviderError::HostNotAllowed { .. })
        ));
    }
}
