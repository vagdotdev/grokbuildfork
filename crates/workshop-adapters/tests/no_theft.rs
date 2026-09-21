//! gate:no-theft (source level). The adapter crate must never read another
//! app's credential store. It has no business reading file *contents* at all:
//! identity and login state come from spawning the vendor CLI.

use std::path::Path;

const FORBIDDEN_PATHS: &[&str] = &[
    ".codex/auth.json",
    "opencode/auth.json",
    ".cursor/sdk/auth.json",
    ".claude/.credentials.json",
    "Claude Code-credentials",
    "find-generic-password",
    "opencode-with-claude",
    "127.0.0.1:3456",
];

const FORBIDDEN_APIS: &[&str] = &[
    "fs::read(",
    "fs::read_to_string(",
    "File::open(",
    "OpenOptions::new(",
    "fs::read_to_end(",
    "tokio::fs::read",
    "rusqlite",
    "security_framework",
    "keyring",
];

fn strip_comments(line: &str) -> &str {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return "";
    }
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

#[test]
fn adapter_sources_never_touch_vendor_credential_files() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    assert!(
        files.len() >= 10,
        "expected the crate's sources, found {files:?}"
    );

    let mut violations = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, raw) in text.lines().enumerate() {
            let code = strip_comments(raw);
            for needle in FORBIDDEN_PATHS.iter().chain(FORBIDDEN_APIS) {
                if code.contains(needle) {
                    violations.push(format!(
                        "{}:{}: `{needle}` in `{}`",
                        file.display(),
                        n + 1,
                        code.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "credential-theft gate failed:\n{}",
        violations.join("\n")
    );
}

fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
