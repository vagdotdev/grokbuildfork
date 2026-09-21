//! Typed provider manifests.
//!
//! Endpoints, limits, and terms verified against vendor documentation and live probes on
//! 2026-09-21 (`internal/free-model-providers-survey.md`, `internal/opencode-free-models-proof.md`):
//!
//! * **Kilo Gateway** `https://api.kilo.ai/api/gateway` — OpenAI Chat Completions + SSE + tools.
//!   Documented anonymous access for `:free` models, 200 requests/hour per IP; prompts may be
//!   logged or trained on. The only documented keyless hosted pool a third-party client may use.
//! * **OpenRouter** `https://openrouter.ai/api/v1` — key or documented OAuth PKCE
//!   (`openrouter.ai/auth` → `POST /api/v1/auth/keys`). `:free` rows: 20 RPM, 50 requests/day
//!   until $10 lifetime credits; free endpoints need the account's privacy opt-in.
//! * **Google AI Studio** `https://generativelanguage.googleapis.com/v1beta/openai` — documented
//!   OpenAI-compatible endpoint; Gemini Flash / Flash-Lite free tier (~250 requests/day, Flash
//!   only); content used to improve Google products.
//! * **NVIDIA build.nvidia.com** `https://integrate.api.nvidia.com/v1` — trial tier, ~40 RPM,
//!   requests logged to improve NVIDIA products; prototyping/dev use.
//! * **OpenAI**, **Anthropic** (API key only; `anthropic-version` pinned), **OpenCode Zen**
//!   (`oc_sk_` key for paid models and Go; the keyless free tier is client-locked to the genuine
//!   `opencode` binary and keyed free access is unconfirmed, so Zen `*-free` rows are adapter-only).
//! * **Local** OpenAI-compatible servers on loopback, zero key: Ollama `:11434/v1`, LM Studio
//!   `:1234/v1`, llama.cpp `:8080/v1`, vLLM `:8000/v1`.
//! * **Custom** OpenAI-compatible / Anthropic-compatible BYOK endpoints the user types in.

use serde::{Deserialize, Serialize};

use crate::catalog::FreeTier;

/// The connection class, always shown to the user next to a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderClass {
    /// Workshop owns prompt, tools, loop, and HTTP; the key (if any) was issued for API use.
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

/// Where the credential comes from. Never a plaintext Workshop file unless the OS keyring is
/// unavailable, and then only the owner-only fallback under the Workshop home.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CredentialSource {
    /// Process environment variable; detection reports presence only, never the value.
    Env { var: String },
    /// Saved by the user in the OS keyring (`Save securely`).
    Keyring,
    /// Saved in the 0600 fallback file under `$WORKSHOP_HOME/secrets/` because no keyring was
    /// available.
    File,
    /// No credential (local servers, anonymous free pools).
    None,
}

/// How a user connects a provider. Order in the manifest is the order the picker offers them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum AuthMethod {
    /// Works with no credential at all (Kilo `:free`, local servers).
    Anonymous,
    /// Paste a key minted on the vendor's dashboard.
    ApiKey,
    /// Vendor-documented OAuth PKCE that mints a key for a third-party client (OpenRouter).
    OAuthPkce,
}

/// Where the model list comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelCatalogSource {
    /// Rows shipped in this crate only.
    Builtin,
    /// `GET {url}` in the provider's own list format (keyless or keyed as noted).
    ModelsEndpoint { url: String, keyless: bool },
    /// Reviewed Models.dev-compatible metadata with a shipped fallback.
    ModelsDev { url: String },
}

/// What happens to prompts sent to this provider, shown as a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataBadge {
    /// Prompts may be logged or used for training.
    MayTrain,
    /// Vendor documents no training / zero data retention.
    NoTraining,
    /// Never leaves the machine.
    Local,
    /// Depends on the account or model; see the vendor's terms.
    Unknown,
}

