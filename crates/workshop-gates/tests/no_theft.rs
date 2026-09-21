//! gate:no-theft
//!
//! Scans every text file under the workspace (`crates/`, `prod/`, `third_party/`, `bin/`,
//! `patches/`, `scripts/`, and the root manifests) and fails if any of them references a foreign
//! credential store or a Claude OAuth capture path. The plan forbids these outright; the picker
//! export that inspired the UX used every one of them, and none may be ported.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::{Regex, RegexSet};

struct Forbidden {
    /// Stable id shown in failures.
    id: &'static str,
    /// Why it is forbidden.
    why: &'static str,
    pattern: &'static str,
}

/// Each pattern is a case-insensitive regex.
const FORBIDDEN: &[Forbidden] = &[
    Forbidden {
        id: "codex-auth-json",
        why: "reads Codex's stored ChatGPT OAuth / API key",
        pattern: r"\.codex[/\\]auth\.json",
    },
    Forbidden {
        id: "opencode-auth-json",
        why: "reads OpenCode's provider credentials file",
        pattern: r"opencode[/\\]auth\.json",
    },
    Forbidden {
        id: "opencode-sqlite-db",
        why: "reads OpenCode's session database",
        pattern: r"opencode[/\\]opencode\.db",
    },
    Forbidden {
        id: "claude-code-keychain-item",
        why: "reads Claude Code's macOS keychain item",
        pattern: r"Claude Code-credentials",
    },
    Forbidden {
        id: "claude-code-credentials-file",
        why: "reads Claude Code's Linux/Windows credentials file",
        pattern: r"\.claude[/\\]\.credentials\.json",
    },
    Forbidden {
        id: "claude-keychain-lookup",
        why: "looks up a Claude keychain entry with the security tool",
        pattern: r"find-generic-password[^\n]*claude",
    },
    Forbidden {
        id: "cursor-sdk-auth-json",
        why: "reads Cursor's SDK auth file for an API key",
        pattern: r"\.cursor[/\\]sdk[/\\]auth\.json",
    },
    Forbidden {
        id: "cursor-state-db",
        why: "reads Cursor's desktop state database",
        pattern: r"state\.vscdb",
    },
    Forbidden {
        id: "provider-autodock",
        why: "the deleted vault that copied OpenCode/Codex tokens; must not be replayed",
        pattern: r"provider_autodock",
    },
    Forbidden {
        id: "opencode-with-claude",
        why: "bundles the Meridian Claude Pro/Max OAuth plugin",
        pattern: r"opencode-with-claude",
    },
    Forbidden {
        id: "meridian-loopback-proxy",
        why: "points Anthropic traffic at the Meridian OAuth proxy",
        pattern: r"127\.0\.0\.1:3456|localhost:3456",
    },
    Forbidden {
        id: "claude-oauth-endpoint",
        why: "drives a Claude/Anthropic consumer OAuth flow (prohibited by Anthropic; Workshop uses the official claude CLI or a Console API key)",
        pattern: r"(claude\.com|claude\.ai|anthropic\.com)[/:][^\s\x22']*oauth",
    },
    Forbidden {
        id: "claude-code-oauth-client-id",
        why: "reuses Claude Code's OAuth client id",
        pattern: r"9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    },
];

fn regexes() -> &'static Vec<(&'static Forbidden, Regex)> {
    static RE: OnceLock<Vec<(&'static Forbidden, Regex)>> = OnceLock::new();
    RE.get_or_init(|| {
        FORBIDDEN
            .iter()
            .map(|f| {
                (
                    f,
                    Regex::new(&format!("(?i){}", f.pattern)).unwrap_or_else(|e| panic!("{}: {e}", f.id)),
                )
            })
            .collect()
    })
}

/// One pass over a whole file decides whether the per-line scan is needed at all.
fn regex_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| {
        RegexSet::new(FORBIDDEN.iter().map(|f| format!("(?i){}", f.pattern))).expect("valid patterns")
    })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/workshop-gates sits two levels below the workspace root")
        .to_path_buf()
}

const SCAN_DIRS: &[&str] = &["crates", "prod", "third_party", "bin", "patches", "scripts"];
const SCAN_ROOT_FILES: &[&str] = &["Cargo.toml", "Cargo.lock", "package.json", "README.md"];
const SKIP_DIR_NAMES: &[&str] = &["target", ".git", "node_modules", "bazel-out"];

struct Hit {
    path: PathBuf,
    line: usize,
    id: &'static str,
    why: &'static str,
    excerpt: String,
}

fn allowlist(root: &Path) -> Vec<PathBuf> {
    let file = root.join("crates/workshop-gates/no-theft-allowlist.txt");
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| root.join(l))
        .collect()
}

fn scan_file(path: &Path, hits: &mut Vec<Hit>) {
    let Ok(bytes) = std::fs::read(path) else { return };
    if bytes.contains(&0) {
        return; // binary
    }
    let text = String::from_utf8_lossy(&bytes);
    if !regex_set().is_match(&text) {
        return;
    }
    for (lineno, line) in text.lines().enumerate() {
        for (f, re) in regexes() {
            if re.is_match(line) {
                hits.push(Hit {
                    path: path.to_path_buf(),
                    line: lineno + 1,
                    id: f.id,
                    why: f.why,
                    excerpt: line.trim().chars().take(160).collect(),
                });
            }
        }
    }
}

