//! Workshop Direct API and Local providers (milestone C).
//!
//! * [`manifest`] — typed [`ProviderManifest`]s: Kilo Gateway (keyless `:free` pool, 200
//!   requests/hour per IP, may log/train), OpenRouter (PKCE "Sign in" + key; `:free` rows at
//!   20 RPM / 50 per day), Google AI Studio (key; Gemini Flash free tier), NVIDIA build (key;
//!   trial tier ~40 RPM), OpenAI, Anthropic (API key only), OpenCode Zen (`oc_sk_` key for paid
//!   models; free rows adapter-only), the loopback Local servers (Ollama, LM Studio, llama.cpp,
//!   vLLM), and user-typed OpenAI-/Anthropic-compatible BYOK endpoints. Each manifest pins the
//!   endpoint, protocol, auth methods, [`FreeTier`], connect copy, credential URL, terms caveat,
//!   rate-limit hint, and data badge.
//! * [`catalog`] — [`CatalogModel`] rows (`provider:model:variant`) with protocol, base URL,
//!   credential source, price, tools, context window, and a free claim with source + timestamp;
//!   parsers for the Kilo / OpenRouter / NVIDIA / Models.dev list formats and a cached fetcher.
//! * [`local`] — loopback-only probes that enumerate local models (Ollama `/api/tags` +
//!   `/api/show`, llama.cpp `/v1/models` + `/props`, generic `/v1/models`) and the ≥ 7B,
//!   tool-capable default rule.
//! * [`default_selection`] — the plan's first-run order: detected Local → Kilo `kilo-auto/free`
//!   (fallbacks `openrouter/free` → `nvidia/nemotron-3-super-120b-a12b:free`) → prompt to connect.
//! * [`oauth`] — OpenRouter PKCE sign-in (loopback callback on any port, headless code paste).
//! * [`secrets`] / [`broker`] — BYOK storage in the OS keyring with an owner-only file fallback
//!   under the Workshop home, an atomic 0600 connections file, and credentials bound to a provider
//!   id + auth header + host allowlist.
//! * [`resolve`] — the `[model.<key>]` resolution API for the M0 overlay (upstream
//!   `ModelEntryConfig` field names, secrets exposed only as `env_key` names).
//! * [`sampler`] — the mapping onto `xai_grok_sampler::SamplerConfig` / `ApiBackend`.
//!
//! Catalog compatibility (Models.dev metadata) is not wire compatibility; only the three
//! protocols the sampler speaks are offered here. No vendor client is ever impersonated.

pub mod broker;
pub mod catalog;
pub mod config;
pub mod default_selection;
pub mod local;
pub mod manifest;
pub mod oauth;
pub mod resolve;
pub mod sampler;
pub mod secrets;

pub use broker::{CredentialBroker, CredentialHandle, CredentialRef, ProviderError};
pub use catalog::fetch::{CatalogFetcher, FetchedCatalog, Freshness};
pub use catalog::{
    Catalog, CatalogModel, CatalogSource, FreeTier, KILO_DEFAULT_CHAIN, PickerGroup, Price,
};
pub use config::{ConnectionRecord, ConnectionsFile, atomic_write_private};
pub use default_selection::{ConnectOption, DefaultSelection, on_rate_limited, select_default};
pub use local::{
    LocalHealth, LocalModel, LocalServerStatus, probe_all_local_servers, probe_server,
};
pub use manifest::{
    AuthHeader, AuthMethod, CredentialSource, DataBadge, Protocol, ProviderClass, ProviderManifest,
    builtin_manifests, manifest,
};
pub use oauth::{OpenRouterSignIn, SignInMode};
pub use resolve::{CredentialInjection, ModelEntrySpec, resolve_model_entry, workshop_env_key};
pub use sampler::{api_backend_for, auth_scheme_for, sampler_config_for};
pub use secrets::{
    FileSecretStore, KeyringSecretStore, LayeredSecretStore, MemorySecretStore, SecretStore,
    workshop_home,
};

/// How usage for a connection is accounted, shown next to the class label everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageState {
    /// Token counts and price come from the provider response and a priced catalog row.
    Exact,
    /// Token counts are known but price is estimated (no price row).
    Estimated,
    /// A vendor subscription: Workshop cannot see the bill.
    SubscriptionUnknown,
    /// Local inference: no bill.
    Local,
}