impl DataBadge {
    pub fn label(self) -> &'static str {
        match self {
            DataBadge::MayTrain => "may log/train",
            DataBadge::NoTraining => "no training",
            DataBadge::Local => "local",
            DataBadge::Unknown => "see terms",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub id: String,
    pub display_name: String,
    pub class: ProviderClass,
    /// Default protocol for rows that do not override it.
    pub protocol: Protocol,
    /// Base URL up to and including the API version segment, no trailing slash.
    pub base_url: String,
    /// Hosts a credential for this provider may ever be sent to.
    pub allowed_hosts: Vec<String>,
    pub auth: AuthHeader,
    /// Connect methods in picker order.
    pub auth_methods: Vec<AuthMethod>,
    /// Headers every request must carry (name, value).
    pub required_headers: Vec<(String, String)>,
    /// Preferred credential source; the broker substitutes `Keyring`/`File` after a save.
    pub credential: CredentialSource,
    pub model_catalog_source: ModelCatalogSource,
    pub docs_url: String,
    /// Plaintext HTTP allowed without confirmation (loopback only).
    pub loopback_only: bool,
    /// How this provider's zero-price rows are reached from Workshop's own client.
    pub free_tier: FreeTier,
    /// Connect action shown while the provider has no credential, e.g. "Sign in to OpenCode Zen".
    pub connect_copy: Option<String>,
    /// Where the user mints the key the connect action asks for.
    pub credential_url: Option<String>,
    /// Terms / logging caveat surfaced next to the rows.
    pub terms_caveat: Option<String>,
    /// Rate-limit hint surfaced in the picker and in 429 handling.
    pub rate_limit_hint: Option<String>,
    pub data_badge: DataBadge,
}

impl ProviderManifest {
    pub fn endpoint(&self, protocol: Protocol) -> String {
        format!("{}{}", self.base_url, protocol.path())
    }

    pub fn is_local(&self) -> bool {
        self.class == ProviderClass::Local
    }

    /// Whether requests need a credential at all. Kilo's `:free` rows and local servers do not.
    pub fn requires_credential(&self) -> bool {
        self.auth != AuthHeader::None && !self.auth_methods.contains(&AuthMethod::Anonymous)
    }

    /// Whether the provider can also take a key (paid rows or a personal quota).
    pub fn accepts_key(&self) -> bool {
        self.auth_methods
            .iter()
            .any(|m| matches!(m, AuthMethod::ApiKey | AuthMethod::OAuthPkce))
    }

    pub fn supports_pkce(&self) -> bool {
        self.auth_methods.contains(&AuthMethod::OAuthPkce)
    }

