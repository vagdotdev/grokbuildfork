//! The pinned voice artifacts: `voice/MODEL.lock.json`, compiled into the binary so the TUI's
//! self-heal downloads exactly what the installer installs. Three Whisper tiers are pinned;
//! which one a machine uses is decided by [`crate::tier`]. Changing a pin is a lock-file change,
//! and only then does anyone download again.

use serde::Deserialize;

/// Raw lock file (also read by scripts/install.sh and the release workflow).
pub const MODEL_LOCK_JSON: &str = include_str!("../../../voice/MODEL.lock.json");
/// Test hook: a path to a lock file that replaces the compiled-in pins (tiny models against a
/// loopback mirror). Read once, at the first use of the lock.
pub const LOCK_ENV: &str = "WORKSHOP_VOICE_LOCK";
/// Test/mirror hook: a flat base URL that serves the release assets (`SHA256SUMS`, the helper
/// archive, the model files) in place of the GitHub release of the running version.
pub const MIRROR_BASE_ENV: &str = "WORKSHOP_VOICE_MIRROR_BASE";

#[derive(Debug, Clone, Deserialize)]
pub struct ModelLockFile {
    pub schema_version: u32,
    /// Fastest-to-slowest is the reverse of this order: `turbo`, `small`, `base`.
    pub tiers: Vec<String>,
    pub models: std::collections::BTreeMap<String, ModelPin>,
    /// `{release_repo}`, `{version}` and `{file}` are substituted at run time.
    pub mirror_url_template: String,
    pub selection: SelectionParams,
    pub engine: EngineLock,
    pub license: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPin {
    pub name: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub upstream_url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SelectionParams {
    /// An interim decode must fit in this many ms for a tier to stay selected.
    pub interim_budget_ms: u64,
    pub min_ram_bytes_for_probe: u64,
    pub probe_ratio_small_over_base: f64,
    pub probe_ratio_turbo_over_base: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EngineLock {
    pub name: String,
    pub whisper_cpp_version: String,
    pub whisper_cpp_git: String,
    pub platforms: Vec<String>,
}

/// GitHub `OWNER/NAME` the release workflow bakes in (same variable the updater uses).
pub const RELEASE_REPO: &str = match option_env!("WORKSHOP_RELEASE_REPO") {
    Some(repo) => repo,
    None => "vagdotdev/grokbuildfork",
};

/// Version stamped by the release workflow; source builds have none and skip the mirror.
pub const RELEASE_VERSION: Option<&str> = match option_env!("WORKSHOP_VERSION") {
    Some(v) => Some(v),
    None => option_env!("GROK_VERSION"),
};

pub fn lock() -> &'static ModelLockFile {
    static LOCK: std::sync::OnceLock<ModelLockFile> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| {
        let override_json = std::env::var_os(LOCK_ENV)
            .filter(|v| !v.is_empty())
            .and_then(|p| std::fs::read_to_string(p).ok());
        let lock: ModelLockFile =
            serde_json::from_str(override_json.as_deref().unwrap_or(MODEL_LOCK_JSON))
                .expect("voice/MODEL.lock.json is valid JSON");
        for tier in &lock.tiers {
            assert!(
                lock.models.contains_key(tier),
                "voice/MODEL.lock.json: tier {tier} has no model pin"
            );
        }
        lock
    })
}

/// The release mirror every voice asset is fetched from: [`MIRROR_BASE_ENV`] when set, else the
/// GitHub release of the running version (from the lock's template, minus the file). `None` for a
/// source build, which has no release to fetch a helper from.
pub fn release_mirror_base() -> Option<String> {
    if let Some(base) = std::env::var_os(MIRROR_BASE_ENV).filter(|v| !v.is_empty()) {
        return Some(base.to_string_lossy().trim_end_matches('/').to_owned());
    }
    let version = RELEASE_VERSION?.trim().trim_start_matches('v');
    if version.is_empty() || RELEASE_REPO.is_empty() {
        return None;
    }
    let template = &lock().mirror_url_template;
    let base = template.strip_suffix("/{file}")?;
    Some(
        base.replace("{release_repo}", RELEASE_REPO)
            .replace("{version}", version),
    )
}

/// Pin for a tier id (`turbo` / `small` / `base`).
pub fn pin(tier: &str) -> Option<&'static ModelPin> {
    lock().models.get(tier)
}

