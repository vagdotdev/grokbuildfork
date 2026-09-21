//! Workshop Direct API and Local providers.
//!
//! * [`manifest`] — typed [`ProviderManifest`]s for the built-in providers: OpenAI, Anthropic (API
//!   key only), OpenRouter, OpenCode Zen (key required — "Sign in to OpenCode Zen"), and the local
//!   OpenAI-compatible servers (Ollama, LM Studio, llama.cpp, vLLM). A manifest pins the wire
//!   protocol, base URL, allowed hosts, auth header, required headers, credential source, and the
//!   connect copy shown before a credential exists.
//! * [`catalog`] — [`CatalogModel`] rows (`provider:model:variant` keys) with protocol, base URL,
//!   credential source, price, and a [`FreeTier`] claim with source + timestamp. Zero-price rows
//!   are never a keyless default; keyless free providers from the ongoing survey slot in as
//!   [`FreeTier::Keyless`].
//! * [`secrets`] / [`broker`] — BYOK storage in the OS keyring and an atomic, owner-only
//!   connections file. Credentials are bound to a provider id, auth scheme, and host allowlist;
//!   the broker refuses to release a credential for any other host.
//! * [`sampler`] — the mapping onto `xai_grok_sampler::SamplerConfig` /
//!   `xai_grok_sampling_types::ApiBackend`. The sampler crate is not modified.
//! * [`local`] — loopback-only reachability probes and the plaintext-HTTP rule.
//!
//! Catalog compatibility (Models.dev / OpenCode metadata) is not wire compatibility; only the
//! three protocols the sampler speaks are offered here.

pub mod broker;
pub mod catalog;
pub mod config;
pub mod local;
pub mod manifest;
pub mod sampler;
pub mod secrets;

pub use broker::{CredentialBroker, CredentialHandle, CredentialRef, ProviderError};
pub use catalog::{Catalog, CatalogModel, CatalogSource, FreeTier, PickerGroup, Price};
pub use config::{ConnectionRecord, ConnectionsFile, atomic_write_private};
pub use manifest::{
    AuthHeader, CredentialSource, Protocol, ProviderClass, ProviderManifest, builtin_manifests,
    manifest,
};
pub use sampler::{api_backend_for, auth_scheme_for, sampler_config_for};
pub use secrets::{KeyringSecretStore, MemorySecretStore, SecretStore};

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