    /// Picker badge line, e.g. `Free · No sign-in · Shared pool · may log/train`.
    pub fn badge_line(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        match self.free_tier {
            FreeTier::Keyless => parts.extend(["Free", "No sign-in", "Shared pool"]),
            FreeTier::KeyRequired if self.supports_pkce() => parts.extend(["Free", "Sign in"]),
            FreeTier::KeyRequired => parts.extend(["Free tier", "API key"]),
            FreeTier::None if self.is_local() => parts.extend(["Free", "Local", "Offline"]),
            FreeTier::None => parts.push(self.class.label()),
        }
        parts.push(self.data_badge.label());
        parts.join(" · ")
    }

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn local(id: &str, display_name: &str, base_url: &str, docs_url: &str) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            class: ProviderClass::Local,
            protocol: Protocol::ChatCompletions,
            base_url: base_url.into(),
            allowed_hosts: Self::strs(&["127.0.0.1", "localhost", "::1"]),
            auth: AuthHeader::None,
            auth_methods: vec![AuthMethod::Anonymous],
            required_headers: Vec::new(),
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: format!("{base_url}/models"),
                keyless: true,
            },
            docs_url: docs_url.into(),
            loopback_only: true,
            free_tier: FreeTier::None,
            connect_copy: None,
            credential_url: None,
            terms_caveat: Some("Runs on this machine; nothing leaves it.".into()),
            rate_limit_hint: None,
            data_badge: DataBadge::Local,
        }
    }

    /// A user-typed OpenAI-compatible endpoint (BYOK). `base_url` must be `https://`, or plaintext
    /// on loopback / an explicitly confirmed dev endpoint (enforced by the broker on use).
    pub fn custom_openai_compatible(id: &str, display_name: &str, base_url: &str) -> Option<Self> {
        let host = url::Url::parse(base_url)
            .ok()?
            .host_str()?
            .to_ascii_lowercase();
        Some(Self {
            id: id.into(),
            display_name: display_name.into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: base_url.trim_end_matches('/').into(),
            allowed_hosts: vec![host],
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::Keyring,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: format!("{}/models", base_url.trim_end_matches('/')),
                keyless: false,
            },
            docs_url: String::new(),
            loopback_only: false,
            free_tier: FreeTier::None,
            connect_copy: Some(format!("Add an API key for {display_name}")),
            credential_url: None,
            terms_caveat: Some("Custom endpoint: check the operator's terms.".into()),
            rate_limit_hint: None,
            data_badge: DataBadge::Unknown,
        })
    }

    /// A user-typed Anthropic Messages-compatible endpoint (BYOK).
    pub fn custom_anthropic_compatible(
        id: &str,
        display_name: &str,
        base_url: &str,
    ) -> Option<Self> {
        let mut m = Self::custom_openai_compatible(id, display_name, base_url)?;
        m.protocol = Protocol::Messages;
        m.auth = AuthHeader::XApiKey;
        m.required_headers = vec![("anthropic-version".into(), "2023-06-01".into())];
        Some(m)
    }
}

