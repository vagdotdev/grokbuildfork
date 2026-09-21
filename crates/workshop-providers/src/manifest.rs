//! Typed provider manifests.
//!
//! Endpoints and headers verified against vendor documentation on 2026-09-21:
//! * OpenAI: `https://api.openai.com/v1` — Responses (`/responses`) and Chat Completions
//!   (`/chat/completions`), `Authorization: Bearer`.
//! * Anthropic: `https://api.anthropic.com/v1/messages`, `x-api-key`, and the required
//!   `anthropic-version: 2023-06-01` header. API key only; no Claude Pro/Max OAuth.
//! * OpenRouter: `https://openrouter.ai/api/v1/chat/completions`, `Authorization: Bearer`;
//!   optional attribution headers `HTTP-Referer` / `X-Title`.
//! * OpenCode Zen: `https://opencode.ai/zen/v1` — `/chat/completions`, `/responses`, or
//!   `/messages` depending on the model; `Authorization: Bearer` with a Zen API key
//!   (`OPENCODE_API_KEY`, minted at `opencode.ai/auth`). Zen's keyless free tier is gated to the
//!   genuine `opencode` client (see `internal/opencode-free-models-proof.md`), so Workshop models
//!   Zen as a **key-required** provider: the picker shows "Sign in to OpenCode Zen", and the
//!   zero-price rows only become selectable once a key is connected. Free models without a key
//!   are reachable only through the `opencode` agent adapter.
//! * Local servers speak OpenAI Chat Completions on loopback: Ollama `:11434/v1`, LM Studio
//!   `:1234/v1`, llama.cpp server `:8080/v1`, vLLM `:8000/v1`.

use serde::{Deserialize, Serialize};

/// The connection class, always shown to the user next to a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// Workshop owns prompt, tools, loop, and HTTP; the key was issued for API use.
    Direct,
    /// Same, on loopback, with no or a local token.
    Local,
}

impl ProviderClass {
    pub fn label(self) -> &'static str {
        match self {
            ProviderClass::Direct => "Direct API",
            ProviderClass::Local => "Local",
        }
    }
}

/// Wire protocol. Exactly the three the sampler implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    ChatCompletions,
    Responses,
    Messages,
}

impl Protocol {
    /// Path appended to the provider base URL.
    pub fn path(self) -> &'static str {
        match self {
            Protocol::ChatCompletions => "/chat/completions",
            Protocol::Responses => "/responses",
            Protocol::Messages => "/messages",
        }
    }
}

/// Where the credential goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthHeader {
    Bearer,
    XApiKey,
    None,
}

/// Where the credential comes from. Never a plaintext Workshop file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CredentialSource {
    /// Process environment variable; detection reports presence only, never the value.
    Env { var: String },
    /// Saved by the user in the OS keyring (`Save securely`).
    Keyring,
    /// No credential (local servers).
    None,
}

/// Where the model list comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelCatalogSource {
    /// Rows shipped in this crate.
    Builtin,
    /// `GET {base_url}/models` on the provider.
    ModelsEndpoint,
    /// Reviewed Models.dev-compatible metadata with a shipped fallback.
    ModelsDev { url: String },
}

/// Static, compiled-in data: serializable for the review screen and logs, never deserialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderManifest {
    pub id: &'static str,
    pub display_name: &'static str,
    pub class: ProviderClass,
    /// Default protocol for rows that do not override it.
    pub protocol: Protocol,
    /// Base URL up to and including the API version segment, no trailing slash.
    pub base_url: &'static str,
    /// Hosts a credential for this provider may ever be sent to.
    pub allowed_hosts: &'static [&'static str],
    pub auth: AuthHeader,
    /// Headers every request must carry (name, value).
    pub required_headers: &'static [(&'static str, &'static str)],
    /// Preferred credential source; the broker may substitute `Keyring` after `Save securely`.
    pub credential: CredentialSource,
    pub model_catalog_source: ModelCatalogSource,
    pub docs_url: &'static str,
    /// Plaintext HTTP allowed without confirmation (loopback only).
    pub loopback_only: bool,
    /// Connect action shown while the provider has no credential, e.g. "Sign in to OpenCode Zen".
    pub connect_copy: Option<&'static str>,
    /// Where the user mints the key the connect action asks for.
    pub credential_url: Option<&'static str>,
}

