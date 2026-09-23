//! Mirror of OpenCode's free model list.
//!
//! Source: `GET /config/providers` on the running `opencode serve`. With no
//! OpenCode credential the genuine client itself keeps only zero-cost
//! `opencode/*` models enabled and reports its default (`Big Pickle` today),
//! so the rows here are exactly what OpenCode would offer for free — refreshed
//! every launch, nothing hard-coded.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One free row for the picker's Models tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreeModel {
    /// Model id within the `opencode` provider, e.g. `big-pickle`.
    pub id: String,
    pub name: String,
    /// `opencode/<id>`, the form OpenCode accepts for `--model`.
    pub model_ref: String,
    pub context_limit: Option<u64>,
    pub output_limit: Option<u64>,
    pub tool_call: bool,
    pub reasoning: bool,
    pub release_date: Option<String>,
    /// OpenCode's own current default for this provider.
    pub is_default: bool,
    /// The model can see images.
    #[serde(default)]
    pub image_input: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FreeCatalog {
    pub fetched_at: SystemTime,
    pub opencode_version: String,
    /// `opencode/<id>` of OpenCode's default free model, when it is free.
    pub default_model: Option<String>,
    /// Default first, then by name.
    pub models: Vec<FreeModel>,
}

impl FreeCatalog {
    pub fn default_or_first(&self) -> Option<&FreeModel> {
        self.models
            .iter()
            .find(|m| m.is_default)
            .or_else(|| self.models.first())
    }
}

fn is_zero_cost(model: &Value) -> bool {
    let Some(cost) = model.get("cost").and_then(Value::as_object) else {
        return false;
    };
    fn zero(v: &Value) -> bool {
        match v {
            Value::Number(n) => n.as_f64() == Some(0.0),
            Value::Object(o) => o.values().all(zero),
            Value::Null => true,
            _ => false,
        }
    }
    let input_present = cost.contains_key("input");
    input_present && cost.values().all(zero)
}

fn capability(model: &Value, modern: &str, legacy: &str) -> bool {
    model
        .get("capabilities")
        .and_then(|c| c.get(modern))
        .and_then(Value::as_bool)
        .or_else(|| model.get(legacy).and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Image input: 1.18.31 reports `capabilities.input.image`; models.dev entries carry
/// `modalities.input` (a list) or `attachment`.
fn reads_images(model: &Value) -> bool {
    model
        .get("capabilities")
        .and_then(|c| c.get("input"))
        .and_then(|i| i.get("image"))
        .and_then(Value::as_bool)
        .or_else(|| {
            model
                .get("modalities")
                .and_then(|m| m.get("input"))
                .and_then(Value::as_array)
                .map(|input| input.iter().any(|v| v.as_str() == Some("image")))
        })
        .or_else(|| model.get("attachment").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Build the free catalog from a `/config/providers` response.
pub fn parse_free_catalog(providers: &Value, opencode_version: &str) -> FreeCatalog {
    let default_id = providers
        .get("default")
        .and_then(|d| d.get("opencode"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut models: Vec<FreeModel> = providers
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.get("id").and_then(Value::as_str) == Some("opencode"))
        .flat_map(|p| {
            p.get("models")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<Vec<_>>()
        })
        .filter(|(_, m)| is_zero_cost(m))
        .filter(|(_, m)| {
            m.get("status")
                .and_then(Value::as_str)
                .is_none_or(|s| s != "deprecated")
        })
        .map(|(key, m)| {
            let id = m
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(&key)
                .to_string();
            let limit = m.get("limit").unwrap_or(&Value::Null);
            FreeModel {
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                model_ref: format!("opencode/{id}"),
                context_limit: limit.get("context").and_then(Value::as_u64),
                output_limit: limit.get("output").and_then(Value::as_u64),
                tool_call: capability(&m, "toolcall", "tool_call"),
                reasoning: capability(&m, "reasoning", "reasoning"),
                release_date: m
                    .get("release_date")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                is_default: default_id.as_deref() == Some(id.as_str()),
                image_input: reads_images(&m),
                id,
            }
        })
        .collect();
    models.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    let default_model = models
        .iter()
        .find(|m| m.is_default)
        .map(|m| m.model_ref.clone());
    FreeCatalog {
        fetched_at: SystemTime::now(),
        opencode_version: opencode_version.to_string(),
        default_model,
        models,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_only_free_active_opencode_models_and_marks_default() {
        let providers = json!({
            "default": {"opencode": "big-pickle", "anthropic": "claude-sonnet-4"},
            "providers": [
                {"id": "anthropic", "name": "Anthropic", "models": {
                    "claude-sonnet-4": {"id": "claude-sonnet-4", "name": "Claude", "cost": {"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}}
                }},
                {"id": "opencode", "name": "OpenCode Zen", "models": {
                    "big-pickle": {"id": "big-pickle", "name": "Big Pickle", "status": "active",
                        "cost": {"input": 0, "output": 0, "cache": {"read": 0, "write": 0}},
                        "limit": {"context": 200000, "output": 32000},
                        "capabilities": {"toolcall": true, "reasoning": true}, "release_date": "2025-10-17"},
                    "mimo-v2.5-free": {"id": "mimo-v2.5-free", "name": "MiMo V2.5 Free", "status": "active",
                        "cost": {"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}, "tool_call": true},
                    "old-free": {"id": "old-free", "name": "Old Free", "status": "deprecated",
                        "cost": {"input": 0, "output": 0, "cache": {"read": 0, "write": 0}}},
                    "gpt-5-nano": {"id": "gpt-5-nano", "name": "GPT-5 Nano", "status": "active",
                        "cost": {"input": 0.05, "output": 0.4, "cache": {"read": 0, "write": 0}}},
                    "no-cost-field": {"id": "no-cost-field", "name": "Mystery"}
                }}
            ]
        });
        let cat = parse_free_catalog(&providers, "1.18.31");
        assert_eq!(
            cat.models
                .iter()
                .map(|m| m.model_ref.as_str())
                .collect::<Vec<_>>(),
            ["opencode/big-pickle", "opencode/mimo-v2.5-free"]
        );
        assert_eq!(cat.default_model.as_deref(), Some("opencode/big-pickle"));
        let bp = &cat.models[0];
        assert!(bp.is_default && bp.tool_call && bp.reasoning);
        assert_eq!(bp.context_limit, Some(200000));
        assert_eq!(bp.output_limit, Some(32000));
        assert!(cat.models[1].tool_call, "legacy tool_call field honoured");
        assert_eq!(cat.default_or_first().unwrap().id, "big-pickle");
    }

    /// Captured from a live 1.18.31 `opencode serve`: Big Pickle is text-only, the Muse and MiMo
    /// models read images.
    #[test]
    fn image_input_comes_from_the_live_catalog() {
        let providers: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/opencode_serve_providers.json"
        ))
        .unwrap();
        let cat = parse_free_catalog(&providers, "1.18.31");
        let sees = |id: &str| cat.models.iter().find(|m| m.id == id).unwrap().image_input;
        assert!(!sees("big-pickle"));
        for id in [
            "muse-spark-1.3-contributor-free",
            "muse-spark-1.2-contributor-free",
            "mimo-v2.6-flash-free",
        ] {
            assert!(sees(id), "{id}");
        }
        assert!(reads_images(
            &json!({"modalities": {"input": ["text", "image"]}})
        ));
        assert!(reads_images(&json!({"attachment": true})));
        assert!(!reads_images(&json!({})));
    }
}
