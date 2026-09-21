//! Model catalog rows.
//!
//! A row is `provider:model[:variant]`, carries its protocol, base URL, credential source, and a
//! `free` flag with the source and timestamp of that claim. Free means a zero-price catalog row
//! from OpenCode Zen (`provider == "opencode"` and `input == 0`), matching the picker export; the
//! two default-visible free rows are `deepseek-v4-flash` and `big-pickle`.

use serde::{Deserialize, Serialize};

use crate::manifest::{CredentialSource, Protocol, ProviderClass, builtin_manifests};

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
    /// e.g. `builtin`, `models.dev`, `opencode.ai/zen/v1/models`.
    pub name: String,
    /// ISO-8601 date the row was reviewed or fetched.
    pub as_of: String,
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
    pub source: CatalogSource,
    /// Shown by default on the Models tab (other free rows stay hidden until "show more").
    pub default_visible: bool,
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

    /// Free tag rule from the export: OpenCode rows with zero input price.
    pub fn is_free(&self) -> bool {
        self.provider_id == "opencode" && self.price.is_some_and(|p| p.input == 0.0)
    }

    pub fn endpoint(&self) -> String {
        format!("{}{}", self.base_url, self.protocol.path())
    }
}

const ZEN_REVIEWED: &str = "2026-09-21";

fn zen_row(model_id: &str, display: &str, protocol: Protocol, default_visible: bool) -> CatalogModel {
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
        source: CatalogSource {
            name: "opencode.ai/docs/zen + opencode models opencode".into(),
            as_of: ZEN_REVIEWED.into(),
        },
        default_visible,
    }
}

/// OpenCode Zen free rows as documented on 2026-09-21. The first two are the picker defaults.
pub fn opencode_free_models() -> Vec<CatalogModel> {
    vec![
        zen_row("deepseek-v4-flash", "deepseek v4 flash", Protocol::ChatCompletions, true),
        zen_row("big-pickle", "big pickle", Protocol::ChatCompletions, true),
        zen_row("deepseek-v4-flash-free", "DeepSeek V4 Flash Free", Protocol::ChatCompletions, false),
        zen_row("mimo-v2.6-flash-free", "MiMo-V2.6-Flash Free", Protocol::ChatCompletions, false),
        zen_row("mimo-v2.5-free", "MiMo-V2.5 Free", Protocol::ChatCompletions, false),
        zen_row("ling-3.0-flash-fin-free", "Ling 3.0 Flash Fin Free", Protocol::ChatCompletions, false),
        zen_row("nemotron-3-ultra-free", "Nemotron 3 Ultra Free", Protocol::ChatCompletions, false),
        zen_row("nemotron-3.5-lightning-free", "Nemotron 3.5 Lightning Free", Protocol::ChatCompletions, false),
        zen_row("muse-spark-1.3-contributor-free", "Muse Spark 1.3 Contributor Free", Protocol::Responses, false),
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
        price: if m.is_local() { Some(Price::FREE) } else { None },
        source: CatalogSource {
            name: "user".into(),
            as_of: String::new(),
        },
        default_visible: true,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    /// Rows shipped in this crate: the Zen free rows. BYOK and local rows are added per user.
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

    /// Rows for the Models tab grouped by provider display name, in manifest order, default-visible
    /// rows first within each group. Agent-adapter rows (Cursor and friends) never appear here.
    pub fn picker_groups(&self, show_hidden: bool) -> Vec<(String, Vec<&CatalogModel>)> {
        let manifests = builtin_manifests();
        let mut groups: Vec<(String, Vec<&CatalogModel>)> = Vec::new();
        for m in &manifests {
            let mut rows: Vec<&CatalogModel> = self
                .models
                .iter()
                .filter(|row| row.provider_id == m.id && (show_hidden || row.default_visible))
                .collect();
            if rows.is_empty() {
                continue;
            }
            rows.sort_by(|a, b| {
                b.default_visible
                    .cmp(&a.default_visible)
                    .then_with(|| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()))
            });
            groups.push((m.display_name.to_string(), rows));
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_rows_named_in_the_spec_are_default_visible_and_tagged_free() {
        let cat = Catalog::builtin();
        let visible: Vec<&str> = cat
            .models
            .iter()
            .filter(|m| m.default_visible)
            .map(|m| m.display_name.as_str())
            .collect();
        assert_eq!(visible, ["deepseek v4 flash", "big pickle"]);
        for m in &cat.models {
            assert!(m.is_free(), "{}", m.key());
            assert_eq!(m.provider_id, "opencode");
            assert!(!m.source.as_of.is_empty(), "free claims carry a timestamp");
            assert!(m.endpoint().starts_with("https://opencode.ai/zen/v1/"));
        }
        assert_eq!(cat.get("opencode:big-pickle").unwrap().slash_id(), "opencode/big-pickle");
    }

    #[test]
    fn free_tag_requires_opencode_provider() {
        let mut row = custom_row("ollama", "llama3").unwrap();
        assert_eq!(row.price, Some(Price::FREE));
        assert!(!row.is_free(), "local zero-cost rows are Local, not Free");
        row.provider_id = "cursor".into();
        assert!(!row.is_free());
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

        let groups = cat.picker_groups(false);
        let names: Vec<&str> = groups.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["OpenAI", "OpenCode Zen", "Ollama"]);
        assert_eq!(groups[1].1.len(), 2, "hidden free rows stay hidden");
        assert!(cat.picker_groups(true)[1].1.len() > 2);
    }
}
