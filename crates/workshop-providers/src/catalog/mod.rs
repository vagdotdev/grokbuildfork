//! Model catalog rows.
//!
//! A row is `provider:model[:variant]` and carries its protocol, base URL, credential source,
//! price, tool support, context window, and a [`FreeTier`] claim with the source and timestamp of
//! that claim.
//!
//! Free rows, in the order the plan's Models tab shows them: detected Local models; Kilo Gateway
//! `:free` (keyless, shared pool, may log/train); OpenRouter `:free` (sign in; 20 RPM / 50 per
//! day); Google AI Studio Gemini Flash (free tier, API key); NVIDIA build (trial tier, API key).
//! OpenCode Zen `*-free` rows are **not** here: the keyless tier is client-locked and keyed free
//! access is unconfirmed, so they are reachable only through the OpenCode CLI adapter.

pub mod fetch;
pub mod sources;

use serde::{Deserialize, Serialize};

use crate::manifest::{
    CredentialSource, DataBadge, Protocol, ProviderClass, ProviderManifest, builtin_manifests,
    manifest,
};

/// Price per million tokens, USD.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
}

impl Price {
    pub const FREE: Price = Price {
        input: 0.0,
        output: 0.0,
    };

    pub fn is_zero(&self) -> bool {
        self.input == 0.0 && self.output == 0.0
    }
}

/// Where a catalog row's metadata (including any price) came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSource {
    /// e.g. `builtin`, `api.kilo.ai/api/gateway/models`, `models.opencode.ai/api.json`.
    pub name: String,
    /// ISO-8601 date the row was reviewed or fetched.
    pub as_of: String,
}

/// How a zero-price row can actually be reached from Workshop's own HTTP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeTier {
    /// Priced, price unknown, or not a free row.
    None,
    /// Zero-price, but the provider still requires a credential (OpenRouter `:free`, Google,
    /// NVIDIA trial).
    KeyRequired,
    /// Zero-price and reachable without any credential from a third-party client (Kilo `:free`).
    Keyless,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    pub display_name: String,
    pub class: ProviderClass,
    pub protocol: Protocol,
    pub base_url: String,
    pub credential: CredentialSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<Price>,
    pub free_tier: FreeTier,
    pub source: CatalogSource,
    /// Shown on the Models tab even before the provider is connected.
    pub default_visible: bool,
    /// Caveat shown with the row (training/trial terms, how the row is reached).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Advertises tool calling (`None` = unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    pub data_badge: DataBadge,
}

impl CatalogModel {
    /// Radio key `provider:model[:variant]`.
    pub fn key(&self) -> String {
        match &self.variant {
            Some(v) => format!("{}:{}:{}", self.provider_id, self.model_id, v),
            None => format!("{}:{}", self.provider_id, self.model_id),
        }
    }

    /// `provider/model`, the form `/models` lists.
    pub fn slash_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }

    /// Zero-price row (the `Free` badge). Says nothing about how the row is reached; see
    /// [`Self::free_tier`] and [`Self::is_keyless`].
    pub fn is_free(&self) -> bool {
        self.price.is_some_and(|p| p.is_zero()) && self.class == ProviderClass::Direct
    }

    /// Usable without connecting any credential.
    pub fn is_keyless(&self) -> bool {
        self.free_tier == FreeTier::Keyless || self.credential == CredentialSource::None
    }

    pub fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.protocol.path())
    }

    /// Badge text for the row, e.g. `Free · No sign-in · Shared pool · may log/train`.
    pub fn badge_line(&self) -> String {
        match manifest(&self.provider_id) {
            Some(m) => m.badge_line(),
            None => self.class.label().to_string(),
        }
    }
}

/// A row built from a manifest: the manifest supplies protocol, base URL, credential source, and
/// badges; the caller supplies the model id and any metadata it knows.
pub fn row_for(
    m: &ProviderManifest,
    model_id: &str,
    display_name: &str,
    source: CatalogSource,
) -> CatalogModel {
    let free_tier = m.free_tier;
    CatalogModel {
        provider_id: m.id.clone(),
        model_id: model_id.into(),
        variant: None,
        display_name: display_name.into(),
        class: m.class,
        protocol: m.protocol,
        base_url: m.base_url.clone(),
        credential: m.credential.clone(),
        price: if free_tier != FreeTier::None || m.is_local() {
            Some(Price::FREE)
        } else {
            None
        },
        free_tier,
        source,
        default_visible: true,
        note: m.terms_caveat.clone(),
        tools: None,
        context_window: None,
        data_badge: m.data_badge,
    }
}

const SEED_REVIEWED: &str = "2026-09-21";