impl ProviderManifest {
    pub fn endpoint(&self, protocol: Protocol) -> String {
        format!("{}{}", self.base_url, protocol.path())
    }

    pub fn is_local(&self) -> bool {
        self.class == ProviderClass::Local
    }

    /// Whether requests need a credential at all.
    pub fn requires_credential(&self) -> bool {
        self.auth != AuthHeader::None
    }
}

/// All built-in provider manifests, in picker order (Direct API first, then Local).
pub fn builtin_manifests() -> Vec<ProviderManifest> {
    vec![
        ProviderManifest {
            id: "openai",
            display_name: "OpenAI",
            class: ProviderClass::Direct,
            protocol: Protocol::Responses,
            base_url: "https://api.openai.com/v1",
            allowed_hosts: &["api.openai.com"],
            auth: AuthHeader::Bearer,
            required_headers: &[],
            credential: CredentialSource::Env {
                var: "OPENAI_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://platform.openai.com/docs/api-reference",
            loopback_only: false,
            connect_copy: Some("Add an OpenAI API key"),
            credential_url: Some("https://platform.openai.com/api-keys"),
        },
        ProviderManifest {
            id: "anthropic",
            display_name: "Anthropic",
            class: ProviderClass::Direct,
            protocol: Protocol::Messages,
            base_url: "https://api.anthropic.com/v1",
            allowed_hosts: &["api.anthropic.com"],
            auth: AuthHeader::XApiKey,
            required_headers: &[("anthropic-version", "2023-06-01")],
            credential: CredentialSource::Env {
                var: "ANTHROPIC_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://docs.anthropic.com/en/api/messages",
            loopback_only: false,
            connect_copy: Some("Add an Anthropic API key"),
            credential_url: Some("https://console.anthropic.com/settings/keys"),
        },
        ProviderManifest {
            id: "openrouter",
            display_name: "OpenRouter",
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://openrouter.ai/api/v1",
            allowed_hosts: &["openrouter.ai"],
            auth: AuthHeader::Bearer,
            required_headers: &[("X-Title", "Workshop")],
            credential: CredentialSource::Env {
                var: "OPENROUTER_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://openrouter.ai/docs/api-reference/overview",
            loopback_only: false,
            connect_copy: Some("Add an OpenRouter API key"),
            credential_url: Some("https://openrouter.ai/settings/keys"),
        },
        ProviderManifest {
            id: "opencode",
            display_name: "OpenCode Zen",
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://opencode.ai/zen/v1",
            allowed_hosts: &["opencode.ai"],
            auth: AuthHeader::Bearer,
            required_headers: &[],
            credential: CredentialSource::Env {
                var: "OPENCODE_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsDev {
                url: "https://models.opencode.ai/api.json".into(),
            },
            docs_url: "https://opencode.ai/docs/zen/",
            loopback_only: false,
            connect_copy: Some("Sign in to OpenCode Zen"),
            credential_url: Some("https://opencode.ai/auth"),
        },
        ProviderManifest {
            id: "ollama",
            display_name: "Ollama",
            class: ProviderClass::Local,
            protocol: Protocol::ChatCompletions,
            base_url: "http://127.0.0.1:11434/v1",
            allowed_hosts: &["127.0.0.1", "localhost", "::1"],
            auth: AuthHeader::None,
            required_headers: &[],
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://github.com/ollama/ollama/blob/main/docs/openai.md",
            loopback_only: true,
            connect_copy: None,
            credential_url: None,
        },
        ProviderManifest {
            id: "lmstudio",
            display_name: "LM Studio",
            class: ProviderClass::Local,
            protocol: Protocol::ChatCompletions,
            base_url: "http://127.0.0.1:1234/v1",
            allowed_hosts: &["127.0.0.1", "localhost", "::1"],
            auth: AuthHeader::None,
            required_headers: &[],
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://lmstudio.ai/docs/app/api/endpoints/openai",
            loopback_only: true,
            connect_copy: None,
            credential_url: None,
        },
        ProviderManifest {
            id: "llamacpp",
            display_name: "llama.cpp",
            class: ProviderClass::Local,
            protocol: Protocol::ChatCompletions,
            base_url: "http://127.0.0.1:8080/v1",
            allowed_hosts: &["127.0.0.1", "localhost", "::1"],
            auth: AuthHeader::None,
            required_headers: &[],
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md",
            loopback_only: true,
            connect_copy: None,
            credential_url: None,
        },
        ProviderManifest {
            id: "vllm",
            display_name: "vLLM",
            class: ProviderClass::Local,
            protocol: Protocol::ChatCompletions,
            base_url: "http://127.0.0.1:8000/v1",
            allowed_hosts: &["127.0.0.1", "localhost", "::1"],
            auth: AuthHeader::None,
            required_headers: &[],
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint,
            docs_url: "https://docs.vllm.ai/en/latest/serving/openai_compatible_server.html",
            loopback_only: true,
            connect_copy: None,
            credential_url: None,
        },
    ]
}

/// Look up a built-in manifest by id.
pub fn manifest(id: &str) -> Option<ProviderManifest> {
    builtin_manifests().into_iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_providers_exist_with_expected_wire_shape() {
        let ids: Vec<&str> = builtin_manifests().iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            [
                "openai",
                "anthropic",
                "openrouter",
                "opencode",
                "ollama",
                "lmstudio",
                "llamacpp",
                "vllm"
            ]
        );
        let anthropic = manifest("anthropic").unwrap();
        assert_eq!(anthropic.protocol, Protocol::Messages);
        assert_eq!(anthropic.auth, AuthHeader::XApiKey);
        assert!(
            anthropic
                .required_headers
                .contains(&("anthropic-version", "2023-06-01"))
        );
        assert_eq!(
            anthropic.endpoint(Protocol::Messages),
            "https://api.anthropic.com/v1/messages"
        );
        assert!(
            matches!(anthropic.credential, CredentialSource::Env { ref var } if var == "ANTHROPIC_API_KEY")
        );

        let openai = manifest("openai").unwrap();
        assert_eq!(
            openai.endpoint(Protocol::Responses),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            openai.endpoint(Protocol::ChatCompletions),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(openai.auth, AuthHeader::Bearer);

        let zen = manifest("opencode").unwrap();
        assert_eq!(
            zen.endpoint(Protocol::ChatCompletions),
            "https://opencode.ai/zen/v1/chat/completions"
        );
        assert_eq!(zen.class, ProviderClass::Direct);
        assert!(
            zen.requires_credential(),
            "Zen is key-required from third-party clients"
        );
        assert_eq!(zen.connect_copy, Some("Sign in to OpenCode Zen"));
        assert_eq!(zen.credential_url, Some("https://opencode.ai/auth"));
        assert!(
            matches!(zen.model_catalog_source, ModelCatalogSource::ModelsDev { ref url } if url == "https://models.opencode.ai/api.json")
        );
    }

    #[test]
    fn local_providers_are_loopback_only_and_credential_free() {
        for m in builtin_manifests().into_iter().filter(|m| m.is_local()) {
            assert!(m.loopback_only, "{}", m.id);
            assert_eq!(m.auth, AuthHeader::None, "{}", m.id);
            assert_eq!(m.credential, CredentialSource::None, "{}", m.id);
            assert!(!m.requires_credential(), "{}", m.id);
            assert_eq!(m.connect_copy, None, "{}", m.id);
            assert!(m.base_url.starts_with("http://127.0.0.1:"), "{}", m.id);
            assert_eq!(m.protocol, Protocol::ChatCompletions, "{}", m.id);
        }
    }

    #[test]
    fn no_manifest_defaults_to_xai_or_grok() {
        for m in builtin_manifests() {
            assert!(
                !m.base_url.contains("x.ai") && !m.base_url.contains("grok.com"),
                "{}",
                m.id
            );
            for host in m.allowed_hosts {
                assert!(
                    !host.ends_with("x.ai") && !host.ends_with("grok.com"),
                    "{}",
                    m.id
                );
            }
        }
        assert!(
            manifest("xai").is_none(),
            "optional xAI is a separate, user-selected plugin"
        );
    }
}
