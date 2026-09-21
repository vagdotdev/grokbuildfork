//! Gate 4 — updater (`gate:no-xai`).
//!
//! `CLI_BASE_URL_PRIMARY`, `CLI_BASE_URL_FALLBACK`, `NPM_PACKAGE`, and
//! `GH_RELEASE_REPO` must not resolve to `https://x.ai/cli`, the Grok GCS
//! bucket, `@xai-official/grok`, or `xai-org-shared/grok-build`, and
//! auto-update must be off until Workshop owns a signed channel.
//!
//! `GH_RELEASE_REPO` is public and asserted directly. The base URLs and npm
//! package are `pub(crate)` upstream, so their compiled defaults are checked
//! on the built binary by `scripts/no-xai-scan.sh`; here the default-path
//! source is checked with comments stripped, so a comment naming the
//! forbidden thing is allowed but a live constant is not.

use workshop_gates::{repo_root, strip_comments};

const FORBIDDEN_UPDATE_TARGETS: &[&str] = &[
    "https://x.ai/cli",
    "grok-build-public-artifacts",
    "@xai-official/grok",
    "xai-org-shared/grok-build",
];

#[test]
fn gh_release_repo_is_not_xai() {
    assert_ne!(
        xai_grok_update::version::GH_RELEASE_REPO,
        "xai-org-shared/grok-build",
        "updater still targets the xAI GitHub release repo"
    );
    assert!(!xai_grok_update::version::GH_RELEASE_REPO.contains("xai-org"));
}

#[test]
fn updater_source_defaults_are_not_xai() {
    let path = repo_root().join("crates/codegen/xai-grok-update/src/version.rs");
    let src = std::fs::read_to_string(&path).expect("read version.rs");
    let code = strip_comments(&src);
    let offenders: Vec<&str> = FORBIDDEN_UPDATE_TARGETS
        .iter()
        .copied()
        .filter(|needle| code.contains(needle))
        .collect();
    assert!(
        offenders.is_empty(),
        "{} still compiles xAI update targets into the binary: {offenders:?}",
        path.display()
    );
}

#[test]
fn auto_update_is_compiled_off() {
    let path = repo_root().join("crates/codegen/xai-grok-update/src/auto_update.rs");
    let src = std::fs::read_to_string(&path).expect("read auto_update.rs");
    let code = strip_comments(&src);
    assert!(
        code.contains("WORKSHOP_AUTO_UPDATE_ENABLED: bool = false"),
        "auto_update.rs has no compile-time off switch; Workshop has no update channel yet"
    );
}