fn seed_source(name: &str) -> CatalogSource {
    CatalogSource {
        name: name.into(),
        as_of: SEED_REVIEWED.into(),
    }
}

/// The plan's first-run free chain: primary, then fallbacks, in order.
pub const KILO_DEFAULT_CHAIN: [&str; 3] = [
    "kilo-auto/free",
    "openrouter/free",
    "nvidia/nemotron-3-super-120b-a12b:free",
];

/// Kilo Gateway `:free` rows as of the review date (the live list comes from `/models`, `isFree`).
pub fn kilo_seed_models() -> Vec<CatalogModel> {
    let m = manifest("kilo").expect("kilo manifest");
    let src = || seed_source("api.kilo.ai/api/gateway/models (isFree) — seed");
    let mut rows = vec![
        row_for(
            &m,
            "kilo-auto/free",
            "Auto Free (rotates free models)",
            src(),
        ),
        row_for(&m, "openrouter/free", "Free Models Router", src()),
        row_for(
            &m,
            "nvidia/nemotron-3-super-120b-a12b:free",
            "NVIDIA: Nemotron 3 Super (free)",
            src(),
        ),
        row_for(
            &m,
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "NVIDIA: Nemotron 3 Ultra (free)",
            src(),
        ),
        row_for(
            &m,
            "qwen/qwen3.8-27b:free",
            "Qwen: Qwen3.8 27B (free)",
            src(),
        ),
        row_for(&m, "z-ai/glm-5.2:free", "Z.ai: GLM 5.2 (free)", src()),
    ];
    for r in &mut rows {
        r.tools = Some(r.model_id != "z-ai/glm-5.2:free");
    }
    rows
}

/// OpenRouter `:free` rows as of the review date (live list: `/api/v1/models`, `pricing.prompt == "0"`).
pub fn openrouter_seed_models() -> Vec<CatalogModel> {
    let m = manifest("openrouter").expect("openrouter manifest");
    let src = || seed_source("openrouter.ai/api/v1/models (pricing.prompt == 0) — seed");
    let mut rows = vec![
        row_for(&m, "openrouter/free", "Free Models Router", src()),
        row_for(
            &m,
            "nvidia/nemotron-3-super-120b-a12b:free",
            "NVIDIA: Nemotron 3 Super (free)",
            src(),
        ),
        row_for(
            &m,
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "NVIDIA: Nemotron 3 Ultra (free)",
            src(),
        ),
        row_for(
            &m,
            "google/gemma-4-31b-it:free",
            "Google: Gemma 4 31B (free)",
            src(),
        ),
        row_for(
            &m,
            "qwen/qwen3.8-27b:free",
            "Qwen: Qwen3.8 27B (free)",
            src(),
        ),
    ];
    for r in &mut rows {
        r.tools = Some(true);
    }
    rows
}

/// Google AI Studio free-tier models (pricing page, 2026-09-21). Keyed list endpoint, so seeded.
pub fn google_seed_models() -> Vec<CatalogModel> {
    let m = manifest("google").expect("google manifest");
    let src = || seed_source("ai.google.dev/gemini-api/docs/pricing (free tier) — seed");
    [
        ("gemini-3.8-flash", "Gemini 3.8 Flash"),
        ("gemini-3.7-flash", "Gemini 3.7 Flash"),
        ("gemini-3.6-flash", "Gemini 3.6 Flash"),
        ("gemini-3.5-flash", "Gemini 3.5 Flash"),
        ("gemini-3.5-flash-lite", "Gemini 3.5 Flash-Lite"),
        ("gemini-3.1-flash-lite", "Gemini 3.1 Flash-Lite"),
    ]
    .into_iter()
    .map(|(id, name)| {
        let mut r = row_for(&m, id, name, src());
        r.tools = Some(true);
        r
    })
    .collect()
}

/// NVIDIA build.nvidia.com coding-relevant trial models (live `/v1/models`, 2026-09-21).
pub const NVIDIA_CODING_MODELS: [&str; 8] = [
    "nvidia/nemotron-3-ultra-550b-a55b",
    "nvidia/nemotron-3-super-120b-a12b",
    "nvidia/nemotron-3.5-lightning-30b-a3b",
    "moonshotai/kimi-k3",
    "moonshotai/kimi-k2.6",
    "z-ai/glm-5.3",
    "z-ai/glm-5.3-flash",
    "openai/gpt-oss-20b",
];

