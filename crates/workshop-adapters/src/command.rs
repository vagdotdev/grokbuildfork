//! Pinned, documented command lines per adapter.
//!
//! Verified against the vendor documentation and the installed CLIs on 2026-09-21:
//!
//! | Vendor   | Run                                                              | Resume                          | Docs |
//! |----------|------------------------------------------------------------------|---------------------------------|------|
//! | Claude   | `claude -p --output-format stream-json --verbose … PROMPT`       | `--resume <session_id>`         | code.claude.com/docs/en/cli-reference |
//! | Codex    | `codex exec --json --color never --sandbox <policy> -C <dir> … PROMPT` | `codex exec … resume <id> PROMPT` | developers.openai.com/codex/cli/reference |
//! | Cursor   | `agent -p --output-format stream-json --workspace <dir> --trust … PROMPT` | `--resume=<chatId>`         | cursor.com/docs/cli/reference/parameters |
//! | OpenCode | `opencode run --format json --dir <dir> … PROMPT`                | `--session <id>`                | opencode.ai/docs/cli |
//!
//! Permission profiles map to each CLI's own controls: Claude `--permission-mode` +
//! `--allowedTools`/`--disallowedTools` (+ `--permission-prompts none` from 2.1.259), Codex
//! `--sandbox read-only|workspace-write`, Cursor `--force`/`--sandbox enabled`, OpenCode the
//! documented `OPENCODE_PERMISSION` inline JSON. Versions outside [`SupportMatrix`] fail closed.

use std::ffi::OsString;
use std::path::Path;

use workshop_detect::{Identity, Vendor};

use crate::error::FailureReason;
use crate::request::{PermissionProfile, RunRequest};

/// Oldest CLI version whose documented flags this crate was verified against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportMatrix {
    pub claude_min: (u64, u64, u64),
    pub codex_min: (u64, u64, u64),
    /// Cursor versions are dates: `YYYY.MM.DD-<hash>`.
    pub cursor_min: (u64, u64, u64),
    pub opencode_min: (u64, u64, u64),
}

impl Default for SupportMatrix {
    fn default() -> Self {
        Self {
            claude_min: (2, 1, 0),
            codex_min: (0, 50, 0),
            cursor_min: (2026, 1, 1),
            opencode_min: (1, 0, 0),
        }
    }
}

/// Claude Code added `--permission-prompts none` in this version.
const CLAUDE_PERMISSION_PROMPTS_MIN: (u64, u64, u64) = (2, 1, 259);

/// Parse the leading `a.b.c` of a version string (`2.1.278`, `0.155.1`, `2026.09.18-9a7762b`).
pub fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let a = parts.next()??;
    let b = parts.next().flatten().unwrap_or(0);
    let c = parts.next().flatten().unwrap_or(0);
    Some((a, b, c))
}

impl SupportMatrix {
    fn min_for(&self, vendor: Vendor) -> (u64, u64, u64) {
        match vendor {
            Vendor::Claude => self.claude_min,
            Vendor::Codex => self.codex_min,
            Vendor::Cursor => self.cursor_min,
            Vendor::OpenCode => self.opencode_min,
        }
    }

    /// Fail closed on versions older than the pin or versions that do not parse.
    pub fn check(&self, identity: &Identity) -> Result<(), FailureReason> {
        let min = self.min_for(identity.vendor);
        let supported = format!(">= {}.{}.{}", min.0, min.1, min.2);
        match version_triple(&identity.version) {
            Some(v) if v >= min => Ok(()),
            _ => Err(FailureReason::UnsupportedVersion {
                vendor: identity.vendor,
                version: identity.version.clone(),
                supported,
            }),
        }
    }
}

/// A prompt that starts with `-` would be read as a flag; a leading space is harmless to the model.
fn positional_prompt(prompt: &str) -> String {
    if prompt.starts_with('-') {
        format!(" {prompt}")
    } else {
        prompt.to_string()
    }
}

