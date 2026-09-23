//! gate:no-theft (source level) for detection and the model-list probes. Login state and model
//! lists come from spawning the vendor CLI; this crate never names another app's credential
//! store or keychain, and the only file it reads or writes is Workshop's own model cache
//! (`src/models/cache.rs`). The runtime half is `scripts/no-theft-fs-audit.sh`, which runs these
//! probes under strace against decoy credential files.

use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    ".claude/.credentials",
    "Claude Code-credentials",
    "find-generic-password",
    "security_framework",
    "keyring",
    ".codex/auth.json",
    "auth.json",
    ".cursor/sdk",
    "opencode/auth.json",
    "rusqlite",
];

const FILE_IO: &[&str] = &[
    "fs::read(",
    "fs::read_to_string(",
    "fs::write(",
    "File::open(",
    "File::create(",
    "OpenOptions::new(",
    "tokio::fs",
];

/// The one module allowed to touch files: `$WORKSHOP_HOME/catalog-cache/<rail>-models.json`.
const CACHE_MODULE: &str = "src/models/cache.rs";

fn code(line: &str) -> &str {
    if line.trim_start().starts_with("//") {
        return "";
    }
    line.find("//").map_or(line, |i| &line[..i])
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn detection_and_model_probes_never_touch_foreign_credentials() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    assert!(
        files.iter().any(|f| f.ends_with("models/claude.rs"))
            && files.iter().any(|f| f.ends_with("models/codex.rs"))
            && files.iter().any(|f| f.ends_with("models/cursor.rs")),
        "the model probes are scanned: {files:?}"
    );
    let mut violations = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(file).unwrap();
        // Unit tests may name a path as input; they never link into the binary.
        let production = text.split("#[cfg(test)]").next().unwrap_or("");
        for (n, raw) in production.lines().enumerate() {
            let line = code(raw);
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    violations.push(format!("{rel}:{}: `{needle}` in `{}`", n + 1, line.trim()));
                }
            }
            if rel != CACHE_MODULE {
                for needle in FILE_IO {
                    if line.contains(needle) {
                        violations.push(format!(
                            "{rel}:{}: file access outside {CACHE_MODULE}: `{}`",
                            n + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "gate:no-theft (workshop-detect):\n{}",
        violations.join("\n")
    );
}
