//! Workshop overlay: the engine's permission asks in the pager's own approval prompt.
//!
//! `opencode serve` runs with an `ask` policy for edits and commands (see
//! `workshop_adapters::opencode_engine::ask_before_edit_and_bash`). Each ask reaches the UI thread
//! as `WorkshopTurnMsg::PermissionAsk`; in Normal and Plan mode it is turned into the same
//! ACP-shaped request the shell sends for its own tools and queued on the agent, so the user sees
//! the familiar prompt (title, the command or the diff, Yes / Yes-always / No, followup text) and
//! the answer flows back to the server as `once` / `always` / `reject`.

use agent_client_protocol as acp;
use tokio::sync::oneshot;
use workshop_adapters::opencode_engine::{PermissionReply, PermissionRequest};

use crate::app::agent_view::AgentView;
use crate::app::workshop::WorkshopTurnMsg;

const ALLOW_ONCE: &str = "allow-once";
const ALLOW_ALWAYS: &str = "allow-always";
const REJECT_ONCE: &str = "reject-once";
/// Most diff lines shown inside the prompt; the row's expanded body has the whole diff later.
const MAX_DIFF_LINES: usize = 24;

/// Map the user's pick in the approval prompt to the engine's reply vocabulary. Anything that is
/// not an explicit allow — a reject, a cancel (Esc/Ctrl-C), a dropped prompt — is `reject`.
pub fn reply_for_response(
    response: Option<&acp::RequestPermissionResponse>,
) -> (PermissionReply, Option<String>) {
    let Some(response) = response else {
        return (PermissionReply::Reject, None);
    };
    let followup = response
        .meta
        .as_ref()
        .and_then(|m| m.get("followup_message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    match &response.outcome {
        acp::RequestPermissionOutcome::Selected(sel) => {
            let id = sel.option_id.0.as_ref();
            if id == ALLOW_ONCE
                || id == xai_grok_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID
            {
                (PermissionReply::Once, None)
            } else if id == ALLOW_ALWAYS {
                (PermissionReply::Always, None)
            } else {
                (PermissionReply::Reject, followup)
            }
        }
        _ => (PermissionReply::Reject, None),
    }
}

/// `/home/me/Desktop/*` → `~/Desktop/*`: paths under the user's home read shorter in a prompt.
fn shorten_home(path: &str) -> String {
    match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
        Some(h) if path == h => "~".to_owned(),
        Some(h) if path.starts_with(&format!("{h}/")) => format!("~{}", &path[h.len()..]),
        _ => path.to_owned(),
    }
}

