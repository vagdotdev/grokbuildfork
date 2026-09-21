//! gate:no-theft as a cargo test (ADR 0003).
//!
//! No code path may open another app's credentials: `~/.codex/auth.json`, OpenCode `auth.json`,
//! the Claude Code keychain item, `~/.cursor/sdk/auth.json`, nor bundle the Blackpen
//! Meridian / OAuth-capture mechanism. `provider_autodock.rs` (the July dock that copied OpenCode
//! and Codex tokens into a vault) must not exist. The markers live in
//! [`workshop_gates::THEFT_MARKERS`] so this test and `scripts/no-xai-scan.sh` share one list.
//!
//! Source scan over `crates/` with comments and `#[cfg(test)]` modules stripped; the runtime half is
//! `scripts/no-theft-fs-audit.sh` (decoy credential files under a throwaway HOME, `strace -f`).

use std::path::Path;

use workshop_gates::{THEFT_MARKERS, files_under, repo_root, scannable_source};

/// Files allowed to name the forbidden things: the gates themselves (comments are stripped anyway),
/// and test code. `#[cfg(test)]` modules are stripped by [`scannable_source`]; integration-test
/// targets (`crates/*/tests/**`, `*test*.rs`) are separate crates that never link into the binary,
/// and they legitimately plant decoy credential files / name a marker as *input* to prove it is
/// never read or is dropped (e.g. workshop-adapters `tests/common/mod.rs`, `fake_cli_e2e.rs`).
/// The runtime half, `scripts/no-theft-fs-audit.sh`, covers what the binary actually opens.
fn allowlisted(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/crates/workshop-gates/")
        || s.contains("/tests/")
        || path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains("test"))
}

fn scannable_files(root: &Path) -> Vec<std::path::PathBuf> {
    files_under(&root.join("crates"))
        .into_iter()
        .filter(|p| {
            p.extension()
                .is_some_and(|ext| ext == "rs" || ext == "toml" || ext == "json")
        })
        .filter(|p| !allowlisted(p))
        .collect()
}

#[test]
fn no_source_opens_foreign_credentials() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for path in scannable_files(&root) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let code = if path.extension().is_some_and(|e| e == "rs") {
            scannable_source(&src)
        } else {
            src
        };
        for marker in THEFT_MARKERS {
            if code.contains(marker) {
                offenders.push(format!(
                    "{} contains {marker:?}",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "foreign-credential access in the tree:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn provider_autodock_is_not_in_the_tree() {
    let root = repo_root();
    let hits: Vec<_> = files_under(&root.join("crates"))
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("provider_autodock"))
        })
        .collect();
    assert!(hits.is_empty(), "provider_autodock replayed: {hits:?}");
}

#[test]
fn the_scan_detects_the_july_dock_pattern() {
    // Self-check: the marker list catches the code this gate exists to keep out, so a green run
    // is not vacuous.
    let july_dock_excerpt = r#"
        let codex = home.join(".codex/auth.json");
        let opencode = home.join(".local/share/opencode/auth.json");
        let cursor = home.join(".cursor/sdk/auth.json");
    "#;
    let code = scannable_source(july_dock_excerpt);
    assert!(
        THEFT_MARKERS.iter().any(|marker| code.contains(marker)),
        "marker list no longer matches the dock's credential paths"
    );
    // ...and a comment naming the forbidden path is allowed.
    let comment_only = "// never read ~/.codex/auth.json\nfn ok() {}\n";
    assert!(
        !THEFT_MARKERS
            .iter()
            .any(|marker| scannable_source(comment_only).contains(marker))
    );
}
