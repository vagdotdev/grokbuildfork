//! Workshop no-xAI / no-token-theft gates (docs/workshop-production-plan.md section 1).
//!
//! The library holds the pure predicates; `tests/` runs them as the required CI check
//! (`cargo test -p workshop-gates`). They are written so that a rename-only diff fails:
//! each gate inspects the compiled default, not a string in a README.

#![deny(clippy::indexing_slicing)]

/// Hosts that must never appear in a Workshop compile-time default or be contacted on the
/// default path. The optional xAI provider may list them only inside its own manifest.
pub const FORBIDDEN_DEFAULT_HOST_SUFFIXES: &[&str] = &[
    "x.ai",
    "grok.com",
    "api.mixpanel.com",
    "storage.googleapis.com",
];

/// `true` when `url`'s host is, or is a subdomain of, a forbidden default host.
pub fn url_hits_forbidden_host(url: &str) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    FORBIDDEN_DEFAULT_HOST_SUFFIXES
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

/// Cheap host extraction (scheme://host[:port]/...) without pulling in a URL crate.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    let host = if let Some(stripped) = hostport.strip_prefix('[') {
        stripped.split(']').next()?
    } else {
        hostport.split(':').next()?
    };
    Some(host.to_ascii_lowercase())
}

/// `true` when `url` is loopback (`127.0.0.1`, `::1`, `localhost`) — Workshop's allowed idle default.
pub fn url_is_loopback(url: &str) -> bool {
    matches!(
        host_of(url).as_deref(),
        Some("127.0.0.1") | Some("::1") | Some("localhost")
    )
}

/// Markers whose presence in a source tree means another app's credentials are being read
/// (gate:no-theft). Kept here so the source scan and the CI script share one list.
pub const THEFT_MARKERS: &[&str] = &[
    "Claude Code-credentials",
    ".codex/auth.json",
    ".cursor/sdk/auth.json",
    "share/opencode/auth.json",
    "opencode-with-claude",
    "127.0.0.1:3456",
    "provider_autodock",
];

/// Repository root (two levels above this crate's manifest), for source-scanning gates.
pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/workshop-gates sits two levels below the repo root")
        .to_path_buf()
}

/// Rust source with `//` line comments and `/* */` block comments removed (string
/// literals kept), so a comment that *names* a forbidden thing does not trip a source gate.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_block = 0usize;
    let mut in_str = false;
    let mut in_line = false;
    while let Some(c) = chars.next() {
        if in_line {
            if c == '\n' {
                in_line = false;
                out.push('\n');
            }
            continue;
        }
        if in_block > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                in_block += 1;
            } else if c == '\n' {
                out.push('\n');
            }
            continue;
        }
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                in_line = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block = 1;
            }
            _ => out.push(c),
        }
    }
    out
}

/// Source with the trailing `#[cfg(test)]` module removed. Test modules may use forbidden
/// paths as negative examples; they are not compiled into the binary.
pub fn strip_test_modules(src: &str) -> &str {
    match src.find("#[cfg(test)]") {
        Some(idx) => src.get(..idx).unwrap_or(src),
        None => src,
    }
}

/// What a default-path source gate scans: comments and test modules removed.
pub fn scannable_source(src: &str) -> String {
    strip_comments(strip_test_modules(src))
}

/// Every regular file under `root` (recursive), sorted; no external walker dependency.
pub fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments_keeps_strings_and_drops_comments() {
        let src = "let a = \"https://x.ai // not a comment\"; // https://auth.x.ai\n/* grok.com */ let b = 1;";
        let stripped = strip_comments(src);
        assert!(stripped.contains("https://x.ai // not a comment"));
        assert!(!stripped.contains("auth.x.ai"));
        assert!(!stripped.contains("grok.com"));
        assert!(stripped.contains("let b = 1;"));
    }

    #[test]
    fn strip_test_modules_cuts_at_cfg_test() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests { const X: &str = \"https://x.ai/cli\"; }";
        assert_eq!(strip_test_modules(src), "fn a() {}\n");
        assert!(!scannable_source(src).contains("x.ai"));
    }

    #[test]
    fn host_extraction() {
        assert_eq!(host_of("https://auth.x.ai/x").as_deref(), Some("auth.x.ai"));
        assert_eq!(
            host_of("wss://code.grok.com:443/ws").as_deref(),
            Some("code.grok.com")
        );
        assert_eq!(host_of("http://127.0.0.1:1/v1").as_deref(), Some("127.0.0.1"));
        assert_eq!(host_of("http://[::1]:8080/").as_deref(), Some("::1"));
        assert_eq!(host_of("not a url"), None);
    }

    #[test]
    fn forbidden_hosts() {
        assert!(url_hits_forbidden_host("https://auth.x.ai"));
        assert!(url_hits_forbidden_host("https://accounts.x.ai/sign-in"));
        assert!(url_hits_forbidden_host("https://cli-chat-proxy.grok.com/v1"));
        assert!(url_hits_forbidden_host("wss://grok.com/ws/gw/"));
        assert!(url_hits_forbidden_host("https://api.mixpanel.com/track"));
        assert!(url_hits_forbidden_host(
            "https://storage.googleapis.com/grok-build-public-artifacts/cli"
        ));
        assert!(!url_hits_forbidden_host("https://evil-x.ai.example/"));
        assert!(!url_hits_forbidden_host("http://127.0.0.1:1/v1"));
        assert!(!url_hits_forbidden_host("https://api.openai.com/v1"));
    }

    #[test]
    fn loopback() {
        assert!(url_is_loopback("http://127.0.0.1:1/v1"));
        assert!(url_is_loopback("ws://localhost:9/x"));
        assert!(!url_is_loopback("https://cli-chat-proxy.grok.com/v1"));
    }
}