/// The directories an `external_directory` ask names, shortened with `~` for the user's home.
pub fn external_directories(req: &PermissionRequest) -> Vec<String> {
    let mut dirs: Vec<String> = req
        .metadata
        .get("directories")
        .and_then(|d| d.as_array())
        .map(|d| {
            d.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if dirs.is_empty() {
        dirs = req
            .patterns
            .iter()
            .map(|p| p.trim_end_matches("/*").trim_end_matches('*').to_owned())
            .filter(|p| !p.is_empty())
            .collect();
    }
    dirs.iter().map(|d| shorten_home(d)).collect()
}

/// The ACP request the pager's permission view renders for an engine ask, in plain words: an
/// Execute call ("Run this command?", the command in full underneath) for `bash` and for an
/// out-of-folder command ("Run this command? It works outside this folder: ~/Desktop"), an Edit
/// call with the file for `edit`, a generic call otherwise. The question is the title; the
/// command goes in the body, where the view wraps it — a long command never cuts the question.
pub fn acp_request(req: &PermissionRequest, session_id: &str) -> acp::RequestPermissionRequest {
    let mut fields = acp::ToolCallUpdateFields::default();
    let mut options = Vec::new();
    let always_label = |what: &str| {
        if req.always.is_empty() || req.always.iter().any(|p| p == "*") {
            format!("Yes, and don't ask again for {what} this session")
        } else {
            let patterns: Vec<String> = req.always.iter().map(|p| shorten_home(p)).collect();
            format!(
                "Yes, and don't ask again for `{}` this session",
                patterns.join("`, `")
            )
        }
    };
    let execute = |fields: &mut acp::ToolCallUpdateFields, command: &str, question: &str| {
        fields.kind = Some(acp::ToolKind::Execute);
        fields.title = Some(format!("Execute `{command}`"));
        // `description` is what the view shows as the question; `command` is wrapped below it.
        fields.raw_input = Some(serde_json::json!({
            "command": command,
            "description": question,
        }));
    };
    match req.kind.as_str() {
        "bash" => {
            let command = req.command().unwrap_or(req.title.as_str()).to_owned();
            execute(&mut fields, &command, "Run this command?");
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes, run it",
                acp::PermissionOptionKind::AllowOnce,
            ));
            options.push(acp::PermissionOption::new(
                ALLOW_ALWAYS,
                always_label("commands like this"),
                acp::PermissionOptionKind::AllowAlways,
            ));
        }
        "external_directory" if req.command().is_some() => {
            let command = req.command().unwrap_or_default().to_owned();
            let dirs = external_directories(req);
            let where_ = if dirs.is_empty() {
                "outside this folder".to_owned()
            } else {
                format!("outside this folder: {}", dirs.join(", "))
            };
            execute(
                &mut fields,
                &command,
                &format!("Run this command? It works {where_}"),
            );
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes, run it",
                acp::PermissionOptionKind::AllowOnce,
            ));
            options.push(acp::PermissionOption::new(
                ALLOW_ALWAYS,
                always_label("that folder"),
                acp::PermissionOptionKind::AllowAlways,
            ));
        }
        "edit" | "external_directory" => {
            let path = req.file_path().unwrap_or(req.title.as_str()).to_owned();
            fields.kind = Some(acp::ToolKind::Edit);
            fields.title = Some(format!("Edit {path}"));
            fields.raw_input = Some(serde_json::json!({ "file_path": path }));
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes",
                acp::PermissionOptionKind::AllowOnce,
            ));
            options.push(acp::PermissionOption::new(
                ALLOW_ALWAYS,
                if req.kind == "edit" {
                    "Yes, allow all edits during this session".to_owned()
                } else {
                    always_label("that folder")
                },
                acp::PermissionOptionKind::AllowAlways,
            ));
        }
        other => {
            fields.title = Some(if req.title.is_empty() {
                format!("Allow {other}?")
            } else {
                format!("Allow {other}? {}", req.title)
            });
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes",
                acp::PermissionOptionKind::AllowOnce,
            ));
            if !req.always.is_empty() {
                options.push(acp::PermissionOption::new(
                    ALLOW_ALWAYS,
                    always_label("this"),
                    acp::PermissionOptionKind::AllowAlways,
                ));
            }
        }
    }
    options.push(acp::PermissionOption::new(
        REJECT_ONCE,
        "No, and tell Workshop what to do differently",
        acp::PermissionOptionKind::RejectOnce,
    ));
    let tool_call = acp::ToolCallUpdate::new(
        acp::ToolCallId::new(req.call_id.clone().unwrap_or_else(|| req.id.clone())),
        fields,
    );
    acp::RequestPermissionRequest::new(
        acp::SessionId::new(session_id.to_owned()),
        tool_call,
        options,
    )
}

/// Whether a shell command only reads: every simple command in the pipeline/list is a known
/// read-only program with read-only arguments, nothing is redirected to a file, and nothing can
/// smuggle another command in (`$(…)`, backticks, `sh -c`, `xargs`, `sudo`). Such commands run
/// without a prompt in Normal mode, the way Claude Code and OpenCode treat them; anything not
/// recognised prompts. Conservative by construction: unknown programs and flags prompt.
pub fn is_read_only_command(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() || command.contains("$(") || command.contains('`') {
        return false;
    }
    let segments = split_simple_commands(command);
    !segments.is_empty() && segments.iter().all(|s| simple_command_is_read_only(s))
}

