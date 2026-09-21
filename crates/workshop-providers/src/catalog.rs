//! Model catalog rows.
//!
//! A row is `provider:model[:variant]`, carries its protocol, base URL, credential source, price,
//! and a [`FreeTier`] claim with the source and timestamp of that claim.
//!
//! Free is never a keyless default. OpenCode Zen's zero-price rows are real, but Zen's keyless
//! free tier is gated to the genuine `opencode` client (see
//! `internal/opencode-free-models-proof.md`), so those rows are [`FreeTier::KeyRequired`] and stay
//! hidden until the user connects a Zen key. Providers that turn out to offer a keyless free tier
//! to third-party clients (the ranked survey in progress) get [`FreeTier::Keyless`] rows here.

use serde::{Deserialize, Serialize};

use crate::manifest::{
    CredentialSource, Protocol, ProviderClass, ProviderManifest, builtin_manifests,
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
}

/// Where a catalog row's metadata (including any price) came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSource {
    /// e.g. `builtin`, `models.opencode.ai/api.json`, `opencode.ai/zen/v1/models`.
    pub name: String,
    /// ISO-8601 date the row was reviewed or fetched.
    pub as_of: String,
}

/// How a zero-price row can actually be reached from Workshop's own HTTP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FreeTier {
    /// Priced, or price unknown.
    None,
    /// Zero-price, but the provider still requires a credential (OpenCode Zen).
    KeyRequired,
    /// Zero-price and reachable without any credential from a third-party client.
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

    /// `Free` tag rule from the picker export: a zero-price OpenCode row. Says nothing about how
    /// the row is reached; see [`Self::free_tier`].
    pub fn is_free(&self) -> bool {
        self.provider_id == "opencode" && self.price.is_some_and(|p| p.input == 0.0)
    }

    /// Usable without connecting any credential.
    pub fn is_keyless(&self) -> bool {
        self.free_tier == FreeTier::Keyless || self.credential == CredentialSource::None
    }

    pub fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.protocol.path())
    }
}

const ZEN_REVIEWED: &str = "2026-09-21";
const ZEN_NOTE: &str = "Requires OpenCode Zen sign-in (or the OpenCode CLI adapter). Free rows may be used to improve the model.";

fn zen_row(model_id: &str, display: &str, protocol: Protocol) -> CatalogModel {
    CatalogModel {
        provider_id: "opencode".into(),
        model_id: model_id.into(),
        variant: None,
        display_name: display.into(),
        class: ProviderClass::Direct,
        protocol,
        base_url: "https://opencode.ai/zen/v1".into(),
        credential: CredentialSource::Env {
            var: "OPENCODE_API_KEY".into(),
        },
        price: Some(Price::FREE),
        free_tier: FreeTier::KeyRequired,
        source: CatalogSource {
            name: "models.opencode.ai/api.json (cost.input == 0, not deprecated) + opencode.ai/docs/zen".into(),
            as_of: ZEN_REVIEWED.into(),
        },
        default_visible: false,
        note: Some(ZEN_NOTE.into()),
    }
}

/// OpenCode Zen zero-price rows as of 2026-09-21 (seed; the live catalog is the manifest's
/// Models.dev feed). All are key-required from Workshop's own client.
pub fn opencode_free_models() -> Vec<CatalogModel> {
    vec![
        zen_row("big-pickle", "Big Pickle", Protocol::ChatCompletions),
        zen_row(
            "mimo-v2.5-free",
            "MiMo-V2.5 Free",
            Protocol::ChatCompletions,
        ),
        zen_row(
            "ling-3.0-flash-fin-free",
            "Ling 3.0 Flash Fin Free",
            Protocol::ChatCompletions,
        ),
        zen_row(
            "nemotron-3-ultra-free",
            "Nemotron 3 Ultra Free",
            Protocol::ChatCompletions,
        ),
        zen_row(
            "nemotron-3.5-lightning-free",
            "Nemotron 3.5 Lightning Free",
            Protocol::ChatCompletions,
        ),
        zen_row(
            "muse-spark-1.3-contributor-free",
            "Muse Spark 1.3 Contributor Free",
            Protocol::Responses,
        ),
        zen_row(
            "muse-spark-1.2-contributor-free",
            "Muse Spark 1.2 Contributor Free",
            Protocol::Responses,
        ),
    ]
}