fn claude_args(req: &RunRequest, identity: &Identity) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    match req.permissions {
        PermissionProfile::ReadOnly => {
            args.extend(["--permission-mode".into(), "default".into()]);
            args.extend(["--allowedTools".into(), "Read,Glob,Grep,LS,WebFetch".into()]);
            args.extend([
                "--disallowedTools".into(),
                "Edit,Write,MultiEdit,NotebookEdit,Bash".into(),
            ]);
        }
        PermissionProfile::WorkspaceWrite => {
            args.extend(["--permission-mode".into(), "acceptEdits".into()]);
            args.extend([
                "--allowedTools".into(),
                "Read,Glob,Grep,LS,Edit,Write,MultiEdit,NotebookEdit".into(),
            ]);
            args.extend(["--disallowedTools".into(), "Bash".into()]);
        }
    }
    if version_triple(&identity.version).is_some_and(|v| v >= CLAUDE_PERMISSION_PROMPTS_MIN) {
        args.extend(["--permission-prompts".into(), "none".into()]);
    }
    if let Some(n) = req.max_turns {
        args.extend(["--max-turns".into(), n.to_string()]);
    }
    if let Some(model) = &req.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(id) = &req.resume {
        args.extend(["--resume".into(), id.clone()]);
    }
    args.push(positional_prompt(&req.prompt));
    args
}

fn codex_args(req: &RunRequest, _identity: &Identity) -> Vec<String> {
    let mut args: Vec<String> = vec!["exec".into(), "--json".into(), "--color".into(), "never".into()];
    let sandbox = match req.permissions {
        PermissionProfile::ReadOnly => "read-only",
        PermissionProfile::WorkspaceWrite => "workspace-write",
    };
    args.extend(["--sandbox".into(), sandbox.into()]);
    if !req.workdir.path().join(".git").exists() {
        args.push("--skip-git-repo-check".into());
    }
    if let Some(model) = &req.model {
        args.extend(["--model".into(), model.clone()]);
    }
    args.extend(["-C".into(), req.workdir.path().to_string_lossy().into_owned()]);
    if let Some(id) = &req.resume {
        args.extend(["resume".into(), id.clone()]);
    }
    args.push(positional_prompt(&req.prompt));
    args
}

fn cursor_args(req: &RunRequest, _identity: &Identity) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--workspace".into(),
        req.workdir.path().to_string_lossy().into_owned(),
        "--trust".into(),
    ];
    if req.permissions == PermissionProfile::WorkspaceWrite {
        args.extend(["--force".into(), "--sandbox".into(), "enabled".into()]);
    }
    if let Some(model) = &req.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(id) = &req.resume {
        // `--resume [chatId]` takes an optional value; `=` keeps the prompt from being eaten.
        args.push(format!("--resume={id}"));
    }
    args.push(positional_prompt(&req.prompt));
    args
}

fn opencode_args(req: &RunRequest, _identity: &Identity) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--dir".into(),
        req.workdir.path().to_string_lossy().into_owned(),
    ];
    if let Some(model) = &req.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(id) = &req.resume {
        args.extend(["--session".into(), id.clone()]);
    }
    args.push(positional_prompt(&req.prompt));
    args
}

/// Build the argument vector (without the binary) for `req` against a verified binary.
pub fn build_argv(req: &RunRequest, identity: &Identity) -> Vec<String> {
    debug_assert_eq!(req.vendor, identity.vendor);
    match req.vendor {
        Vendor::Claude => claude_args(req, identity),
        Vendor::Codex => codex_args(req, identity),
        Vendor::Cursor => cursor_args(req, identity),
        Vendor::OpenCode => opencode_args(req, identity),
    }
}

/// OpenCode permission config (documented `OPENCODE_PERMISSION` inline JSON) per profile.
pub fn opencode_permission_json(profile: PermissionProfile) -> &'static str {
    match profile {
        PermissionProfile::ReadOnly => {
            r#"{"*":"deny","read":"allow","glob":"allow","grep":"allow","lsp":"allow","edit":"deny","bash":"deny","webfetch":"deny","websearch":"deny","external_directory":"deny","task":"deny"}"#
        }
        PermissionProfile::WorkspaceWrite => {
            r#"{"*":"deny","read":"allow","glob":"allow","grep":"allow","lsp":"allow","edit":"allow","bash":"deny","webfetch":"deny","websearch":"deny","external_directory":"deny","task":"deny"}"#
        }
    }
}

