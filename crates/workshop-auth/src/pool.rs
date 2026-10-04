//! The keyless community pool as a connection of its own (the `direct` free-models mode: no
//! OpenCode engine). The pool is the Kilo Gateway's anonymous `:free` list — the same rows the
//! silent fallback draws on — read from the live list the picker caches, else the compiled seed.
//! This module decides which rows are offered, which one a first run lands on, which one takes
//! over when the current one is gone or rate-limited, and which one sees images.

use workshop_providers::{Catalog, CatalogModel, KILO_DEFAULT_CHAIN};

use crate::is_chat_model_name;

/// The pool's provider id in the catalog.
pub const POOL_PROVIDER_ID: &str = "kilo";
/// Group header of the pool rows on `/model` (never a vendor or gateway name).
pub const POOL_GROUP: &str = "Community pool";
/// The pool rows' badge: honest about what the rows are and where prompts go.
pub const POOL_BADGE: &str = "Free · no sign-in · shared pool · the provider may log prompts";

/// Vision rows, best first (the live list decides which of them exist today).
const VISION_PREFERENCE: [&str; 3] = [
    "qwen/qwen3.8-27b:free",
    "stepfun/step-3.7-flash:free",
    "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
];

/// `kilo-auto/free` and `openrouter/free` route to whatever is free today: fine behind the
/// scenes, but a row must name the model that answers.
pub fn is_pool_router(model_id: &str) -> bool {
    model_id.ends_with("/free")
}

fn offered(m: &CatalogModel) -> bool {
    m.provider_id == POOL_PROVIDER_ID
        && m.is_free()
        && m.is_keyless()
        && !is_pool_router(&m.model_id)
        && m.tools != Some(false)
        && is_chat_model_name(&m.model_id)
        && is_chat_model_name(&m.display_name)
}

fn chain_rank(model_id: &str) -> usize {
    KILO_DEFAULT_CHAIN
        .iter()
        .position(|id| *id == model_id)
        .unwrap_or(KILO_DEFAULT_CHAIN.len())
}

/// The pool rows worth offering, the default chain first, then by name: free, keyless, chat
/// models that advertise tools (or do not say), never a router.
pub fn pool_rows(catalog: &Catalog) -> Vec<CatalogModel> {
    let mut rows: Vec<CatalogModel> = catalog
        .rows_for(POOL_PROVIDER_ID)
        .into_iter()
        .filter(|m| offered(m))
        .cloned()
        .collect();
    rows.sort_by(|a, b| {
        chain_rank(&a.model_id)
            .cmp(&chain_rank(&b.model_id))
            .then_with(|| {
                a.display_name
                    .to_lowercase()
                    .cmp(&b.display_name.to_lowercase())
            })
    });
    rows
}

/// The row a first run lands on: the first concrete model of the default chain that the list
/// offers, else the first offered row.
pub fn pool_default(rows: &[CatalogModel]) -> Option<CatalogModel> {
    rows.first().cloned()
}

/// The row that takes over from `current` (gone from the list, or rate-limited): the next one
/// in chain order, never `current` itself.
pub fn pool_next(rows: &[CatalogModel], current: &str) -> Option<CatalogModel> {
    let at = rows.iter().position(|m| m.model_id == current);
    let after = at.map_or(0, |i| i + 1);
    rows.iter()
        .cycle()
        .skip(after)
        .take(rows.len())
        .find(|m| m.model_id != current)
        .cloned()
}

/// The row that sees images: the best-ranked preferred one the list offers, else any offered
/// row that says it accepts images. `None` when the pool has no vision row today.
pub fn pool_vision(rows: &[CatalogModel]) -> Option<CatalogModel> {
    VISION_PREFERENCE
        .iter()
        .find_map(|id| {
            rows.iter()
                .find(|m| m.model_id == *id && m.image_input == Some(true))
                .cloned()
        })
        .or_else(|| rows.iter().find(|m| m.image_input == Some(true)).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Catalog {
        Catalog::builtin()
    }

    #[test]
    fn offered_rows_skip_routers_and_toolless_models_and_lead_with_the_chain() {
        let rows = pool_rows(&seed());
        let ids: Vec<&str> = rows.iter().map(|r| r.model_id.as_str()).collect();
        assert_eq!(ids[0], "nvidia/nemotron-3-super-120b-a12b:free", "{ids:?}");
        assert!(!ids.iter().any(|i| is_pool_router(i)), "{ids:?}");
        assert!(!ids.contains(&"z-ai/glm-5.2:free"), "no tools: {ids:?}");
        assert!(rows.iter().all(|r| r.is_keyless() && r.is_free()));
        assert_eq!(
            pool_default(&rows).unwrap().model_id,
            "nvidia/nemotron-3-super-120b-a12b:free"
        );
    }

    #[test]
    fn next_walks_the_list_after_the_current_row_and_never_returns_it() {
        let rows = pool_rows(&seed());
        let first = &rows[0].model_id;
        let next = pool_next(&rows, first).unwrap();
        assert_ne!(&next.model_id, first);
        assert_eq!(next.model_id, rows[1].model_id);
        // From the last row it wraps to the first.
        let last = &rows[rows.len() - 1].model_id;
        assert_eq!(pool_next(&rows, last).unwrap().model_id, rows[0].model_id);
        // A current model that is not listed any more: the first row.
        assert_eq!(
            pool_next(&rows, "gone/model:free").unwrap().model_id,
            rows[0].model_id
        );
        assert!(pool_next(&rows[..1], first).is_none());
    }

    #[test]
    fn vision_prefers_the_ranked_rows_and_needs_image_input() {
        let rows = pool_rows(&seed());
        assert_eq!(
            pool_vision(&rows).unwrap().model_id,
            "qwen/qwen3.8-27b:free"
        );
        let text_only: Vec<CatalogModel> = rows
            .iter()
            .filter(|r| r.image_input != Some(true))
            .cloned()
            .collect();
        assert!(pool_vision(&text_only).is_none());
    }
}
