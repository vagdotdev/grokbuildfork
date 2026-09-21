//! Gate 5 — no token theft (`gate:no-theft`).
//!
//! No code path may open another app's credentials: `~/.codex/auth.json`,
//! OpenCode `auth.json`, the Claude Code keychain item, `~/.cursor/sdk/auth.json`,
//! nor bundle the Blackpen Meridian/OAuth-capture mechanism. `provider_autodock.rs`
//! (the July dock that copied OpenCode and Codex tokens into a vault) must not exist.
//!
//! Source scan over `crates/` with comments stripped, plus a filesystem audit
//! (`scripts/no-theft-fs-audit.sh`) on the built binary for the runtime side.

use std::path::Path;

use workshop_gates::{repo_root, scannable_source};

/// Literals that only appear in code that reads or replays foreign credentials.
const FORBIDDEN_LITERALS: &[&str] = &[
    ".codex/auth.json",
    "Claude Code-credentials",
    ".cursor/sdk/auth.json",
    "opencode-with-claude",
    "127.0.0.1:3456",
    "share/opencode/auth.json",
    "opencode\", \"auth.json\"",
    "provider_autodock",
];

/// Files allowed to name the forbidden things: the gates themselves and the
/// Workshop policy crate, whose doc comments are stripped anyway.
fn allowlisted(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/crates/workshop-gates/")
}

fn rust_sources(root: &Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "rs"))
        .filter(|p| !allowlisted(p))
        .collect()
}

#[test]
fn no_source_opens_foreign_credentials() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for path in rust_sources(&root) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let code = scannable_source(&src);
        for needle in FORBIDDEN_LITERALS {
            if code.contains(needle) {
                offenders.push(format!(
                    "{} contains {needle:?}",
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
    let hits: Vec<_> = walkdir::WalkDir::new(root.join("crates"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("provider_autodock")
        })
        .map(|e| e.into_path())
        .collect();
    assert!(hits.is_empty(), "provider_autodock replayed: {hits:?}");
}

#[test]
fn the_scan_detects_the_july_dock_pattern() {
    // Self-check: the forbidden list catches the actual code this gate exists
    // to keep out, so a green run is not a vacuous one.
    let july_dock_excerpt = r#"
        let codex = home.join(".codex/auth.json");
        let opencode = data_dir.join("opencode").join("auth.json");
        let opencode2 = home.join(".local/share/opencode/auth.json");
    "#;
    let code = scannable_source(july_dock_excerpt);
    assert!(
        FORBIDDEN_LITERALS.iter().any(|needle| code.contains(needle)),
        "forbidden list no longer matches the dock's credential paths"
    );
}