impl ModelPin {
    /// Download sources in order: the project mirror for the running release (when this is a
    /// release build), then the pinned upstream file. Both must yield the pinned SHA-256.
    pub fn download_urls(&self) -> Vec<String> {
        let mut urls = Vec::with_capacity(2);
        if let Some(mirror) = self.mirror_url(RELEASE_REPO, RELEASE_VERSION) {
            urls.push(mirror);
        }
        urls.push(self.upstream_url.clone());
        urls
    }

    /// The one URL the background prefetch uses: this file on the release mirror, never the
    /// upstream host (the prefetch's egress is the mirror's host only).
    pub fn prefetch_url(&self) -> Option<String> {
        release_mirror_base().map(|base| format!("{base}/{}", self.file))
    }

    pub fn mirror_url(&self, release_repo: &str, version: Option<&str>) -> Option<String> {
        let version = version?.trim().trim_start_matches('v');
        if version.is_empty() || release_repo.is_empty() {
            return None;
        }
        Some(
            lock()
                .mirror_url_template
                .replace("{release_repo}", release_repo)
                .replace("{version}", version)
                .replace("{file}", &self.file),
        )
    }

    /// Human size for one-line messages ("547 MiB").
    pub fn human_size(&self) -> String {
        format!("{} MiB", self.size / (1024 * 1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_pins_three_multilingual_tiers() {
        let l = lock();
        assert_eq!(l.schema_version, 2);
        assert_eq!(l.tiers, vec!["turbo", "small", "base"]);
        let turbo = pin("turbo").unwrap();
        assert_eq!(turbo.file, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(turbo.size, 574_041_195);
        assert_eq!(
            turbo.sha256,
            "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"
        );
        assert_eq!(turbo.human_size(), "547 MiB");
        let small = pin("small").unwrap();
        assert_eq!(
            (small.file.as_str(), small.size),
            ("ggml-small.bin", 487_601_967)
        );
        assert_eq!(
            small.sha256,
            "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b"
        );
        let base = pin("base").unwrap();
        assert_eq!(
            (base.file.as_str(), base.size),
            ("ggml-base.bin", 147_951_465)
        );
        assert_eq!(
            base.sha256,
            "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"
        );
        for m in l.models.values() {
            assert!(
                m.upstream_url
                    .starts_with("https://huggingface.co/ggerganov/whisper.cpp/")
            );
            assert!(
                !m.file.ends_with(".en.bin"),
                "tiers must be multilingual: {}",
                m.file
            );
            assert_eq!(m.sha256.len(), 64);
        }
        assert_eq!(l.selection.interim_budget_ms, 1000);
        assert_eq!(l.license, "MIT");
        assert!(l.engine.platforms.contains(&"macos-aarch64".to_owned()));
        assert!(pin("tiny").is_none());
    }

    #[test]
    fn mirror_url_substitutes_repo_version_and_file() {
        let base = pin("base").unwrap();
        assert_eq!(
            base.mirror_url("owner/name", Some("v1.2.3")).as_deref(),
            Some("https://github.com/owner/name/releases/download/v1.2.3/ggml-base.bin")
        );
        assert!(base.mirror_url("owner/name", None).is_none());
        let urls = base.download_urls();
        assert_eq!(urls.last(), Some(&base.upstream_url));
        assert!(urls.iter().all(|u| u.starts_with("https://")));
    }

    #[test]
    fn prefetch_uses_the_mirror_base_only() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let base = pin("base").unwrap();
        crate::test_support::with_env(
            &[(MIRROR_BASE_ENV, Some("http://127.0.0.1:9/assets/"))],
            || {
                assert_eq!(
                    release_mirror_base().as_deref(),
                    Some("http://127.0.0.1:9/assets")
                );
                assert_eq!(
                    base.prefetch_url().as_deref(),
                    Some("http://127.0.0.1:9/assets/ggml-base.bin")
                );
            },
        );
        crate::test_support::with_env(&[(MIRROR_BASE_ENV, None)], || {
            // A release build derives the GitHub release; a source build has no mirror.
            match RELEASE_VERSION {
                Some(v) => assert_eq!(
                    release_mirror_base(),
                    Some(format!(
                        "https://github.com/{RELEASE_REPO}/releases/download/v{}",
                        v.trim_start_matches('v')
                    ))
                ),
                None => assert!(release_mirror_base().is_none()),
            }
        });
    }
}
