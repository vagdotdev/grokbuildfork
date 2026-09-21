//! Parsers for the provider model-list formats Workshop reads. Pure functions over JSON so they
//! are unit-tested against captured responses (`tests/fixtures/catalog/`, captured 2026-09-21).

use serde_json::Value;

use super::{CatalogModel, CatalogSource, FreeTier, Price, row_for};
use crate::manifest::{DataBadge, ProviderManifest, manifest};

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("unexpected catalog shape: {0}")]
    Shape(&'static str),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn data_array(v: &Value) -> Result<&Vec<Value>, ParseError> {
    v.get("data")
        .and_then(Value::as_array)
        .ok_or(ParseError::Shape("expected {\"data\": [...]}"))
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn has_param(v: &Value, param: &str) -> bool {
    v.get("supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|p| p.as_str() == Some(param)))
}

fn outputs_text(v: &Value) -> bool {
    match v
        .pointer("/architecture/output_modalities")
        .and_then(Value::as_array)
    {
        Some(mods) => mods.iter().any(|m| m.as_str() == Some("text")),
        None => true,
    }
}

fn zero_price_str(v: &Value) -> bool {
    matches!(v.pointer("/pricing/prompt").and_then(Value::as_str), Some(p) if p.trim().parse::<f64>().ok() == Some(0.0))
}

/// Kilo Gateway `GET /api/gateway/models`: OpenRouter-shaped rows plus `isFree` and
/// `mayTrainOnYourPrompts`. Returns only the free rows, ordered by `preferredIndex` then id.
pub fn parse_kilo_models(body: &str, as_of: &str) -> Result<Vec<CatalogModel>, ParseError> {
    let m = manifest("kilo").expect("kilo manifest");
    let v: Value = serde_json::from_str(body)?;
    let mut rows: Vec<(i64, CatalogModel)> = Vec::new();
    for item in data_array(&v)? {
        if item.get("isFree").and_then(Value::as_bool) != Some(true) || !outputs_text(item) {
            continue;
        }
        let Some(id) = str_of(item, "id") else {
            continue;
        };
        let name = str_of(item, "name").unwrap_or(id);
        let mut row = row_for(
            &m,
            id,
            name,
            CatalogSource {
                name: "api.kilo.ai/api/gateway/models (isFree)".into(),
                as_of: as_of.into(),
            },
        );
        row.tools = Some(has_param(item, "tools"));
        row.context_window = item.get("context_length").and_then(Value::as_u64);
        row.price = Some(Price::FREE);
        row.free_tier = FreeTier::Keyless;
        row.data_badge = if item.get("mayTrainOnYourPrompts").and_then(Value::as_bool) == Some(true)
        {
            DataBadge::MayTrain
        } else {
            DataBadge::Unknown
        };
        if let Some(exp) = str_of(item, "expiration_date") {
            row.note = Some(
                format!(
                    "{} Free access expires {exp}.",
                    row.note.clone().unwrap_or_default()
                )
                .trim()
                .to_string(),
            );
        }
        let order = item
            .get("preferredIndex")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX);
        rows.push((order, row));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.model_id.cmp(&b.1.model_id)));
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// OpenRouter `GET /api/v1/models`: free rows are `pricing.prompt == "0"` with a `:free` suffix
/// (or the `openrouter/free` router) that output text.
pub fn parse_openrouter_models(body: &str, as_of: &str) -> Result<Vec<CatalogModel>, ParseError> {
    let m = manifest("openrouter").expect("openrouter manifest");
    let v: Value = serde_json::from_str(body)?;
    let mut rows = Vec::new();
    for item in data_array(&v)? {
        let Some(id) = str_of(item, "id") else {
            continue;
        };
        let free_id = id.ends_with(":free") || id == "openrouter/free";
        if !free_id || !zero_price_str(item) || !outputs_text(item) {
            continue;
        }
        let name = str_of(item, "name").unwrap_or(id);
        let mut row = row_for(
            &m,
            id,
            name,
            CatalogSource {
                name: "openrouter.ai/api/v1/models (pricing.prompt == 0)".into(),
                as_of: as_of.into(),
            },
        );
        row.tools = Some(has_param(item, "tools"));
        row.context_window = item.get("context_length").and_then(Value::as_u64);
        row.price = Some(Price::FREE);
        row.free_tier = FreeTier::KeyRequired;
        rows.push(row);
    }
    rows.sort_by(|a, b| {
        b.tools
            .cmp(&a.tools)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    Ok(rows)
}

/// A plain OpenAI `GET /v1/models` list (`{"data":[{"id":…}]}`) for `m`. With `only`, keeps just
/// those ids (NVIDIA lists 80+ models, many not chat models).
pub fn parse_openai_models_list(
    m: &ProviderManifest,
    body: &str,
    as_of: &str,
    only: Option<&[&str]>,
) -> Result<Vec<CatalogModel>, ParseError> {
    let v: Value = serde_json::from_str(body)?;
    let mut rows = Vec::new();
    for item in data_array(&v)? {
        let Some(id) = str_of(item, "id") else {
            continue;
        };
        if let Some(only) = only
            && !only.contains(&id)
        {
            continue;
        }
        let mut row = row_for(
            m,
            id,
            id,
            CatalogSource {
                name: format!(
                    "{}/models",
                    m.base_url
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                ),
                as_of: as_of.into(),
            },
        );
        if only.is_some() {
            row.tools = Some(true);
        }
        rows.push(row);
    }
    if let Some(only) = only {
        rows.sort_by_key(|r| only.iter().position(|o| *o == r.model_id));
    }
    Ok(rows)
}

/// Models.dev-compatible `api.json` (`https://models.opencode.ai/api.json`): rows for one provider
/// with cost, tool support, and context window. Deprecated rows are skipped.
pub fn parse_models_dev(
    m: &ProviderManifest,
    body: &str,
    as_of: &str,
    source_name: &str,
) -> Result<Vec<CatalogModel>, ParseError> {
    let v: Value = serde_json::from_str(body)?;
    let provider = v
        .get(&m.id)
        .ok_or(ParseError::Shape("provider id not present in api.json"))?;
    let models = provider
        .get("models")
        .and_then(Value::as_object)
        .ok_or(ParseError::Shape("provider.models is not an object"))?;
    let mut rows = Vec::new();
    for (id, item) in models {
        if str_of(item, "status") == Some("deprecated") {
            continue;
        }
        let name = str_of(item, "name").unwrap_or(id);
        let mut row = row_for(
            m,
            id,
            name,
            CatalogSource {
                name: source_name.into(),
                as_of: as_of.into(),
            },
        );
        let input = item.pointer("/cost/input").and_then(Value::as_f64);
        let output = item.pointer("/cost/output").and_then(Value::as_f64);
        row.price = match (input, output) {
            (Some(i), Some(o)) => Some(Price {
                input: i,
                output: o,
            }),
            _ => None,
        };
        // A zero price here says nothing about keyless reachability; the manifest decides.
        row.free_tier = if row.price.is_some_and(|p| p.is_zero()) && m.free_tier != FreeTier::None {
            m.free_tier
        } else {
            FreeTier::None
        };
        row.tools = item.get("tool_call").and_then(Value::as_bool);
        row.context_window = item.pointer("/limit/context").and_then(Value::as_u64);
        rows.push(row);
    }
    rows.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/catalog")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn kilo_free_rows_are_keyless_and_ordered() {
        let rows = parse_kilo_models(&fixture("kilo-models.json"), "2026-09-21").unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.model_id.as_str()).collect();
        assert!(ids.contains(&"kilo-auto/free") && ids.contains(&"openrouter/free"));
        assert!(
            !ids.iter().any(|i| !i.ends_with(":free")
                && *i != "kilo-auto/free"
                && *i != "openrouter/free"),
            "paid row leaked: {ids:?}"
        );
        let auto = rows
            .iter()
            .find(|r| r.model_id == "kilo-auto/free")
            .unwrap();
        assert!(auto.is_keyless() && auto.is_free());
        assert_eq!(auto.data_badge, DataBadge::MayTrain);
        assert_eq!(auto.context_window, Some(256_000));
        let glm = rows
            .iter()
            .find(|r| r.model_id == "z-ai/glm-5.2:free")
            .unwrap();
        assert_eq!(glm.tools, Some(false), "GLM 5.2 free advertises no tools");
        let ultra = rows
            .iter()
            .find(|r| r.model_id == "nvidia/nemotron-3-ultra-550b-a55b:free")
            .unwrap();
        assert_eq!(ultra.tools, Some(true));
        assert_eq!(ultra.context_window, Some(1_000_000));
        assert!(ultra.badge_line().starts_with("Free · No sign-in"));
    }

    #[test]
    fn openrouter_free_rows_need_a_key_and_skip_non_text_and_paid() {
        let rows =
            parse_openrouter_models(&fixture("openrouter-models.json"), "2026-09-21").unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.model_id.as_str()).collect();
        assert!(ids.contains(&"openrouter/free"));
        assert!(ids.contains(&"google/gemma-4-31b-it:free"));
        assert!(
            !ids.contains(&"google/lyria-3-pro-preview"),
            "audio model without :free is not a free chat row"
        );
        assert!(
            !ids.contains(&"xiaomi/mimo-v2.6-pro-ultraspeed"),
            "priced row"
        );
        for r in &rows {
            assert_eq!(r.free_tier, FreeTier::KeyRequired);
            assert!(r.is_free() && !r.is_keyless());
        }
        assert_eq!(rows[0].tools, Some(true), "tool-capable rows first");
        let safety = rows
            .iter()
            .find(|r| r.model_id == "nvidia/nemotron-3.5-content-safety:free")
            .unwrap();
        assert_eq!(safety.tools, Some(false));
    }

    #[test]
    fn nvidia_list_is_filtered_to_coding_models_in_seed_order() {
        let m = manifest("nvidia").unwrap();
        let rows = parse_openai_models_list(
            &m,
            &fixture("nvidia-models.json"),
            "2026-09-21",
            Some(&super::super::NVIDIA_CODING_MODELS),
        )
        .unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.model_id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "nvidia/nemotron-3-super-120b-a12b",
                "moonshotai/kimi-k3",
                "z-ai/glm-5.3-flash"
            ]
        );
        assert!(
            rows.iter()
                .all(|r| r.free_tier == FreeTier::KeyRequired && r.tools == Some(true))
        );
        let all = parse_openai_models_list(&m, &fixture("nvidia-models.json"), "2026-09-21", None)
            .unwrap();
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn models_dev_rows_carry_cost_tools_and_context() {
        let zen = manifest("opencode").unwrap();
        let rows = parse_models_dev(
            &zen,
            &fixture("models-dev.json"),
            "2026-09-21",
            "models.opencode.ai/api.json",
        )
        .unwrap();
        let pickle = rows.iter().find(|r| r.model_id == "big-pickle").unwrap();
        assert_eq!(pickle.price, Some(Price::FREE));
        assert_eq!(
            pickle.free_tier,
            FreeTier::None,
            "zero price on Zen is not reachable from Workshop"
        );
        assert!(!pickle.is_keyless());
        assert_eq!(pickle.tools, Some(true));
        assert_eq!(pickle.context_window, Some(200_000));
        let nano = rows.iter().find(|r| r.model_id == "gpt-5-nano").unwrap();
        assert!(nano.price.is_some_and(|p| !p.is_zero()));

        let or = manifest("openrouter").unwrap();
        let rows = parse_models_dev(
            &or,
            &fixture("models-dev.json"),
            "2026-09-21",
            "models.opencode.ai/api.json",
        )
        .unwrap();
        let free = rows
            .iter()
            .find(|r| r.model_id == "openrouter/free")
            .unwrap();
        assert_eq!(
            free.free_tier,
            FreeTier::KeyRequired,
            "provider free tier applies to zero-price rows"
        );
        let sonnet = rows
            .iter()
            .find(|r| r.model_id == "anthropic/claude-sonnet-4.6")
            .unwrap();
        assert_eq!(sonnet.free_tier, FreeTier::None);
        assert!(
            parse_models_dev(
                &manifest("kilo").unwrap(),
                &fixture("models-dev.json"),
                "x",
                "y"
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_bodies_are_errors_not_panics() {
        assert!(parse_kilo_models("not json", "x").is_err());
        assert!(parse_kilo_models(r#"{"models":[]}"#, "x").is_err());
        assert!(parse_openrouter_models(r#"[]"#, "x").is_err());
        assert!(
            parse_openai_models_list(
                &manifest("nvidia").unwrap(),
                r#"{"data":"nope"}"#,
                "x",
                None
            )
            .is_err()
        );
    }
}