pub fn nvidia_seed_models() -> Vec<CatalogModel> {
    let m = manifest("nvidia").expect("nvidia manifest");
    NVIDIA_CODING_MODELS
        .iter()
        .map(|id| {
            let mut r = row_for(
                &m,
                id,
                id,
                seed_source("integrate.api.nvidia.com/v1/models — seed"),
            );
            r.tools = Some(true);
            r
        })
        .collect()
}

/// A bring-your-own-key or local row: the user types the model id; the manifest supplies the rest.
pub fn custom_row(provider_id: &str, model_id: &str) -> Option<CatalogModel> {
    let m = manifest(provider_id)?;
    Some(row_for(
        &m,
        model_id,
        model_id,
        CatalogSource {
            name: "user".into(),
            as_of: String::new(),
        },
    ))
}

/// One provider group on the Models tab.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerGroup<'a> {
    pub provider_id: String,
    pub display_name: String,
    pub class: ProviderClass,
    /// Badge line for the group header.
    pub badge_line: String,
    /// Rows the user can select right now.
    pub rows: Vec<&'a CatalogModel>,
    /// Rows that exist but need the provider connected first.
    pub locked_rows: usize,
    /// Connect action to show when the provider is not connected.
    pub connect_copy: Option<String>,
    pub rate_limit_hint: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    /// Rows shipped in this crate: the reviewed free seeds. Live catalogs ([`fetch`]) and local
    /// enumeration ([`crate::local`]) replace or extend them at runtime.
    pub fn builtin() -> Self {
        let mut models = Vec::new();
        models.extend(kilo_seed_models());
        models.extend(openrouter_seed_models());
        models.extend(google_seed_models());
        models.extend(nvidia_seed_models());
        Self { models }
    }

    pub fn push(&mut self, model: CatalogModel) {
        self.models.retain(|m| m.key() != model.key());
        self.models.push(model);
    }

    /// Replace every row of `provider_id` with `rows` (a fresh live or cached list).
    pub fn replace_provider(&mut self, provider_id: &str, rows: Vec<CatalogModel>) {
        self.models.retain(|m| m.provider_id != provider_id);
        self.models.extend(rows);
    }

    pub fn get(&self, key: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.key() == key)
    }

    pub fn rows_for(&self, provider_id: &str) -> Vec<&CatalogModel> {
        self.models
            .iter()
            .filter(|m| m.provider_id == provider_id)
            .collect()
    }

    /// Groups for the Models tab: Local providers with rows first, then the rest in manifest order
    /// (free-first). `connected(provider_id)` says whether the provider has a credential (or needs
    /// none). Rows of an unconnected credential-requiring provider are locked and the group shows
    /// the manifest's connect copy; a group with nothing to show is omitted. Agent-adapter rows
    /// never appear here.
    pub fn picker_groups<'a>(&'a self, connected: impl Fn(&str) -> bool) -> Vec<PickerGroup<'a>> {
        let manifests: Vec<ProviderManifest> = builtin_manifests();
        let (local, hosted): (Vec<&ProviderManifest>, Vec<&ProviderManifest>) =
            manifests.iter().partition(|m| m.is_local());
        let mut groups = Vec::new();
        for m in local.into_iter().chain(hosted) {
            let is_connected = !m.requires_credential() || connected(&m.id);
            let mut rows: Vec<&CatalogModel> = self.rows_for(&m.id);
            if m.is_local() && rows.is_empty() {
                continue; // not detected
            }
            rows.sort_by(|a, b| {
                b.is_free()
                    .cmp(&a.is_free())
                    .then_with(|| b.tools.unwrap_or(false).cmp(&a.tools.unwrap_or(false)))
                    .then_with(|| {
                        a.display_name
                            .to_lowercase()
                            .cmp(&b.display_name.to_lowercase())
                    })
            });
            let (selectable, locked): (Vec<&CatalogModel>, Vec<&CatalogModel>) = rows
                .into_iter()
                .partition(|row| is_connected || row.is_keyless());
            let connect_copy = if is_connected {
                None
            } else {
                m.connect_copy.clone()
            };
            if selectable.is_empty() && locked.is_empty() && connect_copy.is_none() {
                continue;
            }
            groups.push(PickerGroup {
                provider_id: m.id.clone(),
                display_name: m.display_name.clone(),
                class: m.class,
                badge_line: m.badge_line(),
                rows: selectable,
                locked_rows: locked.len(),
                connect_copy,
                rate_limit_hint: m.rate_limit_hint.clone(),
            });
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_cover_the_plans_free_chain_and_are_badged() {
        let cat = Catalog::builtin();
        for id in KILO_DEFAULT_CHAIN {
            let row = cat
                .get(&format!("kilo:{id}"))
                .unwrap_or_else(|| panic!("kilo:{id}"));
            assert!(row.is_free() && row.is_keyless(), "{id}");
            assert_eq!(row.free_tier, FreeTier::Keyless);
            assert_eq!(row.data_badge, DataBadge::MayTrain);
            assert!(
                row.note
                    .as_deref()
                    .unwrap()
                    .contains("logged or used for training")
            );
            assert!(!row.source.as_of.is_empty());
        }
        for row in cat.rows_for("openrouter") {
            assert!(row.is_free() && !row.is_keyless(), "{}", row.key());
            assert_eq!(row.free_tier, FreeTier::KeyRequired);
        }
        for row in cat
            .rows_for("google")
            .into_iter()
            .chain(cat.rows_for("nvidia"))
        {
            assert_eq!(row.free_tier, FreeTier::KeyRequired, "{}", row.key());
            assert!(row.is_free());
        }
        assert!(
            cat.rows_for("opencode").is_empty(),
            "Zen free rows are adapter-only"
        );
        assert_eq!(
            cat.get("kilo:kilo-auto/free").unwrap().slash_id(),
            "kilo/kilo-auto/free"
        );
    }

    #[test]
    fn picker_groups_follow_the_plan_order_and_lock_keyed_free_rows() {
        let mut cat = Catalog::builtin();
        cat.push(custom_row("ollama", "qwen2.5-coder:7b").unwrap());
        let groups = cat.picker_groups(|_| false);
        let names: Vec<&str> = groups.iter().map(|g| g.display_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Ollama",
                "Kilo Gateway",
                "OpenRouter",
                "Google AI Studio",
                "NVIDIA build.nvidia.com",
                "OpenAI",
                "Anthropic",
                "OpenCode Zen"
            ]
        );
        let kilo = &groups[1];
        assert!(
            !kilo.rows.is_empty() && kilo.locked_rows == 0 && kilo.connect_copy.is_none(),
            "keyless rows are selectable"
        );
        assert_eq!(
            kilo.badge_line,
            "Free · No sign-in · Shared pool · may log/train"
        );
        assert!(
            kilo.rate_limit_hint
                .as_deref()
                .unwrap()
                .contains("200 requests/hour")
        );
        let or = &groups[2];
        assert!(or.rows.is_empty() && or.locked_rows > 0);
        assert_eq!(or.connect_copy.as_deref(), Some("Sign in with OpenRouter"));
        let zen = &groups[7];
        assert_eq!(zen.connect_copy.as_deref(), Some("Sign in to OpenCode Zen"));
        assert_eq!(zen.locked_rows, 0);

        // Connected OpenRouter unlocks its rows; free + tool-capable rows sort first.
        let groups = cat.picker_groups(|id| id == "openrouter");
        let or = groups
            .iter()
            .find(|g| g.provider_id == "openrouter")
            .unwrap();
        assert!(or.rows.len() >= 5 && or.locked_rows == 0 && or.connect_copy.is_none());
        assert!(or.rows[0].tools == Some(true));

        // Undetected local providers are omitted entirely.
        assert!(!groups.iter().any(|g| g.provider_id == "vllm"));
    }

    #[test]
    fn free_and_keyless_semantics() {
        let local = custom_row("ollama", "llama3").unwrap();
        assert_eq!(local.price, Some(Price::FREE));
        assert!(!local.is_free(), "local zero-cost rows are Local, not Free");
        assert!(local.is_keyless());
        let byok = custom_row("openai", "gpt-5").unwrap();
        assert!(!byok.is_free() && !byok.is_keyless());
        let mut keyless = custom_row("openrouter", "some/free-model").unwrap();
        keyless.free_tier = FreeTier::Keyless;
        assert!(
            keyless.is_keyless(),
            "room for surveyed keyless free providers"
        );
    }

    #[test]
    fn keys_separate_variants_and_replace_provider_swaps_rows() {
        let mut cat = Catalog::builtin();
        let mut fast = custom_row("openai", "gpt-5").unwrap();
        fast.variant = Some("fast".into());
        let mut max = fast.clone();
        max.variant = Some("max".into());
        cat.push(fast.clone());
        cat.push(max.clone());
        assert_eq!(fast.key(), "openai:gpt-5:fast");
        assert_eq!(max.key(), "openai:gpt-5:max");
        assert_eq!(cat.rows_for("openai").len(), 2);
        let before = cat.rows_for("kilo").len();
        cat.replace_provider("kilo", vec![custom_row("kilo", "only/one:free").unwrap()]);
        assert_ne!(before, 1);
        assert_eq!(cat.rows_for("kilo").len(), 1);
    }
}