fn scan(root: &Path, exclude: &[PathBuf]) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in SCAN_DIRS {
        let dir = root.join(dir);
        if !dir.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&dir)
            .into_iter()
            .filter_entry(|e| {
                !(e.file_type().is_dir()
                    && e.file_name().to_str().is_some_and(|n| SKIP_DIR_NAMES.contains(&n)))
            })
            .filter_map(Result::ok)
        {
            if entry.file_type().is_file() {
                files.push(entry.into_path());
            }
        }
    }
    for f in SCAN_ROOT_FILES {
        let p = root.join(f);
        if p.is_file() {
            files.push(p);
        }
    }
    for path in files {
        if exclude.iter().any(|x| x == &path) {
            continue;
        }
        scan_file(&path, &mut hits);
    }
    hits
}

fn report(hits: &[Hit], root: &Path) -> String {
    let mut out = String::from("gate:no-theft FAILED — forbidden credential access or OAuth capture path referenced:\n");
    for h in hits {
        let rel = h.path.strip_prefix(root).unwrap_or(&h.path);
        out.push_str(&format!(
            "  {}:{}  [{}] {}\n      {}\n",
            rel.display(),
            h.line,
            h.id,
            h.why,
            h.excerpt
        ));
    }
    out.push_str("Remove the reference. Workshop spawns official CLIs and stores only its own secrets.\n");
    out
}

#[test]
fn workspace_never_touches_foreign_credentials_or_claude_oauth() {
    let root = workspace_root();
    let this_file = root.join("crates/workshop-gates/tests/no_theft.rs");
    let mut exclude = allowlist(&root);
    exclude.push(this_file);
    let hits = scan(&root, &exclude);
    assert!(hits.is_empty(), "{}", report(&hits, &root));
}

#[test]
fn deleted_autodock_module_is_absent() {
    let root = workspace_root();
    let autodock = root.join("crates/codegen/xai-grok-pager/src/provider_autodock.rs");
    assert!(!autodock.exists(), "{} must stay deleted", autodock.display());
}

#[test]
fn allowlist_is_reviewed_and_narrow() {
    let root = workspace_root();
    let entries = allowlist(&root);
    assert!(
        entries.len() <= 3,
        "no-theft allowlist grew to {} entries; each must name the thing being forbidden, not use it",
        entries.len()
    );
    for e in &entries {
        assert!(e.is_file(), "allowlisted path does not exist: {}", e.display());
    }
}

/// The scanner itself must catch every pattern, or the gate could pass vacuously.
#[test]
fn scanner_detects_each_forbidden_pattern() {
    let tmp = tempfile::tempdir().unwrap();
    let crates = tmp.path().join("crates/planted/src");
    std::fs::create_dir_all(&crates).unwrap();
    let samples: &[(&str, &str)] = &[
        ("codex-auth-json", r#"let p = home.join(".codex/auth.json");"#),
        ("opencode-auth-json", r#"read_json(join(data, "opencode", "auth.json")) // ~/.local/share/opencode/auth.json"#),
        ("opencode-sqlite-db", r#"open("~/.local/share/opencode/opencode.db")"#),
        ("claude-code-keychain-item", r#"security find-generic-password -s "Claude Code-credentials""#),
        ("claude-code-credentials-file", r#"fs::read("~/.claude/.credentials.json")"#),
        ("claude-keychain-lookup", r#"Command::new("security").args(["find-generic-password", "-s", "Claude Code"])"#),
        ("cursor-sdk-auth-json", r#"join(homedir(), ".cursor", "sdk", "auth.json") // ~/.cursor/sdk/auth.json"#),
        ("cursor-state-db", r#"sqlite::open("state.vscdb")"#),
        ("provider-autodock", "mod provider_autodock;"),
        ("opencode-with-claude", r#"plugin: ["opencode-with-claude"],"#),
        ("meridian-loopback-proxy", r#"baseURL: "http://127.0.0.1:3456","#),
        ("claude-oauth-endpoint", r#"const AUTH = "https://claude.com/oauth/authorize";"#),
        ("claude-code-oauth-client-id", r#"client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"#),
    ];
    for (i, (_, line)) in samples.iter().enumerate() {
        std::fs::write(crates.join(format!("f{i}.rs")), format!("// planted\n{line}\n")).unwrap();
    }
    let hits = scan(tmp.path(), &[]);
    for (id, line) in samples {
        assert!(
            hits.iter().any(|h| h.id == *id && h.excerpt.contains(line.trim())),
            "scanner missed [{id}] in {line:?}; hits: {:?}",
            hits.iter().map(|h| h.id).collect::<Vec<_>>()
        );
    }
    // Every declared pattern was exercised at least once.
    for f in FORBIDDEN {
        assert!(samples.iter().any(|(id, _)| id == &f.id), "no sample for {}", f.id);
    }

    // Legitimate mentions of the products themselves do not trip the gate.
    let clean = tmp.path().join("crates/planted/src/clean.rs");
    std::fs::write(
        &clean,
        "// Detect `claude`, `codex`, `cursor-agent`, and `opencode` on PATH.\n\
         // Bash(\"security find-generic-password -s x\") is a shell permission rule.\n\
         let url = \"https://api.anthropic.com/v1/messages\";\n\
         let docs = \"https://code.claude.com/docs/en/cli-reference\";\n",
    )
    .unwrap();
    let mut only_clean = Vec::new();
    scan_file(&clean, &mut only_clean);
    assert!(only_clean.is_empty(), "false positives: {:?}", only_clean.iter().map(|h| h.id).collect::<Vec<_>>());
}
