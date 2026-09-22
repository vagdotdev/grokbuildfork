//! Workshop overlay: bake the release repository into the updater.
//!
//! `WORKSHOP_RELEASE_REPO` (GitHub `OWNER/NAME`) is exported by the release workflow at build time
//! so a binary always updates from the repository that published it. The default is the private
//! fork; it changes when the public release repository exists (docs/workshop/adr/0004-telemetry-off.md).
fn main() {
    println!("cargo:rerun-if-env-changed=WORKSHOP_RELEASE_REPO");
    let repo = std::env::var("WORKSHOP_RELEASE_REPO")
        .ok()
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "vagdotdev/grokbuildfork".to_owned());
    let valid = repo.split('/').count() == 2
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'));
    assert!(
        valid,
        "WORKSHOP_RELEASE_REPO must be a GitHub OWNER/NAME, got {repo:?}"
    );
    println!("cargo:rustc-env=WORKSHOP_RELEASE_REPO_RESOLVED={repo}");
    println!(
        "cargo:rustc-env=WORKSHOP_CHANNEL_BASE_URL=https://raw.githubusercontent.com/{repo}/release-channel"
    );
}