/// All built-in provider manifests, in picker order: free-first Direct API providers, BYOK
/// providers, then Local.
pub fn builtin_manifests() -> Vec<ProviderManifest> {
    let s = ProviderManifest::strs;
    vec![
        ProviderManifest {
            id: "kilo".into(),
            display_name: "Kilo Gateway".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://api.kilo.ai/api/gateway".into(),
            allowed_hosts: s(&["api.kilo.ai"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::Anonymous, AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::None,
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://api.kilo.ai/api/gateway/models".into(),
                keyless: true,
            },
            docs_url: "https://kilo.ai/docs/gateway/authentication".into(),
            loopback_only: false,
            free_tier: FreeTier::Keyless,
            connect_copy: Some("Add a Kilo Gateway key for your own quota".into()),
            credential_url: Some("https://kilo.ai/docs/gateway/authentication".into()),
            terms_caveat: Some(
                "Free community models: prompts may be logged or used for training by the upstream provider. Not for sensitive data."
                    .into(),
            ),
            rate_limit_hint: Some("200 requests/hour per network (shared by everyone behind your IP)".into()),
            data_badge: DataBadge::MayTrain,
        },
        ProviderManifest {
            id: "openrouter".into(),
            display_name: "OpenRouter".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://openrouter.ai/api/v1".into(),
            allowed_hosts: s(&["openrouter.ai"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::OAuthPkce, AuthMethod::ApiKey],
            required_headers: vec![("X-Title".into(), "Workshop".into())],
            credential: CredentialSource::Env {
                var: "OPENROUTER_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://openrouter.ai/api/v1/models".into(),
                keyless: true,
            },
            docs_url: "https://openrouter.ai/docs/api-reference/overview".into(),
            loopback_only: false,
            free_tier: FreeTier::KeyRequired,
            connect_copy: Some("Sign in with OpenRouter".into()),
            credential_url: Some("https://openrouter.ai/settings/keys".into()),
            terms_caveat: Some(
                "Free endpoints require the account's privacy opt-in (providers may train on or publish inputs)."
                    .into(),
            ),
            rate_limit_hint: Some("Free models: 20 requests/minute, 50 requests/day until $10 of lifetime credits".into()),
            data_badge: DataBadge::MayTrain,
        },
        ProviderManifest {
            id: "google".into(),
            display_name: "Google AI Studio".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai".into(),
            allowed_hosts: s(&["generativelanguage.googleapis.com"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::Env {
                var: "GEMINI_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://generativelanguage.googleapis.com/v1beta/openai/models".into(),
                keyless: false,
            },
            docs_url: "https://ai.google.dev/gemini-api/docs/openai".into(),
            loopback_only: false,
            free_tier: FreeTier::KeyRequired,
            connect_copy: Some("Paste a Google AI Studio key".into()),
            credential_url: Some("https://aistudio.google.com/apikey".into()),
            terms_caveat: Some("Free tier: content is used to improve Google products.".into()),
            rate_limit_hint: Some("Free tier: about 250 requests/day, Flash models only".into()),
            data_badge: DataBadge::MayTrain,
        },
        ProviderManifest {
            id: "nvidia".into(),
            display_name: "NVIDIA build.nvidia.com".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://integrate.api.nvidia.com/v1".into(),
            allowed_hosts: s(&["integrate.api.nvidia.com"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::Env {
                var: "NVIDIA_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://integrate.api.nvidia.com/v1/models".into(),
                keyless: true,
            },
            docs_url: "https://build.nvidia.com".into(),
            loopback_only: false,
            free_tier: FreeTier::KeyRequired,
            connect_copy: Some("Paste an NVIDIA API key".into()),
            credential_url: Some("https://build.nvidia.com".into()),
            terms_caveat: Some(
                "Trial tier for prototyping and development; requests are logged to improve NVIDIA products."
                    .into(),
            ),
            rate_limit_hint: Some("Trial tier: about 40 requests/minute".into()),
            data_badge: DataBadge::MayTrain,
        },
        ProviderManifest {
            id: "openai".into(),
            display_name: "OpenAI".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::Responses,
            base_url: "https://api.openai.com/v1".into(),
            allowed_hosts: s(&["api.openai.com"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::Env {
                var: "OPENAI_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://api.openai.com/v1/models".into(),
                keyless: false,
            },
            docs_url: "https://platform.openai.com/docs/api-reference".into(),
            loopback_only: false,
            free_tier: FreeTier::None,
            connect_copy: Some("Add an OpenAI API key".into()),
            credential_url: Some("https://platform.openai.com/api-keys".into()),
            terms_caveat: None,
            rate_limit_hint: None,
            data_badge: DataBadge::Unknown,
        },
        ProviderManifest {
            id: "anthropic".into(),
            display_name: "Anthropic".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::Messages,
            base_url: "https://api.anthropic.com/v1".into(),
            allowed_hosts: s(&["api.anthropic.com"]),
            auth: AuthHeader::XApiKey,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: vec![("anthropic-version".into(), "2023-06-01".into())],
            credential: CredentialSource::Env {
                var: "ANTHROPIC_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsEndpoint {
                url: "https://api.anthropic.com/v1/models".into(),
                keyless: false,
            },
            docs_url: "https://docs.anthropic.com/en/api/messages".into(),
            loopback_only: false,
            free_tier: FreeTier::None,
            connect_copy: Some("Add an Anthropic API key".into()),
            credential_url: Some("https://console.anthropic.com/settings/keys".into()),
            terms_caveat: Some("Console API key only; Claude Pro/Max subscriptions are used through the claude CLI adapter.".into()),
            rate_limit_hint: None,
            data_badge: DataBadge::Unknown,
        },
        ProviderManifest {
            id: "opencode".into(),
            display_name: "OpenCode Zen".into(),
            class: ProviderClass::Direct,
            protocol: Protocol::ChatCompletions,
            base_url: "https://opencode.ai/zen/v1".into(),
            allowed_hosts: s(&["opencode.ai"]),
            auth: AuthHeader::Bearer,
            auth_methods: vec![AuthMethod::ApiKey],
            required_headers: Vec::new(),
            credential: CredentialSource::Env {
                var: "OPENCODE_API_KEY".into(),
            },
            model_catalog_source: ModelCatalogSource::ModelsDev {
                url: "https://models.opencode.ai/api.json".into(),
            },
            docs_url: "https://opencode.ai/docs/zen/".into(),
            loopback_only: false,
            free_tier: FreeTier::None,
            connect_copy: Some("Sign in to OpenCode Zen".into()),
            credential_url: Some("https://opencode.ai/auth".into()),
            terms_caveat: Some(
                "Paste an oc_sk_ key for paid models and Go. Zen's free models are only reachable from the OpenCode CLI (adapter)."
                    .into(),
            ),
            rate_limit_hint: None,
            data_badge: DataBadge::Unknown,
        },
        ProviderManifest::local(
            "ollama",
            "Ollama",
            "http://127.0.0.1:11434/v1",
            "https://github.com/ollama/ollama/blob/main/docs/openai.md",
        ),
        ProviderManifest::local(
            "lmstudio",
            "LM Studio",
            "http://127.0.0.1:1234/v1",
            "https://lmstudio.ai/docs/app/api/endpoints/openai",
        ),
        ProviderManifest::local(
            "llamacpp",
            "llama.cpp",
            "http://127.0.0.1:8080/v1",
            "https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md",
        ),
        ProviderManifest::local(
            "vllm",
            "vLLM",
            "http://127.0.0.1:8000/v1",
            "https://docs.vllm.ai/en/latest/serving/openai_compatible_server.html",
        ),
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
    fn provider_set_and_order_match_the_plan() {
        let ids: Vec<String> = builtin_manifests().iter().map(|m| m.id.clone()).collect();
        assert_eq!(
            ids,
            [
                "kilo",
                "openrouter",
                "google",
                "nvidia",
                "openai",
                "anthropic",
                "opencode",
                "ollama",
                "lmstudio",
                "llamacpp",
                "vllm"
            ]
        );
    }

    #[test]
    fn kilo_is_the_only_keyless_hosted_provider() {
        for m in builtin_manifests() {
            let keyless = !m.requires_credential();
            match m.id.as_str() {
                "kilo" => {
                    assert!(keyless);
                    assert_eq!(m.free_tier, FreeTier::Keyless);
                    assert!(m.accepts_key(), "optional key for a personal quota");
                    assert!(
                        m.rate_limit_hint
                            .as_deref()
                            .unwrap()
                            .contains("200 requests/hour")
                    );
                    assert!(
                        m.terms_caveat
                            .as_deref()
                            .unwrap()
                            .contains("logged or used for training")
                    );
                    assert_eq!(m.data_badge, DataBadge::MayTrain);
                    assert_eq!(
                        m.badge_line(),
                        "Free · No sign-in · Shared pool · may log/train"
                    );
                }
                _ if m.is_local() => assert!(keyless, "{}", m.id),
                _ => assert!(!keyless, "{} must not be keyless", m.id),
            }
        }
    }

    #[test]
    fn openrouter_offers_pkce_first_then_key() {
        let or = manifest("openrouter").unwrap();
        assert_eq!(
            or.auth_methods,
            vec![AuthMethod::OAuthPkce, AuthMethod::ApiKey]
        );
        assert!(or.supports_pkce());
        assert_eq!(or.connect_copy.as_deref(), Some("Sign in with OpenRouter"));
        assert_eq!(or.free_tier, FreeTier::KeyRequired);
        assert!(
            or.rate_limit_hint
                .as_deref()
                .unwrap()
                .contains("50 requests/day")
        );
        assert!(
            or.terms_caveat
                .as_deref()
                .unwrap()
                .contains("privacy opt-in")
        );
        assert_eq!(or.badge_line(), "Free · Sign in · may log/train");
    }

    #[test]
    fn google_and_nvidia_are_paste_key_free_tiers() {
        let g = manifest("google").unwrap();
        assert_eq!(
            g.endpoint(Protocol::ChatCompletions),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(
            g.credential_url.as_deref(),
            Some("https://aistudio.google.com/apikey")
        );
        assert_eq!(g.free_tier, FreeTier::KeyRequired);
        assert!(
            g.rate_limit_hint
                .as_deref()
                .unwrap()
                .contains("250 requests/day")
        );
        assert_eq!(g.badge_line(), "Free tier · API key · may log/train");
        let n = manifest("nvidia").unwrap();
        assert_eq!(
            n.endpoint(Protocol::ChatCompletions),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
        assert_eq!(
            n.credential_url.as_deref(),
            Some("https://build.nvidia.com")
        );
        assert!(
            n.rate_limit_hint
                .as_deref()
                .unwrap()
                .contains("40 requests/minute")
        );
        assert!(
            n.terms_caveat
                .as_deref()
                .unwrap()
                .to_lowercase()
                .contains("trial")
        );
    }

    #[test]
    fn byok_wire_shapes() {
        let anthropic = manifest("anthropic").unwrap();
        assert_eq!(anthropic.protocol, Protocol::Messages);
        assert_eq!(anthropic.auth, AuthHeader::XApiKey);
        assert!(
            anthropic
                .required_headers
                .contains(&("anthropic-version".into(), "2023-06-01".into()))
        );
        assert_eq!(
            anthropic.endpoint(Protocol::Messages),
            "https://api.anthropic.com/v1/messages"
        );
        let openai = manifest("openai").unwrap();
        assert_eq!(
            openai.endpoint(Protocol::Responses),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(openai.auth, AuthHeader::Bearer);
        assert_eq!(openai.badge_line(), "Direct API · see terms");
    }

    #[test]
    fn opencode_zen_is_key_required_and_never_free() {
        let zen = manifest("opencode").unwrap();
        assert!(zen.requires_credential());
        assert_eq!(
            zen.free_tier,
            FreeTier::None,
            "free rows are adapter-only until the keyed curl is confirmed"
        );
        assert_eq!(zen.connect_copy.as_deref(), Some("Sign in to OpenCode Zen"));
        assert_eq!(
            zen.credential_url.as_deref(),
            Some("https://opencode.ai/auth")
        );
        assert!(
            zen.terms_caveat
                .as_deref()
                .unwrap()
                .contains("OpenCode CLI")
        );
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
            assert!(m.base_url.starts_with("http://127.0.0.1:"), "{}", m.id);
            assert_eq!(m.protocol, Protocol::ChatCompletions, "{}", m.id);
            assert_eq!(m.data_badge, DataBadge::Local);
            assert_eq!(m.badge_line(), "Free · Local · Offline · local");
        }
    }

    #[test]
    fn custom_endpoints_take_the_host_from_the_url() {
        let c = ProviderManifest::custom_openai_compatible(
            "my-vllm",
            "Office vLLM",
            "https://llm.example.com/v1/",
        )
        .unwrap();
        assert_eq!(c.base_url, "https://llm.example.com/v1");
        assert_eq!(c.allowed_hosts, vec!["llm.example.com"]);
        assert_eq!(c.protocol, Protocol::ChatCompletions);
        let a = ProviderManifest::custom_anthropic_compatible(
            "zai-anthropic",
            "Z.ai",
            "https://api.z.ai/api/anthropic",
        )
        .unwrap();
        assert_eq!(a.protocol, Protocol::Messages);
        assert_eq!(a.auth, AuthHeader::XApiKey);
        assert!(ProviderManifest::custom_openai_compatible("bad", "Bad", "not a url").is_none());
    }

    #[test]
    fn no_manifest_defaults_to_xai_or_grok() {
        for m in builtin_manifests() {
            assert!(
                !m.base_url.contains("x.ai") && !m.base_url.contains("grok.com"),
                "{}",
                m.id
            );
            for host in &m.allowed_hosts {
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