/// A bring-your-own-key or local row: the user types the model id; the manifest supplies the rest.
pub fn custom_row(provider_id: &str, model_id: &str) -> Option<CatalogModel> {
    let m = crate::manifest::manifest(provider_id)?;
    Some(CatalogModel {
        provider_id: m.id.into(),
        model_id: model_id.into(),
        variant: None,
        display_name: model_id.into(),
        class: m.class,
        protocol: m.protocol,
        base_url: m.base_url.into(),
        credential: m.credential.clone(),
        price: if m.is_local() {
            Some(Price::FREE)
        } else {
            None
        },
        free_tier: FreeTier::None,
        source: CatalogSource {
            name: "user".into(),
            as_of: String::new(),
        },
        default_visible: true,
        note: None,
    })
}

/// One provider group on the Models tab.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerGroup<'a> {
    pub provider_id: &'static str,
    pub display_name: &'static str,
    pub class: ProviderClass,
    /// Rows the user can select right now.
    pub rows: Vec<&'a CatalogModel>,
    /// Rows that exist but need the provider connected first.
    pub locked_rows: usize,
    /// Connect action to show when the provider is not connected (e.g. "Sign in to OpenCode Zen").
    pub connect_copy: Option<&'static str>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    /// Rows shipped in this crate: the Zen zero-price seed. BYOK and local rows are added per user.
    pub fn builtin() -> Self {
        Self {
            models: opencode_free_models(),
        }
    }

    pub fn push(&mut self, model: CatalogModel) {
        self.models.retain(|m| m.key() != model.key());
        self.models.push(model);
    }

    pub fn get(&self, key: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.key() == key)
    }

    /// Groups for the Models tab in manifest order (Direct API first, then Local).
    ///
    /// `connected(provider_id)` says whether the provider has a credential (or needs none). Rows
    /// of an unconnected credential-requiring provider are locked and the group shows the
    /// manifest's connect copy instead; a group with nothing to show is omitted. Agent-adapter
    /// rows (Cursor and friends) never appear here.
    pub fn picker_groups<'a>(&'a self, connected: impl Fn(&str) -> bool) -> Vec<PickerGroup<'a>> {
        let manifests: Vec<ProviderManifest> = builtin_manifests();
        let mut groups = Vec::new();
        for m in &manifests {
            let is_connected = !m.requires_credential() || connected(m.id);
            let mut rows: Vec<&CatalogModel> = self
                .models
                .iter()
                .filter(|row| row.provider_id == m.id)
                .collect();
            rows.sort_by(|a, b| {
                b.is_free().cmp(&a.is_free()).then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
            });
            let (selectable, locked): (Vec<&CatalogModel>, Vec<&CatalogModel>) = rows
                .into_iter()
                .partition(|row| is_connected || row.default_visible || row.is_keyless());
            let connect_copy = if is_connected { None } else { m.connect_copy };
            if selectable.is_empty() && locked.is_empty() && connect_copy.is_none() {
                continue;
            }
            groups.push(PickerGroup {
                provider_id: m.id,
                display_name: m.display_name,
                class: m.class,
                rows: selectable,
                locked_rows: locked.len(),
                connect_copy,
            });
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_rows_are_key_required_and_hidden_until_connected() {
        let cat = Catalog::builtin();
        assert!(!cat.models.is_empty());
        for m in &cat.models {
            assert_eq!(m.provider_id, "opencode");
            assert!(m.is_free(), "{}", m.key());
            assert_eq!(m.free_tier, FreeTier::KeyRequired, "{}", m.key());
            assert!(
                !m.is_keyless(),
                "{}: Zen is never keyless from Workshop",
                m.key()
            );
            assert!(!m.default_visible, "{}: no keyless free default", m.key());
            assert!(!m.source.as_of.is_empty(), "free claims carry a timestamp");
            assert!(m.note.as_deref().is_some_and(|n| n.contains("sign-in")));
            assert!(m.endpoint().starts_with("https://opencode.ai/zen/v1/"));
        }
        assert_eq!(
            cat.get("opencode:big-pickle").unwrap().slash_id(),
            "opencode/big-pickle"
        );

        // Not connected: the group shows the connect action and locks every row.
        let groups = cat.picker_groups(|_| false);
        let zen = groups
            .iter()
            .find(|g| g.provider_id == "opencode")
            .expect("zen group");
        assert!(zen.rows.is_empty());
        assert_eq!(zen.locked_rows, cat.models.len());
        assert_eq!(zen.connect_copy, Some("Sign in to OpenCode Zen"));

        // Connected: rows unlock, connect action disappears.
        let groups = cat.picker_groups(|id| id == "opencode");
        let zen = groups.iter().find(|g| g.provider_id == "opencode").unwrap();
        assert_eq!(zen.rows.len(), cat.models.len());
        assert_eq!(zen.locked_rows, 0);
        assert_eq!(zen.connect_copy, None);
        assert_eq!(
            zen.rows[0].display_name, "Big Pickle",
            "free rows sort first, then by name"
        );
    }

    #[test]
    fn free_tag_requires_opencode_provider_and_keyless_is_explicit() {
        let mut row = custom_row("ollama", "llama3").unwrap();
        assert_eq!(row.price, Some(Price::FREE));
        assert!(!row.is_free(), "local zero-cost rows are Local, not Free");
        assert!(row.is_keyless(), "local rows need no credential");
        row.provider_id = "cursor".into();
        assert!(!row.is_free());

        let mut keyless = custom_row("openrouter", "some/free-model").unwrap();
        keyless.free_tier = FreeTier::Keyless;
        keyless.price = Some(Price::FREE);
        assert!(
            keyless.is_keyless(),
            "room for surveyed keyless free providers"
        );
        let mut cat = Catalog::default();
        cat.push(keyless);
        let groups = cat.picker_groups(|_| false);
        let or = groups
            .iter()
            .find(|g| g.provider_id == "openrouter")
            .unwrap();
        assert_eq!(
            or.rows.len(),
            1,
            "keyless rows are selectable without connecting"
        );
        assert_eq!(or.connect_copy, Some("Add an OpenRouter API key"));
    }

    #[test]
    fn keys_separate_variants_and_groups_follow_manifest_order() {
        let mut cat = Catalog::builtin();
        let mut fast = custom_row("openai", "gpt-5").unwrap();
        fast.variant = Some("fast".into());
        let mut max = fast.clone();
        max.variant = Some("max".into());
        cat.push(fast.clone());
        cat.push(max.clone());
        assert_eq!(fast.key(), "openai:gpt-5:fast");
        assert_eq!(max.key(), "openai:gpt-5:max");
        cat.push(custom_row("ollama", "llama3").unwrap());

        let groups = cat.picker_groups(|id| id == "openai");
        let names: Vec<&str> = groups.iter().map(|g| g.display_name).collect();
        assert_eq!(
            names,
            [
                "OpenAI",
                "Anthropic",
                "OpenRouter",
                "OpenCode Zen",
                "Ollama"
            ]
        );
        assert_eq!(groups[0].rows.len(), 2);
        assert_eq!(groups[0].connect_copy, None);
        assert_eq!(
            groups[1].connect_copy,
            Some("Add an Anthropic API key"),
            "empty unconnected BYOK group still offers Connect"
        );
        assert_eq!(groups[4].class, ProviderClass::Local);
        assert_eq!(groups[4].rows.len(), 1);
    }
}