/// Split a shell command line on `&&`, `||`, `;`, `|`, `&` (background) and newlines, outside
/// quotes. `>&`, `2>&1` and `&>` are redirections, not separators.
fn split_simple_commands(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut single, mut double, mut escape) = (false, false, false);
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if escape {
            cur.push(c);
            escape = false;
        } else if c == '\\' && !single {
            cur.push(c);
            escape = true;
        } else if c == '\'' && !double {
            single = !single;
            cur.push(c);
        } else if c == '"' && !single {
            double = !double;
            cur.push(c);
        } else if single || double {
            cur.push(c);
        } else if c == '\n' || c == ';' || c == '|' {
            if c == '|' && chars.get(i + 1) == Some(&'|') {
                i += 1;
            }
            out.push(std::mem::take(&mut cur));
        } else if c == '&' {
            let prev = i
                .checked_sub(1)
                .and_then(|p| chars.get(p))
                .copied()
                .unwrap_or(' ');
            let next = chars.get(i + 1).copied().unwrap_or(' ');
            if prev == '>' || prev == '<' || next == '>' || next.is_ascii_digit() {
                cur.push(c); // `>&1`, `2>&1`, `&>file`
            } else {
                if next == '&' {
                    i += 1;
                }
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whitespace tokenizer that keeps quoted strings together (quotes removed).
fn tokens(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let (mut single, mut double, mut escape) = (false, false, false);
    for c in segment.chars() {
        if escape {
            cur.push(c);
            escape = false;
            in_token = true;
        } else if c == '\\' && !single {
            escape = true;
            in_token = true;
        } else if c == '\'' && !double {
            single = !single;
            in_token = true;
        } else if c == '"' && !single {
            double = !double;
            in_token = true;
        } else if c.is_whitespace() && !single && !double {
            if in_token {
                out.push(std::mem::take(&mut cur));
                in_token = false;
            }
        } else {
            cur.push(c);
            in_token = true;
        }
    }
    if in_token {
        out.push(cur);
    }
    out
}

/// Programs whose every invocation only reads (file system, process table, hardware).
const READ_ONLY_PROGRAMS: &[&str] = &[
    "ls",
    "dir",
    "cat",
    "head",
    "tail",
    "file",
    "pwd",
    "which",
    "whereis",
    "whoami",
    "id",
    "uname",
    "hostname",
    "arch",
    "nproc",
    "uptime",
    "free",
    "df",
    "du",
    "stat",
    "wc",
    "echo",
    "printf",
    "date",
    "cal",
    "true",
    "false",
    "test",
    "[",
    "[[",
    "type",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "tr",
    "sort",
    "uniq",
    "cut",
    "column",
    "nl",
    "tac",
    "rev",
    "fold",
    "fmt",
    "expand",
    "comm",
    "diff",
    "cmp",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "b2sum",
    "cksum",
    "strings",
    "hexdump",
    "xxd",
    "od",
    "tree",
    "locate",
    "lsblk",
    "lscpu",
    "lsusb",
    "lspci",
    "ps",
    "pgrep",
    "cd",
    "pushd",
    "popd",
    "dirs",
    "jq",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "fd",
    "fdfind",
    "lsb_release",
    "getconf",
    "ldd",
    "nm",
    "objdump",
    "readelf",
    "sw_vers",
    "sysctl",
    "getent",
    "stat",
    "mimetype",
    "xdg-mime",
    "tput",
    "seq",
    "yes",
    "expr",
    "bc",
];

/// Version queries any program answers without doing anything else.
const VERSION_FLAGS: &[&str] = &["--version", "-version", "-V", "version", "--help", "-h"];

fn simple_command_is_read_only(segment: &str) -> bool {
    let mut toks = tokens(segment);
    // Subshell / group punctuation.
    toks.retain(|t| !matches!(t.as_str(), "(" | ")" | "{" | "}" | "!"));
    for t in &mut toks {
        while t.starts_with('(') {
            t.remove(0);
        }
        while t.ends_with(')') && t.len() > 1 {
            t.pop();
        }
    }
    toks.retain(|t| !t.is_empty());
    // Leading VAR=value assignments do not run anything.
    while toks.first().is_some_and(|t| is_assignment(t)) {
        toks.remove(0);
    }
    // Redirections: only to /dev/null or another descriptor. A `<` reads.
    let mut args: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(t) = toks.get(i) {
        let (op, target) = redirection(t);
        if let Some(op) = op {
            let target = match target {
                Some(t) => t.to_owned(),
                None => {
                    i += 1;
                    match toks.get(i) {
                        Some(next) => next.clone(),
                        None => return false,
                    }
                }
            };
            if op == '>' && !(target == "/dev/null" || target.starts_with('&')) {
                return false;
            }
        } else if t.contains('>') {
            // Anything else that looks like it writes somewhere (`foo>bar`, a quoted `>`).
            return false;
        } else {
            args.push(t.clone());
        }
        i += 1;
    }
    let Some(program) = args.first() else {
        return false;
    };
    let program = program.rsplit('/').next().unwrap_or(program).to_owned();
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    if let [only] = rest.as_slice()
        && VERSION_FLAGS.contains(only)
        && !program.is_empty()
    {
        return true;
    }
    match program.as_str() {
        p if READ_ONLY_PROGRAMS.contains(&p) => true,
        "find" => !rest.iter().any(|a| {
            matches!(
                *a,
                "-delete"
                    | "-exec"
                    | "-execdir"
                    | "-ok"
                    | "-okdir"
                    | "-fprint"
                    | "-fprint0"
                    | "-fprintf"
                    | "-fls"
            )
        }),
        "sed" => !rest
            .iter()
            .any(|a| *a == "-i" || a.starts_with("-i") || a.starts_with("--in-place")),
        "git" => git_is_read_only(&rest),
        "dpkg" => rest
            .first()
            .is_some_and(|a| matches!(*a, "-l" | "-s" | "-L" | "-S" | "--list" | "--status")),
        "rpm" => rest.first().is_some_and(|a| a.starts_with("-q")),
        "brew" => rest
            .first()
            .is_some_and(|a| matches!(*a, "list" | "info" | "--prefix")),
        "pip" | "pip3" => rest
            .first()
            .is_some_and(|a| matches!(*a, "list" | "show" | "freeze")),
        "npm" => rest
            .first()
            .is_some_and(|a| matches!(*a, "ls" | "list" | "view" | "outdated")),
        "cargo" => rest
            .first()
            .is_some_and(|a| matches!(*a, "metadata" | "tree" | "--list")),
        "docker" => rest
            .first()
            .is_some_and(|a| matches!(*a, "ps" | "images" | "version")),
        "systemctl" => rest
            .first()
            .is_some_and(|a| matches!(*a, "status" | "is-active" | "list-units")),
        _ => false,
    }
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// `(op, inline target)` for a redirection token: `2>/dev/null` → (`>`, "/dev/null"),
/// `2>` → (`>`, None), `<file` → (`<`, "file"). Not a redirection → (None, None).
fn redirection(token: &str) -> (Option<char>, Option<&str>) {
    let stripped = token.trim_start_matches(|c: char| c.is_ascii_digit());
    if let Some(rest) = stripped.strip_prefix(">>") {
        return (Some('>'), (!rest.is_empty()).then_some(rest));
    }
    if let Some(rest) = stripped.strip_prefix('>') {
        return (Some('>'), (!rest.is_empty()).then_some(rest));
    }
    if let Some(rest) = stripped.strip_prefix("&>") {
        return (Some('>'), (!rest.is_empty()).then_some(rest));
    }
    if let Some(rest) = stripped.strip_prefix('<') {
        return (Some('<'), (!rest.is_empty()).then_some(rest));
    }
    (None, None)
}

fn git_is_read_only(rest: &[&str]) -> bool {
    let mut args = rest.iter().copied().peekable();
    // Global options before the subcommand.
    while let Some(a) = args.peek().copied() {
        if a == "--no-pager" || a.starts_with("--git-dir") || a.starts_with("--work-tree") {
            args.next();
        } else if a == "-C" || a == "-c" {
            args.next();
            args.next();
        } else {
            break;
        }
    }
    let Some(sub) = args.next() else {
        return false;
    };
    let tail: Vec<&str> = args.collect();
    match sub {
        "status" | "log" | "diff" | "show" | "blame" | "rev-parse" | "describe" | "ls-files"
        | "ls-tree" | "cat-file" | "shortlog" | "rev-list" | "name-rev" | "count-objects"
        | "grep" | "help" | "version" | "--version" => true,
        "branch" => tail.iter().all(|a| {
            matches!(
                *a,
                "-a" | "-r"
                    | "-v"
                    | "-vv"
                    | "--list"
                    | "--all"
                    | "--remotes"
                    | "--verbose"
                    | "--show-current"
            )
        }),
        "remote" => tail
            .first()
            .is_none_or(|a| matches!(*a, "-v" | "show" | "get-url")),
        "stash" => tail.first().is_some_and(|a| matches!(*a, "list" | "show")),
        "tag" => tail.is_empty() || tail.iter().all(|a| matches!(*a, "-l" | "--list" | "-n")),
        "config" => tail
            .first()
            .is_some_and(|a| matches!(*a, "--get" | "--list" | "-l" | "--get-all")),
        "worktree" => tail.first() == Some(&"list"),
        "reflog" => tail.first().is_none_or(|a| *a == "show"),
        _ => false,
    }
}

/// The changed lines of an `edit` ask's diff, for the prompt body (capped).
pub fn diff_preview_lines(req: &PermissionRequest) -> Vec<String> {
    let Some(diff) = req.diff() else {
        return Vec::new();
    };
    let mut lines: Vec<String> = diff
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .filter(|l| !l.starts_with('\\'))
        .map(str::to_owned)
        .collect();
    if lines.len() > MAX_DIFF_LINES {
        let hidden = lines.len() - MAX_DIFF_LINES;
        lines.truncate(MAX_DIFF_LINES);
        lines.push(format!("… (+{hidden} more lines)"));
    }
    lines
}

/// Show the engine's ask in the agent's approval prompt and wire the pick back to `reply`. A
/// "No" with typed text also queues that text as the next turn (`WorkshopTurnMsg::FollowUp`).
pub fn enqueue_engine_permission(
    agent: &mut AgentView,
    req: PermissionRequest,
    reply: oneshot::Sender<PermissionReply>,
    ui_tx: Option<tokio::sync::mpsc::UnboundedSender<WorkshopTurnMsg>>,
) {
    let session_id = agent
        .session
        .session_id
        .as_ref()
        .map(|s| s.0.to_string())
        .unwrap_or_else(|| req.session_id.clone());
    let request = acp_request(&req, &session_id);
    let (response_tx, response_rx) = oneshot::channel();
    let args = xai_acp_lib::AcpArgs {
        request,
        response_tx,
    };
    crate::app::acp_handler::enqueue_permission_for_workshop(args, agent);
    let preview = diff_preview_lines(&req);
    if !preview.is_empty()
        && let Some(front) = agent.permission_queue.back_mut()
    {
        front.description = preview;
    }
    let call_id = req.call_id.clone();
    tokio::spawn(async move {
        let response = response_rx.await.ok().and_then(Result::ok);
        let (decision, followup) = reply_for_response(response.as_ref());
        // The UI learns the outcome before the engine does, so the same tool call's next ask
        // (an out-of-folder command asks `external_directory`, then `bash`) is answered without a
        // second prompt.
        if let (Some(call_id), Some(tx)) = (call_id, ui_tx.as_ref()) {
            let _ = tx.send(WorkshopTurnMsg::PermissionDecided { call_id, decision });
        }
        let _ = reply.send(decision);
        if let (Some(text), Some(tx)) = (followup, ui_tx) {
            let _ = tx.send(WorkshopTurnMsg::FollowUp(text));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask(kind: &str, metadata: serde_json::Value, always: &[&str]) -> PermissionRequest {
        PermissionRequest {
            id: "per_1".into(),
            session_id: "ses_1".into(),
            kind: kind.into(),
            title: "rm -rf tmp".into(),
            patterns: vec!["rm -rf tmp".into()],
            call_id: Some("call_1".into()),
            metadata,
            always: always.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn bash_ask_renders_as_an_execute_prompt_with_the_command() {
        let req = ask("bash", json!({"command": "rm -rf tmp"}), &["rm *"]);
        let acp_req = acp_request(&req, "sid");
        assert_eq!(acp_req.tool_call.fields.kind, Some(acp::ToolKind::Execute));
        assert_eq!(
            acp_req.tool_call.fields.title.as_deref(),
            Some("Execute `rm -rf tmp`")
        );
        // `description` is the question the view shows; the command is wrapped underneath.
        assert_eq!(
            acp_req.tool_call.fields.raw_input,
            Some(json!({"command": "rm -rf tmp", "description": "Run this command?"}))
        );
        let names: Vec<&str> = acp_req.options.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Yes, run it",
                "Yes, and don't ask again for `rm *` this session",
                "No, and tell Workshop what to do differently",
            ]
        );
        let kinds: Vec<acp::PermissionOptionKind> =
            acp_req.options.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            vec![
                acp::PermissionOptionKind::AllowOnce,
                acp::PermissionOptionKind::AllowAlways,
                acp::PermissionOptionKind::RejectOnce,
            ]
        );
    }

    #[test]
    fn edit_ask_shows_the_file_and_previews_the_diff() {
        let req = ask(
            "edit",
            json!({"filepath": "/w/hello.txt", "diff": "Index: x\n--- a\n+++ b\n@@ -0,0 +1,1 @@\n+hi\n\\ No newline at end of file\n"}),
            &["*"],
        );
        let acp_req = acp_request(&req, "sid");
        assert_eq!(acp_req.tool_call.fields.kind, Some(acp::ToolKind::Edit));
        assert_eq!(
            acp_req.tool_call.fields.title.as_deref(),
            Some("Edit /w/hello.txt")
        );
        assert_eq!(
            acp_req.tool_call.fields.raw_input,
            Some(json!({"file_path": "/w/hello.txt"}))
        );
        assert_eq!(
            diff_preview_lines(&req),
            vec!["@@ -0,0 +1,1 @@".to_owned(), "+hi".to_owned()]
        );
        assert!(
            acp_req
                .options
                .iter()
                .any(|o| o.name == "Yes, allow all edits during this session")
        );
    }

    /// The 1.18.31 `external_directory` ask for a bash command (captured live): one question in
    /// plain words naming the folder, the command in the body, never in the title.
    #[test]
    fn out_of_folder_command_asks_in_plain_words_with_the_folder_named() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/u".into());
        let long = format!(
            "mkdir -p {home}/Desktop/iBooks && curl -fsSL -o {home}/Desktop/iBooks/alice.epub https://www.gutenberg.org/ebooks/11.epub.noimages && curl -fsSL -o {home}/Desktop/iBooks/frankenstein.epub https://www.gutenberg.org/ebooks/84.epub.noimages"
        );
        let mut req = ask(
            "external_directory",
            json!({"command": long, "directories": [format!("{home}/Desktop")], "patterns": [format!("{home}/Desktop/*")]}),
            &[&format!("{home}/Desktop/*")],
        );
        req.patterns = vec![format!("{home}/Desktop/*")];
        assert_eq!(external_directories(&req), vec!["~/Desktop".to_owned()]);
        let acp_req = acp_request(&req, "sid");
        assert_eq!(acp_req.tool_call.fields.kind, Some(acp::ToolKind::Execute));
        let raw = acp_req.tool_call.fields.raw_input.clone().unwrap();
        assert_eq!(
            raw["description"],
            json!("Run this command? It works outside this folder: ~/Desktop")
        );
        assert_eq!(
            raw["command"],
            json!(long),
            "the whole command, never truncated"
        );
        let names: Vec<&str> = acp_req.options.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Yes, run it",
                "Yes, and don't ask again for `~/Desktop/*` this session",
                "No, and tell Workshop what to do differently",
            ]
        );
    }

    #[test]
    fn read_only_commands_are_recognised_and_writes_are_not() {
        for cmd in [
            "ls ~/",
            "ls -la ~/Desktop 2>/dev/null || echo \"no desktop dir\"",
            "ls ~/ && echo \"---\" && ls ~/Desktop 2>/dev/null || echo \"no desktop dir\"",
            "which apt apt-get dnf pacman 2>/dev/null; echo \"---\"; cat /etc/os-release | head -3",
            "uname -a",
            "cat /etc/os-release",
            "head -20 src/main.rs",
            "file ~/Desktop/iBooks/*.epub",
            "pwd",
            "find . -name '*.rs' | wc -l",
            "find . -type f -name \"*.log\"",
            "grep -rn TODO src/",
            "rg -n 'fn main' .",
            "git status",
            "git log --oneline -5",
            "git diff HEAD~1 -- src/",
            "git branch -a",
            "git remote -v",
            "git -C /tmp/x status",
            "sed -n 1,5p Cargo.toml",
            "python3 --version",
            "cargo --version && rustc --version",
            "cd ~/Desktop && ls",
            "echo hi > /dev/null",
            "ps aux 2>&1 | head",
            "FOO=1 ls",
            "[[ -d tmp ]] && echo yes",
            "dpkg -l | grep ghostty",
            "(cd /tmp && ls)",
            "stat hello.txt; wc -l hello.txt",
        ] {
            assert!(is_read_only_command(cmd), "read-only: {cmd}");
        }
        for cmd in [
            "rm -rf tmp",
            "mkdir -p ~/Desktop/iBooks",
            "mkdir -p ~/Desktop/iBooks && curl -fsSL -o ~/Desktop/iBooks/a.epub https://x/a.epub",
            "curl -fsSL https://ghostty.org/install.sh | sudo sh",
            "sudo apt install ghostty",
            "apt-get install -y foo",
            "pip install requests",
            "brew install ghostty",
            "echo hi > hello.txt",
            "echo hi >> notes.txt",
            "cat a > b",
            "ls > listing.txt",
            "find . -name '*.log' -delete",
            "find . -name '*.log' -exec rm {} \\;",
            "sed -i 's/a/b/' file.txt",
            "git push origin main",
            "git checkout -b x",
            "git branch -D x",
            "git commit -am x",
            "git stash",
            "python3 -c 'print(1)'",
            "python3 script.py",
            "bash -c 'ls'",
            "sh install.sh",
            "xargs rm < list",
            "ls $(cat dirs)",
            "ls `cat dirs`",
            "tee out.txt",
            "chmod +x run.sh",
            "cp a b",
            "mv a b",
            "touch x",
            "npm install",
            "cargo build",
            "make",
            "",
            "   ",
        ] {
            assert!(!is_read_only_command(cmd), "not read-only: {cmd}");
        }
    }

    #[test]
    fn picks_map_to_engine_replies() {
        let selected = |id: &str| {
            acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Selected(
                acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(id)),
            ))
        };
        assert_eq!(
            reply_for_response(Some(&selected(ALLOW_ONCE))).0,
            PermissionReply::Once
        );
        assert_eq!(
            reply_for_response(Some(&selected(
                xai_grok_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID
            )))
            .0,
            PermissionReply::Once
        );
        assert_eq!(
            reply_for_response(Some(&selected(ALLOW_ALWAYS))).0,
            PermissionReply::Always
        );
        let mut rejected = selected(REJECT_ONCE);
        let mut meta = acp::Meta::new();
        meta.insert("followup_message".into(), json!(" use trash instead "));
        rejected.meta = Some(meta);
        assert_eq!(
            reply_for_response(Some(&rejected)),
            (
                PermissionReply::Reject,
                Some("use trash instead".to_owned())
            )
        );
        let cancelled =
            acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
        assert_eq!(
            reply_for_response(Some(&cancelled)).0,
            PermissionReply::Reject
        );
        assert_eq!(reply_for_response(None).0, PermissionReply::Reject);
    }
}
