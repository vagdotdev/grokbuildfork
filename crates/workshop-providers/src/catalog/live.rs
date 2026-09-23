//! Live model lists for the picker.
//!
//! The Models tab shows what each provider offers *now*: the hosted providers with a keyless list
//! endpoint ([`REFRESH_PROVIDERS`]) are fetched concurrently with a short deadline and cached under
//! `<workshop home>/catalog-cache/`; the compiled seeds only stand in while nothing has been
//! fetched yet (offline first run, a provider that is down). Every list carries a
//! [`CatalogStatus`] so the UI can say `fetched 3 min ago` or `cached list from 2026-09-21`.
//!
//! Nothing here runs on its own: [`load_cached`] never touches the network, and [`refresh`] is
//! called only after the user acted (an active connection at startup, opening `/model`, `r`).
//! Keyed list endpoints (Google, OpenAI, Anthropic) are never fetched; their rows are seeds.

use std::path::Path;
use std::time::Duration;

use super::fetch::{self, CatalogFetcher, FetchedCatalog, Freshness, now_secs, seed_rows};
use super::{Catalog, SEED_REVIEWED};
use crate::manifest::{ProviderManifest, manifest};

/// Hosted providers whose model list is fetched live (keyless `GET`), in picker order.
pub const REFRESH_PROVIDERS: [&str; 3] = ["kilo", "openrouter", "nvidia"];
/// Providers shown from the compiled seed only (their list endpoint needs a key).
const SEED_ONLY_PROVIDERS: [&str; 1] = ["google"];

/// Lists younger than this are not re-fetched when `/model` opens (`r` forces a fetch).
pub const PICKER_MAX_AGE: Duration = Duration::from_secs(5 * 60);

/// Ceilings for one refresh: the whole batch, one request, one connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshOptions {
    /// Cached lists younger than this are used as they are (`Duration::ZERO` forces a fetch).
    pub max_age: Duration,
    /// Total budget for the batch; whatever has not landed by then falls back to cache/seed.
    pub deadline: Duration,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for RefreshOptions {
    fn default() -> Self {
        Self {
            max_age: PICKER_MAX_AGE,
            deadline: Duration::from_secs(10),
            request_timeout: Duration::from_secs(8),
            connect_timeout: Duration::from_secs(4),
        }
    }
}

impl RefreshOptions {
    /// Ignore the cache age: every list is fetched again (the picker's `r`).
    pub fn forced() -> Self {
        Self {
            max_age: Duration::ZERO,
            ..Self::default()
        }
    }
}

/// Where one provider's rows came from and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogStatus {
    pub provider_id: String,
    pub freshness: Freshness,
    /// Unix seconds of the fetch the rows come from (`None` for seeds).
    pub fetched_at_secs: Option<u64>,
    pub rows: usize,
    /// Why the last live fetch was not used, when it was attempted and failed.
    pub error: Option<String>,
}

impl CatalogStatus {
    pub fn seed(provider_id: &str, rows: usize) -> Self {
        Self {
            provider_id: provider_id.to_owned(),
            freshness: Freshness::Seed,
            fetched_at_secs: None,
            rows,
            error: None,
        }
    }

    pub fn from_fetched(f: &FetchedCatalog) -> Self {
        Self {
            provider_id: f.provider_id.clone(),
            freshness: f.freshness,
            fetched_at_secs: f.fetched_at_secs,
            rows: f.rows.len(),
            error: f.error.clone(),
        }
    }

    /// The short freshness note shown next to the list: `fetched 3 min ago`, or
    /// `cached list from 2026-09-21` for a seed; ` · refresh failed` when a fetch was tried.
    pub fn note(&self) -> String {
        self.note_at(now_secs())
    }

    pub fn note_at(&self, now_secs: u64) -> String {
        let mut note = match (self.freshness, self.fetched_at_secs) {
            (Freshness::Seed, _) | (_, None) => format!("cached list from {SEED_REVIEWED}"),
            (_, Some(at)) => format!("fetched {}", relative_age(now_secs.saturating_sub(at))),
        };
        if self.error.is_some() {
            note.push_str(" · refresh failed");
        }
        note
    }
}

