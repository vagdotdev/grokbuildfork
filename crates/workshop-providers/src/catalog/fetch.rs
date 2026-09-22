//! Fetch provider model lists with an on-disk cache.
//!
//! Every fetch is a keyless `GET` (keyed list endpoints are seeded instead); responses are cached
//! under `<cache_dir>/<provider_id>.json` with the fetch time. A failed fetch falls back to the
//! cache (even stale), and then to the shipped seeds, so the picker always has rows and every row
//! states where and when it came from.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{
    CatalogModel, google_seed_models, kilo_seed_models, nvidia_seed_models, openrouter_seed_models,
    sources,
};
use crate::manifest::{ModelCatalogSource, ProviderManifest};

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error(transparent)]
    Parse(#[from] sources::ParseError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} has no keyless catalog endpoint; use its seed rows")]
    NoKeylessSource(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    fetched_at_secs: u64,
    url: String,
    body: String,
}

/// Where a returned row set came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Live,
    Cached,
    StaleCache,
    Seed,
}

#[derive(Debug, Clone)]
pub struct FetchedCatalog {
    pub provider_id: String,
    pub rows: Vec<CatalogModel>,
    pub freshness: Freshness,
    /// Unix seconds of the underlying fetch (`None` for seeds).
    pub fetched_at_secs: Option<u64>,
    /// Why a live fetch was not used, when applicable.
    pub error: Option<String>,
}

