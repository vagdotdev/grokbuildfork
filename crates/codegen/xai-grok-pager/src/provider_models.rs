//! Provider model discovery and caching.
//!
//! Fetches the real model list from each docked provider's API, caches results
//! on disk with a 1-hour TTL, and exposes the discovered models for use during
//! bootstrap.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A model discovered from a provider's API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: Option<String>,
    pub context_window: Option<u64>,
    pub created: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    fetched_at: u64,
    models: Vec<DiscoveredModel>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    providers: HashMap<String, CacheEntry>,
}

const CACHE_TTL_SECS: u64 = 3600; // 1 hour

fn cache_path() -> PathBuf {
    xai_grok_config::grok_home().join("provider_models_cache.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_cache() -> CacheFile {
    let path = cache_path();
    match std::fs::read_to_string(&path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => CacheFile::default(),
    }
}

fn write_cache(cache: &CacheFile) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_vec_pretty(cache) {
        let _ = std::fs::write(&path, body);
    }
}

/// Fetch the model list from a provider's API.
///
/// Runs the blocking HTTP request on a dedicated thread so it never conflicts
/// with an existing tokio runtime (the CLI's `provider list` runs inside tokio).
pub fn fetch_provider_models(
    provider_id: &str,
    base_url: &str,
    api_key: &str,
    _backend: &str,
) -> Result<Vec<DiscoveredModel>> {
    let provider_id = provider_id.to_owned();
    let base_url = base_url.to_owned();
    let api_key = api_key.to_owned();
    let handle = std::thread::spawn(move || {
        fetch_provider_models_inner(&provider_id, &base_url, &api_key)
    });
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("model fetch thread panicked"))?
}

fn fetch_provider_models_inner(
    provider_id: &str,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<DiscoveredModel>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let mut request = client.get(&url);

    if provider_id == "anthropic" {
        request = request
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01");
    } else {
        request = request.header("Authorization", format!("Bearer {api_key}"));
    }

    let response = request.send().context("model list request failed")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "model list request returned HTTP {}",
            response.status().as_u16()
        );
    }

    let body: serde_json::Value = response.json().context("failed to parse model list JSON")?;

    // Both OpenAI-compatible and Anthropic APIs return { "data": [...] }.
    let models_array = body
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    let mut models: Vec<DiscoveredModel> = models_array
        .into_iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|v| v.as_str())?.to_owned();
            let name = item
                .get("name")
                .or_else(|| item.get("display_name"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let context_window = item
                .get("context_window")
                .or_else(|| item.get("context_length"))
                .and_then(|v| v.as_u64());
            let created = item
                .get("created")
                .or_else(|| item.get("created_at"))
                .and_then(|v| v.as_i64());
            Some(DiscoveredModel {
                id,
                name,
                context_window,
                created,
            })
        })
        .collect();

    // Sort by creation date descending (newest first) when available.
    models.sort_by(|a, b| b.created.cmp(&a.created));

    Ok(models)
}

/// Returns true if the model ID looks like a chat/completion model (not
/// embedding, moderation, tts, whisper, dall-e, etc.).
pub fn is_chat_model(model_id: &str) -> bool {
    let id = model_id.to_ascii_lowercase();
    // Exclude known non-chat model families.
    let excluded = [
        "embed",
        "moderat",
        "tts",
        "whisper",
        "dall-e",
        "davinci",
        "babbage",
        "ada",
        "text-search",
        "text-similarity",
        "code-search",
        "curie",
    ];
    if excluded.iter().any(|pat| id.contains(pat)) {
        return false;
    }
    true
}

/// Fetch models for all docked providers, using the disk cache when fresh.
///
/// Reads provider metadata + secrets from the local vault, fetches from each
/// ready provider (or returns cached results if within TTL), and writes the
/// cache back.
pub fn cached_or_fetch_all_provider_models() -> Vec<(String, Vec<DiscoveredModel>)> {
    let store = match crate::provider_cmd::list_providers() {
        Ok(providers) => providers,
        Err(_) => return Vec::new(),
    };

    let mut cache = read_cache();
    let now = now_secs();
    let mut results: Vec<(String, Vec<DiscoveredModel>)> = Vec::new();

    for provider in &store {
        if provider.status != crate::provider_cmd::ProviderStatus::Ready {
            continue;
        }

        // Check cache freshness.
        if let Some(entry) = cache.providers.get(&provider.id) {
            if now.saturating_sub(entry.fetched_at) < CACHE_TTL_SECS {
                results.push((provider.id.clone(), entry.models.clone()));
                continue;
            }
        }

        // Resolve spec and secret for fetching.
        let spec = match provider_spec_for_fetch(&provider.id) {
            Some(s) => s,
            None => continue,
        };

        let api_key = match xai_grok_shell::secure_store::get_secret(&provider.id) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => continue,
        };

        match fetch_provider_models(&provider.id, spec.base_url, &api_key, spec.backend) {
            Ok(models) => {
                cache.providers.insert(
                    provider.id.clone(),
                    CacheEntry {
                        fetched_at: now,
                        models: models.clone(),
                    },
                );
                results.push((provider.id.clone(), models));
            }
            Err(e) => {
                tracing::warn!(
                    provider = %provider.id,
                    error = %e,
                    "failed to fetch model list; using cached/default"
                );
                // Use stale cache if available.
                if let Some(entry) = cache.providers.get(&provider.id) {
                    results.push((provider.id.clone(), entry.models.clone()));
                }
            }
        }
    }

    write_cache(&cache);
    results
}

/// Minimal spec info needed for model fetching.
struct FetchSpec {
    base_url: &'static str,
    backend: &'static str,
}

fn provider_spec_for_fetch(id: &str) -> Option<FetchSpec> {
    match id {
        "openrouter" => Some(FetchSpec {
            base_url: "https://openrouter.ai/api/v1",
            backend: "chat_completions",
        }),
        "openai" => Some(FetchSpec {
            base_url: "https://api.openai.com/v1",
            backend: "responses",
        }),
        "anthropic" => Some(FetchSpec {
            base_url: "https://api.anthropic.com/v1",
            backend: "messages",
        }),
        "xai" => Some(FetchSpec {
            base_url: "https://api.x.ai/v1",
            backend: "responses",
        }),
        "groq" => Some(FetchSpec {
            base_url: "https://api.groq.com/openai/v1",
            backend: "chat_completions",
        }),
        "deepseek" => Some(FetchSpec {
            base_url: "https://api.deepseek.com/v1",
            backend: "chat_completions",
        }),
        "mistral" => Some(FetchSpec {
            base_url: "https://api.mistral.ai/v1",
            backend: "chat_completions",
        }),
        "gemini" => Some(FetchSpec {
            base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
            backend: "chat_completions",
        }),
        "together" => Some(FetchSpec {
            base_url: "https://api.together.xyz/v1",
            backend: "chat_completions",
        }),
        "fireworks" => Some(FetchSpec {
            base_url: "https://api.fireworks.ai/inference/v1",
            backend: "chat_completions",
        }),
        "perplexity" => Some(FetchSpec {
            base_url: "https://api.perplexity.ai",
            backend: "chat_completions",
        }),
        _ => None,
    }
}