/// Vendor-specific, non-secret environment additions for a run.
pub fn vendor_env(vendor: Vendor, profile: PermissionProfile) -> Vec<(OsString, OsString)> {
    match vendor {
        Vendor::OpenCode => vec![
            (
                OsString::from("OPENCODE_PERMISSION"),
                OsString::from(opencode_permission_json(profile)),
            ),
            (
                OsString::from("OPENCODE_DISABLE_AUTOUPDATE"),
                OsString::from("1"),
            ),
        ],
        Vendor::Claude | Vendor::Codex | Vendor::Cursor => Vec::new(),
    }
}

/// Convenience for logging: the command as one line, prompt elided.
pub fn describe(bin: &Path, args: &[String]) -> String {
    let mut shown: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some(last) = shown.last_mut() {
        *last = "<prompt>";
    }
    format!("{} {}", bin.display(), shown.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Workdir;
    use std::path::PathBuf;

    fn identity(vendor: Vendor, version: &str) -> Identity {
        Identity {
            vendor,
            path: PathBuf::from(format!("/fake/{}", vendor.id())),
            version: version.into(),
        }
    }

    fn req(vendor: Vendor) -> RunRequest {
        RunRequest::new(vendor, "do the thing", Workdir::in_place_acknowledged("/work/dir"))
    }

    #[test]
    fn version_parsing_covers_all_vendor_formats() {
        assert_eq!(version_triple("2.1.278"), Some((2, 1, 278)));
        assert_eq!(version_triple("0.155.1"), Some((0, 155, 1)));
        assert_eq!(version_triple("2026.09.18-9a7762b"), Some((2026, 9, 18)));
        assert_eq!(version_triple("1.18.31"), Some((1, 18, 31)));
        assert_eq!(version_triple("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(version_triple("garbage"), None);
    }

    #[test]
    fn support_matrix_fails_closed() {
        let m = SupportMatrix::default();
        assert!(m.check(&identity(Vendor::Claude, "2.1.278")).is_ok());
        assert!(m.check(&identity(Vendor::Codex, "0.155.1")).is_ok());
        assert!(m.check(&identity(Vendor::Cursor, "2026.09.18-9a7762b")).is_ok());
        assert!(m.check(&identity(Vendor::OpenCode, "1.18.31")).is_ok());
        assert!(matches!(
            m.check(&identity(Vendor::Claude, "1.0.99")),
            Err(FailureReason::UnsupportedVersion { .. })
        ));
        assert!(matches!(
            m.check(&identity(Vendor::Cursor, "2025.12.31-abcdef0")),
            Err(FailureReason::UnsupportedVersion { .. })
        ));
        assert!(matches!(
            m.check(&identity(Vendor::Codex, "unknown")),
            Err(FailureReason::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn claude_argv_is_the_documented_print_stream_json_shape() {
        let args = build_argv(&req(Vendor::Claude), &identity(Vendor::Claude, "2.1.278"));
        assert_eq!(&args[..4], &["-p", "--output-format", "stream-json", "--verbose"]);
        assert!(args.windows(2).any(|w| w == ["--permission-mode", "default"]));
        assert!(args.windows(2).any(|w| w == ["--permission-prompts", "none"]));
        assert!(args.iter().any(|a| a == "--disallowedTools"));
        assert_eq!(args.last().unwrap(), "do the thing");
        assert!(!args.iter().any(|a| a == "--dangerously-skip-permissions"));

        // Older Claude: no --permission-prompts (added in 2.1.259).
        let old = build_argv(&req(Vendor::Claude), &identity(Vendor::Claude, "2.1.100"));
        assert!(!old.iter().any(|a| a == "--permission-prompts"));

        let mut r = req(Vendor::Claude).with_resume("sess-1").with_permissions(PermissionProfile::WorkspaceWrite);
        r.max_turns = Some(3);
        let args = build_argv(&r, &identity(Vendor::Claude, "2.1.278"));
        assert!(args.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(args.windows(2).any(|w| w == ["--max-turns", "3"]));
        assert!(args.windows(2).any(|w| w == ["--resume", "sess-1"]));
    }

    #[test]
    fn codex_argv_is_exec_json_with_sandbox() {
        let args = build_argv(&req(Vendor::Codex), &identity(Vendor::Codex, "0.155.1"));
        assert_eq!(&args[..4], &["exec", "--json", "--color", "never"]);
        assert!(args.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(args.windows(2).any(|w| w == ["-C", "/work/dir"]));
        assert!(args.iter().any(|a| a == "--skip-git-repo-check"), "no .git under /work/dir");
        assert_eq!(args.last().unwrap(), "do the thing");
        assert!(!args.iter().any(|a| a.contains("dangerously") || a == "--full-auto"));

        let r = req(Vendor::Codex).with_resume("thread-1").with_permissions(PermissionProfile::WorkspaceWrite);
        let args = build_argv(&r, &identity(Vendor::Codex, "0.155.1"));
        assert!(args.windows(2).any(|w| w == ["--sandbox", "workspace-write"]));
        let resume_at = args.iter().position(|a| a == "resume").unwrap();
        assert_eq!(args[resume_at + 1], "thread-1");
        assert_eq!(args[resume_at + 2], "do the thing");
        assert!(args.iter().position(|a| a == "--json").unwrap() < resume_at, "exec flags precede resume");
    }

    #[test]
    fn cursor_argv_is_print_stream_json_with_workspace() {
        let args = build_argv(&req(Vendor::Cursor), &identity(Vendor::Cursor, "2026.09.18-9a7762b"));
        assert_eq!(&args[..3], &["-p", "--output-format", "stream-json"]);
        assert!(args.windows(2).any(|w| w == ["--workspace", "/work/dir"]));
        assert!(args.iter().any(|a| a == "--trust"));
        assert!(!args.iter().any(|a| a == "--force" || a == "--yolo"), "read-only never forces edits");
        assert_eq!(args.last().unwrap(), "do the thing");

        let r = req(Vendor::Cursor).with_resume("chat-9").with_permissions(PermissionProfile::WorkspaceWrite);
        let args = build_argv(&r, &identity(Vendor::Cursor, "2026.09.18-9a7762b"));
        assert!(args.iter().any(|a| a == "--force"));
        assert!(args.windows(2).any(|w| w == ["--sandbox", "enabled"]));
        assert!(args.iter().any(|a| a == "--resume=chat-9"));
    }

    #[test]
    fn opencode_argv_is_run_json_with_dir_and_permission_env() {
        let args = build_argv(&req(Vendor::OpenCode), &identity(Vendor::OpenCode, "1.18.31"));
        assert_eq!(&args[..3], &["run", "--format", "json"]);
        assert!(args.windows(2).any(|w| w == ["--dir", "/work/dir"]));
        assert_eq!(args.last().unwrap(), "do the thing");
        assert!(!args.iter().any(|a| a == "--auto"));
        let r = req(Vendor::OpenCode).with_resume("ses_1");
        let args = build_argv(&r, &identity(Vendor::OpenCode, "1.18.31"));
        assert!(args.windows(2).any(|w| w == ["--session", "ses_1"]));

        let env = vendor_env(Vendor::OpenCode, PermissionProfile::ReadOnly);
        let perm = env.iter().find(|(k, _)| k == "OPENCODE_PERMISSION").unwrap();
        let json: serde_json::Value = serde_json::from_str(perm.1.to_str().unwrap()).unwrap();
        assert_eq!(json["edit"], "deny");
        assert_eq!(json["bash"], "deny");
        let env = vendor_env(Vendor::OpenCode, PermissionProfile::WorkspaceWrite);
        let perm = env.iter().find(|(k, _)| k == "OPENCODE_PERMISSION").unwrap();
        let json: serde_json::Value = serde_json::from_str(perm.1.to_str().unwrap()).unwrap();
        assert_eq!(json["edit"], "allow");
        assert_eq!(json["bash"], "deny");
    }

    #[test]
    fn prompt_starting_with_dash_is_not_a_flag() {
        let mut r = req(Vendor::Claude);
        r.prompt = "--help me".into();
        let args = build_argv(&r, &identity(Vendor::Claude, "2.1.278"));
        assert_eq!(args.last().unwrap(), " --help me");
    }

    #[test]
    fn describe_elides_prompt() {
        let args = build_argv(&req(Vendor::Codex), &identity(Vendor::Codex, "0.155.1"));
        let d = describe(Path::new("/usr/bin/codex"), &args);
        assert!(d.ends_with("<prompt>"));
        assert!(!d.contains("do the thing"));
    }
}