/// `just now`, `3 min ago`, `2 h ago`, `1 day ago`, `4 days ago`.
pub fn relative_age(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", secs / 60),
        3_600..=86_399 => format!("{} h ago", secs / 3_600),
        86_400..=172_799 => "1 day ago".to_owned(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// The hosted rows the picker shows plus where each provider's list came from.
#[derive(Debug, Clone, PartialEq)]
pub struct HostedCatalogs {
    /// Seeds with every fetched (or cached) provider's rows swapped in.
    pub catalog: Catalog,
    pub status: Vec<CatalogStatus>,
}

fn refresh_manifests() -> Vec<ProviderManifest> {
    REFRESH_PROVIDERS
        .iter()
        .filter_map(|id| manifest(id))
        .collect()
}

fn assemble(fetched: Vec<FetchedCatalog>) -> HostedCatalogs {
    let mut catalog = Catalog::builtin();
    let mut status = Vec::with_capacity(fetched.len() + SEED_ONLY_PROVIDERS.len());
    for f in fetched {
        if f.freshness.is_fetched() {
            catalog.replace_provider(&f.provider_id, f.rows.clone());
        }
        status.push(CatalogStatus::from_fetched(&f));
    }
    for id in SEED_ONLY_PROVIDERS {
        status.push(CatalogStatus::seed(id, seed_rows(id).len()));
    }
    HostedCatalogs { catalog, status }
}

fn seed_catalog(m: &ProviderManifest, error: Option<String>) -> FetchedCatalog {
    FetchedCatalog {
        provider_id: m.id.clone(),
        rows: seed_rows(&m.id),
        freshness: Freshness::Seed,
        fetched_at_secs: None,
        error,
    }
}

/// The last fetched lists from `cache_dir` (any age), seeds for the rest. Never uses the network.
pub fn load_cached(cache_dir: &Path) -> HostedCatalogs {
    load_cached_with(cache_dir, &refresh_manifests(), PICKER_MAX_AGE)
}

/// [`load_cached`] for an explicit manifest set (tests point the list URLs at a mock).
pub fn load_cached_with(
    cache_dir: &Path,
    manifests: &[ProviderManifest],
    max_age: Duration,
) -> HostedCatalogs {
    assemble(
        manifests
            .iter()
            .map(|m| fetch::cached(cache_dir, m, max_age).unwrap_or_else(|| seed_catalog(m, None)))
            .collect(),
    )
}

/// Refresh every list older than `opts.max_age` from its live source (concurrently, within
/// `opts.deadline`), caching each success. Failures keep the last cached list (any age), then the
/// seed, with the error attached to that provider's status.
pub async fn refresh(cache_dir: &Path, opts: RefreshOptions) -> HostedCatalogs {
    refresh_with(cache_dir, &refresh_manifests(), opts).await
}

/// [`refresh`] for an explicit manifest set (tests point the list URLs at a mock).
pub async fn refresh_with(
    cache_dir: &Path,
    manifests: &[ProviderManifest],
    opts: RefreshOptions,
) -> HostedCatalogs {
    let fetcher = match CatalogFetcher::with_timeouts(
        cache_dir,
        opts.max_age,
        opts.request_timeout,
        opts.connect_timeout,
    ) {
        Ok(f) => f,
        Err(e) => {
            let mut out = load_cached_with(cache_dir, manifests, opts.max_age);
            let reason = format!("no HTTP client: {e}");
            for s in &mut out.status {
                if REFRESH_PROVIDERS.contains(&s.provider_id.as_str()) {
                    s.error = Some(reason.clone());
                }
            }
            return out;
        }
    };
    let batch = futures_util::future::join_all(manifests.iter().map(|m| fetcher.fetch(m)));
    match tokio::time::timeout(opts.deadline, batch).await {
        Ok(fetched) => assemble(fetched),
        Err(_) => {
            // Whatever landed before the deadline is in the cache (young enough to count as
            // `Cached` even on a forced refresh); the rest is reported as such.
            let mut out = load_cached_with(cache_dir, manifests, opts.max_age.max(PICKER_MAX_AGE));
            for s in &mut out.status {
                if REFRESH_PROVIDERS.contains(&s.provider_id.as_str())
                    && s.freshness != Freshness::Cached
                {
                    s.error = Some(format!("no answer within {} s", opts.deadline.as_secs()));
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ModelCatalogSource;
    use std::path::PathBuf;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/catalog")
                .join(name),
        )
        .unwrap()
    }

    /// The three refresh manifests with their list URLs pointed at `base`.
    fn manifests_at(base: &str) -> Vec<ProviderManifest> {
        refresh_manifests()
            .into_iter()
            .map(|mut m| {
                m.model_catalog_source = ModelCatalogSource::ModelsEndpoint {
                    url: format!("{base}/{}/models", m.id),
                    keyless: true,
                };
                m
            })
            .collect()
    }

    fn status<'a>(out: &'a HostedCatalogs, id: &str) -> &'a CatalogStatus {
        out.status
            .iter()
            .find(|s| s.provider_id == id)
            .unwrap_or_else(|| panic!("status for {id}"))
    }

    fn fast() -> RefreshOptions {
        RefreshOptions {
            max_age: PICKER_MAX_AGE,
            deadline: Duration::from_secs(5),
            request_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(1),
        }
    }

    async fn mock_all_ok() -> MockServer {
        let server = MockServer::start().await;
        for (id, file) in [
            ("kilo", "kilo-models.json"),
            ("openrouter", "openrouter-models.json"),
            ("nvidia", "nvidia-models.json"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/{id}/models")))
                .respond_with(ResponseTemplate::new(200).set_body_string(fixture(file)))
                .mount(&server)
                .await;
        }
        server
    }

    #[test]
    fn notes_say_when_and_where_the_rows_come_from() {
        let seed = CatalogStatus::seed("kilo", 6);
        assert_eq!(seed.note_at(1_000), "cached list from 2026-09-21");
        let live = CatalogStatus {
            provider_id: "kilo".into(),
            freshness: Freshness::Live,
            fetched_at_secs: Some(1_000),
            rows: 21,
            error: None,
        };
        assert_eq!(live.note_at(1_010), "fetched just now");
        assert_eq!(live.note_at(1_000 + 4 * 60), "fetched 4 min ago");
        assert_eq!(live.note_at(1_000 + 3 * 3_600), "fetched 3 h ago");
        assert_eq!(live.note_at(1_000 + 86_400), "fetched 1 day ago");
        assert_eq!(live.note_at(1_000 + 5 * 86_400), "fetched 5 days ago");
        let stale = CatalogStatus {
            freshness: Freshness::StaleCache,
            error: Some("http: boom".into()),
            ..live
        };
        assert_eq!(
            stale.note_at(1_000 + 120),
            "fetched 2 min ago · refresh failed"
        );
        let failed_seed = CatalogStatus {
            error: Some("HTTP 500".into()),
            ..seed
        };
        assert_eq!(
            failed_seed.note_at(0),
            "cached list from 2026-09-21 · refresh failed"
        );
    }

    #[test]
    fn nothing_cached_means_seeds_marked_as_such_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let out = load_cached(tmp.path());
        assert_eq!(out.catalog, Catalog::builtin());
        let ids: Vec<&str> = out.status.iter().map(|s| s.provider_id.as_str()).collect();
        assert_eq!(ids, ["kilo", "openrouter", "nvidia", "google"]);
        for s in &out.status {
            assert_eq!(s.freshness, Freshness::Seed, "{}", s.provider_id);
            assert!(s.rows > 0, "{}", s.provider_id);
            assert_eq!(s.note_at(0), "cached list from 2026-09-21");
        }
        assert!(
            !tmp.path().join("kilo.json").exists(),
            "an offline load writes nothing"
        );
    }

    #[tokio::test]
    async fn refresh_swaps_in_the_live_rows_and_caches_them_for_an_offline_load() {
        let server = mock_all_ok().await;
        let tmp = tempfile::tempdir().unwrap();
        let cache: PathBuf = tmp.path().join("catalog-cache");
        let manifests = manifests_at(&server.uri());

        let out = refresh_with(&cache, &manifests, fast()).await;
        let kilo = status(&out, "kilo");
        assert_eq!(kilo.freshness, Freshness::Live);
        assert!(kilo.error.is_none());
        assert!(kilo.note().starts_with("fetched just now"));
        // The fixture is the capture the seeds were reviewed from, so the row set matches; what
        // changes is provenance: every row is now stamped with the fetch, not the seed review.
        let live_kilo = out.catalog.rows_for("kilo");
        assert_eq!(live_kilo.len(), kilo.rows);
        assert!(!live_kilo.is_empty());
        assert!(
            live_kilo
                .iter()
                .all(|r| !r.source.name.contains("seed") && r.source.as_of != SEED_REVIEWED),
            "{:?}",
            live_kilo.iter().map(|r| &r.source).collect::<Vec<_>>()
        );
        assert!(
            out.catalog
                .rows_for("google")
                .iter()
                .all(|r| r.source.as_of == SEED_REVIEWED),
            "keyed providers keep their seeds"
        );
        assert_eq!(status(&out, "openrouter").freshness, Freshness::Live);
        assert_eq!(status(&out, "nvidia").freshness, Freshness::Live);
        assert_eq!(status(&out, "google").freshness, Freshness::Seed);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);

        // The next (offline) load is served from the cache: same rows, no request.
        let cached = load_cached_with(&cache, &manifests, PICKER_MAX_AGE);
        assert_eq!(cached.catalog, out.catalog);
        assert_eq!(status(&cached, "kilo").freshness, Freshness::Cached);
        assert_eq!(
            status(&cached, "kilo").fetched_at_secs,
            kilo.fetched_at_secs
        );
        // And a refresh within `max_age` does not fetch again.
        let again = refresh_with(&cache, &manifests, fast()).await;
        assert_eq!(status(&again, "kilo").freshness, Freshness::Cached);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        // Forced refresh fetches again.
        let forced = refresh_with(
            &cache,
            &manifests,
            RefreshOptions {
                max_age: Duration::ZERO,
                ..fast()
            },
        )
        .await;
        assert_eq!(status(&forced, "kilo").freshness, Freshness::Live);
        assert_eq!(server.received_requests().await.unwrap().len(), 6);
    }

    #[tokio::test]
    async fn a_failing_source_falls_back_to_the_seed_then_to_the_last_good_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/kilo/models"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        // openrouter / nvidia: no route → 404 → error too.
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("catalog-cache");
        let manifests = manifests_at(&server.uri());

        let out = refresh_with(&cache, &manifests, fast()).await;
        assert_eq!(out.catalog, Catalog::builtin(), "seeds stay in place");
        for id in REFRESH_PROVIDERS {
            let s = status(&out, id);
            assert_eq!(s.freshness, Freshness::Seed, "{id}");
            assert!(s.error.is_some(), "{id}");
            assert_eq!(
                s.note_at(0),
                "cached list from 2026-09-21 · refresh failed",
                "{id}"
            );
        }

        // A good fetch, then the source breaks: the last good list is kept (stale) with the error.
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/kilo/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture("kilo-models.json")))
            .mount(&server)
            .await;
        let good = refresh_with(&cache, &manifests, fast()).await;
        assert_eq!(status(&good, "kilo").freshness, Freshness::Live);
        let live_rows = good.catalog.rows_for("kilo").len();
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/kilo/models"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let broken = refresh_with(&cache, &manifests, RefreshOptions::forced()).await;
        let kilo = status(&broken, "kilo");
        assert_eq!(kilo.freshness, Freshness::StaleCache);
        assert!(kilo.error.as_deref().unwrap().contains("503"));
        assert_eq!(broken.catalog.rows_for("kilo").len(), live_rows);
        assert!(kilo.note().ends_with("· refresh failed"));
    }

    #[tokio::test]
    async fn a_slow_source_does_not_hold_the_picker_past_the_deadline() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/kilo/models"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fixture("kilo-models.json"))
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/nvidia/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(fixture("nvidia-models.json")))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("catalog-cache");
        let manifests = manifests_at(&server.uri());
        let opts = RefreshOptions {
            deadline: Duration::from_millis(1500),
            request_timeout: Duration::from_secs(30),
            ..fast()
        };
        let started = std::time::Instant::now();
        let out = refresh_with(&cache, &manifests, opts).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        let kilo = status(&out, "kilo");
        assert_eq!(kilo.freshness, Freshness::Seed);
        assert!(kilo.error.as_deref().unwrap().contains("no answer within"));
        // The fast source landed in the cache before the deadline and is used.
        let nvidia = status(&out, "nvidia");
        assert_eq!(nvidia.freshness, Freshness::Cached);
        assert!(nvidia.error.is_none());
    }
}