pub struct CatalogFetcher {
    client: reqwest::Client,
    cache_dir: PathBuf,
    ttl: Duration,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn iso_date(secs: u64) -> String {
    // Days since epoch → civil date (Howard Hinnant's algorithm); enough for an `as_of` stamp.
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Seed rows for a provider (what ships in the crate).
pub fn seed_rows(provider_id: &str) -> Vec<CatalogModel> {
    match provider_id {
        "kilo" => kilo_seed_models(),
        "openrouter" => openrouter_seed_models(),
        "google" => google_seed_models(),
        "nvidia" => nvidia_seed_models(),
        _ => Vec::new(),
    }
}

/// Parse a raw list body for `m` into rows.
pub fn parse_for(
    m: &ProviderManifest,
    body: &str,
    as_of: &str,
) -> Result<Vec<CatalogModel>, sources::ParseError> {
    match (&m.model_catalog_source, m.id.as_str()) {
        (_, "kilo") => sources::parse_kilo_models(body, as_of),
        (_, "openrouter") => sources::parse_openrouter_models(body, as_of),
        (_, "nvidia") => {
            sources::parse_openai_models_list(m, body, as_of, Some(&super::NVIDIA_CODING_MODELS))
        }
        (ModelCatalogSource::ModelsDev { url }, _) => {
            sources::parse_models_dev(m, body, as_of, url)
        }
        (ModelCatalogSource::ModelsEndpoint { .. }, _) => {
            sources::parse_openai_models_list(m, body, as_of, None)
        }
        (ModelCatalogSource::Builtin, _) => Ok(seed_rows(&m.id)),
    }
}

impl CatalogFetcher {
    /// `cache_dir` is normally `<workshop home>/catalog-cache`. Uses the workspace TLS policy.
    pub fn new(cache_dir: impl Into<PathBuf>, ttl: Duration) -> Result<Self, FetchError> {
        let client = xai_grok_extra_ca::build_reqwest_client(|b| {
            b.timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(8))
                .user_agent("workshop-providers/0.1")
        })?;
        Ok(Self {
            client,
            cache_dir: cache_dir.into(),
            ttl,
        })
    }

    pub fn cache_path(&self, provider_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{provider_id}.json"))
    }

    fn keyless_url(m: &ProviderManifest) -> Option<String> {
        match &m.model_catalog_source {
            ModelCatalogSource::ModelsEndpoint { url, keyless: true } => Some(url.clone()),
            ModelCatalogSource::ModelsDev { url } => Some(url.clone()),
            _ => None,
        }
    }

    fn read_cache(&self, provider_id: &str) -> Option<CacheEntry> {
        let text = std::fs::read_to_string(self.cache_path(provider_id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn write_cache(&self, provider_id: &str, entry: &CacheEntry) -> Result<(), FetchError> {
        let body = serde_json::to_vec(entry).expect("cache entry serializes");
        crate::config::atomic_write_private(&self.cache_path(provider_id), &body)
            .map_err(|e| FetchError::Io(std::io::Error::other(e.to_string())))
    }

    /// Fetch one provider's rows: fresh cache → live → stale cache → seeds.
    pub async fn fetch(&self, m: &ProviderManifest) -> FetchedCatalog {
        let provider_id = m.id.clone();
        let seed = |error: Option<String>| FetchedCatalog {
            provider_id: provider_id.clone(),
            rows: seed_rows(&provider_id),
            freshness: Freshness::Seed,
            fetched_at_secs: None,
            error,
        };
        let Some(url) = Self::keyless_url(m) else {
            return seed(Some(
                FetchError::NoKeylessSource(provider_id.clone()).to_string(),
            ));
        };

        let cached = self.read_cache(&provider_id);
        if let Some(entry) = &cached
            && now_secs().saturating_sub(entry.fetched_at_secs) < self.ttl.as_secs()
            && entry.url == url
            && let Ok(rows) = parse_for(m, &entry.body, &iso_date(entry.fetched_at_secs))
        {
            return FetchedCatalog {
                provider_id,
                rows,
                freshness: Freshness::Cached,
                fetched_at_secs: Some(entry.fetched_at_secs),
                error: None,
            };
        }

        match self.fetch_live(m, &url).await {
            Ok((rows, entry)) => {
                let _ = self.write_cache(&provider_id, &entry);
                FetchedCatalog {
                    provider_id,
                    rows,
                    freshness: Freshness::Live,
                    fetched_at_secs: Some(entry.fetched_at_secs),
                    error: None,
                }
            }
            Err(e) => {
                if let Some(entry) = cached
                    && let Ok(rows) = parse_for(m, &entry.body, &iso_date(entry.fetched_at_secs))
                {
                    return FetchedCatalog {
                        provider_id,
                        rows,
                        freshness: Freshness::StaleCache,
                        fetched_at_secs: Some(entry.fetched_at_secs),
                        error: Some(e.to_string()),
                    };
                }
                seed(Some(e.to_string()))
            }
        }
    }

    async fn fetch_live(
        &self,
        m: &ProviderManifest,
        url: &str,
    ) -> Result<(Vec<CatalogModel>, CacheEntry), FetchError> {
        let resp = self.client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(FetchError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let body = resp.text().await?;
        let fetched_at_secs = now_secs();
        let rows = parse_for(m, &body, &iso_date(fetched_at_secs))?;
        Ok((
            rows,
            CacheEntry {
                fetched_at_secs,
                url: url.to_string(),
                body,
            },
        ))
    }

    /// Clear one provider's cache (e.g. `Manage models → Refresh`).
    pub fn invalidate(&self, provider_id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.cache_path(provider_id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Convenience: is `dir` a usable cache location (created on demand).
pub fn default_cache_dir(workshop_home: &Path) -> PathBuf {
    workshop_home.join("catalog-cache")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_date_is_correct() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_790_000_000), "2026-09-21");
        assert_eq!(iso_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn seeds_exist_for_every_free_provider() {
        for id in ["kilo", "openrouter", "google", "nvidia"] {
            assert!(!seed_rows(id).is_empty(), "{id}");
        }
        assert!(seed_rows("opencode").is_empty());
    }

    #[tokio::test]
    async fn cache_is_used_when_fresh_and_when_the_network_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher =
            CatalogFetcher::new(tmp.path().join("cache"), Duration::from_secs(3600)).unwrap();
        // Point Kilo at an unroutable URL so the live fetch fails fast.
        let mut m = crate::manifest::manifest("kilo").unwrap();
        m.model_catalog_source = ModelCatalogSource::ModelsEndpoint {
            url: "http://127.0.0.1:9/models".into(),
            keyless: true,
        };
        // Nothing cached → seeds.
        let r = fetcher.fetch(&m).await;
        assert_eq!(r.freshness, Freshness::Seed);
        assert!(r.error.is_some());
        assert!(!r.rows.is_empty());

        // Plant a fresh cache entry with the captured fixture body.
        let body = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog/kilo-models.json"),
        )
        .unwrap();
        fetcher
            .write_cache(
                "kilo",
                &CacheEntry {
                    fetched_at_secs: now_secs(),
                    url: "http://127.0.0.1:9/models".into(),
                    body: body.clone(),
                },
            )
            .unwrap();
        let r = fetcher.fetch(&m).await;
        assert_eq!(r.freshness, Freshness::Cached);
        assert!(r.rows.iter().any(|row| row.model_id == "kilo-auto/free"));

        // Stale cache + failing network → stale cache with the error attached.
        fetcher
            .write_cache(
                "kilo",
                &CacheEntry {
                    fetched_at_secs: 1_000,
                    url: "http://127.0.0.1:9/models".into(),
                    body,
                },
            )
            .unwrap();
        let r = fetcher.fetch(&m).await;
        assert_eq!(r.freshness, Freshness::StaleCache);
        assert!(r.error.is_some());
        assert!(r.rows.iter().any(|row| row.source.as_of == "1970-01-01"));

        fetcher.invalidate("kilo").unwrap();
        assert!(!fetcher.cache_path("kilo").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.path().join("cache"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[tokio::test]
    async fn keyed_list_endpoints_fall_back_to_seeds() {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = CatalogFetcher::new(tmp.path(), Duration::from_secs(3600)).unwrap();
        let google = crate::manifest::manifest("google").unwrap();
        let r = fetcher.fetch(&google).await;
        assert_eq!(r.freshness, Freshness::Seed);
        assert!(r.rows.iter().any(|row| row.model_id == "gemini-3.5-flash"));
    }
}
