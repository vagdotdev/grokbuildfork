//! OpenAI Codex CLI (`codex`).
//!
//! Verified against `@openai/codex` 0.155.1 and 0.156.1 (`codex exec --help`,
//! `codex exec resume --help`, `codex login status --help`) and
//! `codex-rs/exec/src/exec_events.rs` + `codex-rs/cli/src/login.rs` at tags
//! `rust-v0.155.1` / `rust-v0.156.1`.
//!
//! * identity: `codex --version` -> `codex-cli 0.155.1`
//! * status:   `codex login status` -> stderr `Logged in using ChatGPT` (exit 0)
//!   or `Not logged in` (exit 1). Workshop never reads `~/.codex/auth.json`.
//! * login:    `codex login` in the user's terminal.
//! * run:      `codex exec --json -s <sandbox> --skip-git-repo-check [-m M] -`
//!   (prompt on stdin via the `-` sentinel). Headless exec never asks for
//!   approvals (`AskForApproval::Never`), so the sandbox flag is the policy:
//!   `read-only`, `workspace-write` (no network), or `danger-full-access` for
//!   always-approve (every command runs, unsandboxed, as `--yolo`).
//! * resume:   `codex exec resume <id> --json --skip-git-repo-check
//!   -c sandbox_mode="<sandbox>" [-m M] -` (resume has no `-s` flag).
//!
//! `exec` has no ask-user-question item (that lives in the app-server protocol only), so no
//! question event is raised here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::claude::truncate;
use super::{json, tool};
use crate::adapter::{
    Adapter, AdapterId, LoginState, NormalizeError, Normalizer, PermissionPolicy, ProbeOutput,
    PromptDelivery, RunRequest, Terminal, VersionPin,
};
use crate::event::{AdapterEvent, Usage};

pub struct CodexAdapter;

fn sandbox(policy: PermissionPolicy) -> &'static str {
    match policy {
        PermissionPolicy::ReadOnly => "read-only",
        PermissionPolicy::WorkspaceWrite => "workspace-write",
        PermissionPolicy::AlwaysApprove => "danger-full-access",
    }
}

/// The command as Codex's own TUI shows it: without the `bash -lc` wrapper `exec` reports
/// (`/bin/bash -lc 'cd /w && ls'` -> `cd /w && ls`).
fn display_command(command: &str) -> String {
    let trimmed = command.trim();
    for shell in ["/bin/bash", "/bin/zsh", "/bin/sh", "bash", "zsh", "sh"] {
        for flag in [" -lc ", " -c "] {
            if let Some(rest) = trimmed
                .strip_prefix(shell)
                .and_then(|r| r.strip_prefix(flag))
            {
                return unquote(rest.trim());
            }
        }
    }
    trimmed.to_string()
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 {
        if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
            return inner.replace("'\\''", "'");
        }
        if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            return inner.replace("\\\"", "\"").replace("\\\\", "\\");
        }
    }
    s.to_string()
}

impl Adapter for CodexAdapter {
    fn id(&self) -> AdapterId {
        AdapterId::Codex
    }

    fn binary_names(&self) -> &'static [&'static str] {
        &["codex"]
    }

    fn extra_install_dirs(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".npm-global/bin")]
    }

    fn identity_probes(&self) -> &'static [&'static [&'static str]] {
        &[&["--version"]]
    }

    fn identify(&self, outputs: &[ProbeOutput]) -> Option<String> {
        let line = outputs.first()?.stdout.lines().next()?.trim();
        let version = line.strip_prefix("codex-cli ")?.trim();
        (!version.is_empty() && version.chars().next()?.is_ascii_digit())
            .then(|| version.to_string())
    }

    fn version_pin(&self) -> VersionPin {
        VersionPin {
            min_supported: "0.155.1",
            max_tested: "0.156.1",
        }
    }

    fn status_args(&self) -> &'static [&'static str] {
        &["login", "status"]
    }

    fn interpret_status(&self, output: &ProbeOutput) -> LoginState {
        let text = format!("{}\n{}", output.stderr, output.stdout);
        let first = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if output.success() && first.starts_with("Logged in using") {
            // Only a fixed label leaves this function; the API-key variant of
            // this line includes a partially masked key.
            let method = if first.contains("ChatGPT") {
                "ChatGPT"
            } else if first.contains("API key") {
                "API key"
            } else if first.contains("workload identity") {
                "Workload identity"
            } else {
                "Access token"
            };
            LoginState::Ready {
                method: Some(method.to_string()),
            }
        } else if first.starts_with("Not logged in") {
            LoginState::SignIn
        } else {
            LoginState::Unknown {
                reason: format!(
                    "`codex login status` exit {:?}: {}",
                    output.exit_code,
                    truncate(first, 120)
                ),
            }
        }
    }

    fn login_args(&self) -> &'static [&'static str] {
        &["login"]
    }

    fn logout_args(&self) -> &'static [&'static str] {
        &["logout"]
    }

    fn prompt_delivery(&self) -> PromptDelivery {
        PromptDelivery::Stdin
    }

    fn run_args(&self, req: &RunRequest) -> Vec<String> {
        let mut args: Vec<String> = match &req.resume {
            None => vec![
                "exec".into(),
                "--json".into(),
                "-s".into(),
                sandbox(req.permission).into(),
                "--skip-git-repo-check".into(),
            ],
            Some(id) => vec![
                "exec".into(),
                "resume".into(),
                id.clone(),
                "--json".into(),
                "--skip-git-repo-check".into(),
                "-c".into(),
                format!("sandbox_mode=\"{}\"", sandbox(req.permission)),
            ],
        };
        if let Some(model) = &req.model {
            args.push("-m".into());
            args.push(model.clone());
        }
        args.push("-".into());
        args
    }

    fn normalizer(&self) -> Box<dyn Normalizer> {
        Box::new(CodexNormalizer::default())
    }
}

/// Translates `codex exec --json` thread events.
#[derive(Default)]
pub struct CodexNormalizer {
    session_id: Option<String>,
    terminal: Option<Terminal>,
    started_items: HashSet<String>,
    last_message: Option<String>,
}

impl CodexNormalizer {
    /// The pager's tool row for one thread item: `command_execution` is `bash` (the command
    /// without its `bash -lc` wrapper), `file_change` is `edit` — or `write` when every change
    /// adds a file — on the changed paths, `web_search` is `websearch`, an MCP call is its
    /// tool's own name with its arguments.
    fn tool_call_for(item: &Value, id: &str, kind: &str) -> Option<AdapterEvent> {
        let (name, input) = match kind {
            "command_execution" => (
                tool::BASH.to_string(),
                json!({ "command": display_command(json::str(item, "command").unwrap_or_default()) }),
            ),
            "mcp_tool_call" => (
                json::str(item, "tool")
                    .filter(|t| !t.is_empty())
                    .unwrap_or("mcp")
                    .to_string(),
                match item.get("arguments") {
                    Some(args) if args.is_object() => args.clone(),
                    Some(Value::Null) | None => json!({}),
                    Some(other) => json!({ "arguments": other }),
                },
            ),
            "web_search" => (
                tool::WEB_SEARCH.to_string(),
                json!({ "query": json::str(item, "query").unwrap_or_default() }),
            ),
            "file_change" => {
                let changes: Vec<&Value> = item
                    .get("changes")
                    .and_then(Value::as_array)
                    .map(|c| c.iter().collect())
                    .unwrap_or_default();
                let paths: Vec<&str> = changes
                    .iter()
                    .filter_map(|c| json::str(c, "path"))
                    .collect();
                let all_new = !changes.is_empty()
                    && changes.iter().all(|c| json::str(c, "kind") == Some("add"));
                (
                    if all_new { tool::WRITE } else { tool::EDIT }.to_string(),
                    json!({
                        "filePath": paths.join(", "),
                        "changes": item.get("changes").cloned().unwrap_or(Value::Null),
                    }),
                )
            }
            _ => return None,
        };
        Some(AdapterEvent::ToolCall {
            id: id.to_string(),
            name,
            input,
        })
    }
}

impl Normalizer for CodexNormalizer {
    fn on_line(&mut self, line: &str) -> Result<Vec<AdapterEvent>, NormalizeError> {
        let v: Value =
            serde_json::from_str(line).map_err(|_| NormalizeError::NotJson(truncate(line, 200)))?;
        let mut events = Vec::new();
        match json::str(&v, "type") {
            Some("thread.started") => {
                if let Some(id) = json::str(&v, "thread_id") {
                    self.session_id = Some(id.to_string());
                }
            }
            Some("turn.started") => {}
            Some("item.started") | Some("item.updated") => {
                let item = v.get("item").unwrap_or(&Value::Null);
                let id = json::str(item, "id").unwrap_or_default();
                let kind = json::str(item, "type").unwrap_or_default();
                if self.started_items.insert(id.to_string())
                    && let Some(call) = Self::tool_call_for(item, id, kind)
                {
                    events.push(call);
                }
            }
            Some("item.completed") => {
                let item = v.get("item").unwrap_or(&Value::Null);
                let id = json::str(item, "id").unwrap_or_default();
                let kind = json::str(item, "type").unwrap_or_default();
                match kind {
                    "agent_message" => {
                        let text = json::str(item, "text").unwrap_or_default().to_string();
                        self.last_message = Some(text.clone());
                        events.push(AdapterEvent::TextDelta { text });
                    }
                    "reasoning" => events.push(AdapterEvent::Thinking {
                        text: json::str(item, "text").unwrap_or_default().to_string(),
                    }),
                    "error" => events.push(AdapterEvent::Error {
                        message: json::str(item, "message").unwrap_or_default().to_string(),
                    }),
                    "command_execution" | "mcp_tool_call" | "web_search" | "file_change" => {
                        if self.started_items.insert(id.to_string())
                            && let Some(call) = Self::tool_call_for(item, id, kind)
                        {
                            events.push(call);
                        }
                        let status = json::str(item, "status").unwrap_or_default();
                        let (output, is_error) = match kind {
                            "command_execution" => {
                                // As the engine's `bash`: the exit code travels in the detail
                                // (a non-zero one reads `exit N` on the `Run` row); a command
                                // that never ran (declined, no exit code) is the row's error.
                                let output = json::str(item, "aggregated_output")
                                    .unwrap_or_default()
                                    .to_string();
                                let exit = item.get("exit_code").and_then(Value::as_i64);
                                let mut metadata = json!({ "output": output });
                                if let Some(code) = exit {
                                    metadata["exit"] = Value::from(code);
                                }
                                events.push(AdapterEvent::ToolDetail {
                                    id: id.to_string(),
                                    title: Some(display_command(
                                        json::str(item, "command").unwrap_or_default(),
                                    )),
                                    metadata,
                                });
                                (output, exit.is_none() && status != "completed")
                            }
                            "mcp_tool_call" => match item.get("error") {
                                Some(err) if !err.is_null() => (
                                    json::str(err, "message").unwrap_or_default().to_string(),
                                    true,
                                ),
                                _ => (
                                    item.get("result").map(Value::to_string).unwrap_or_default(),
                                    status == "failed",
                                ),
                            },
                            "file_change" => (
                                item.get("changes")
                                    .map(Value::to_string)
                                    .unwrap_or_default(),
                                status == "failed",
                            ),
                            _ => (String::new(), false),
                        };
                        events.push(AdapterEvent::ToolResult {
                            id: id.to_string(),
                            output,
                            is_error,
                        });
                    }
                    // todo_list, collab_tool_call and future item kinds
                    _ => {}
                }
            }
            Some("turn.completed") => {
                if let Some(usage) = v.get("usage") {
                    events.push(AdapterEvent::Usage(Usage {
                        input_tokens: json::u64_of(usage, "input_tokens"),
                        output_tokens: json::u64_of(usage, "output_tokens"),
                        cache_read_tokens: json::u64_of(usage, "cached_input_tokens"),
                        cache_write_tokens: json::u64_of(usage, "cache_write_input_tokens"),
                        reasoning_tokens: json::u64_of(usage, "reasoning_output_tokens"),
                        cost_usd: None,
                    }));
                }
                events.push(AdapterEvent::Done {
                    session_id: self.session_id.clone(),
                    result: self.last_message.clone(),
                });
                self.terminal = Some(Terminal::Completed);
            }
            Some("turn.failed") => {
                let message = v
                    .get("error")
                    .and_then(|e| json::str(e, "message"))
                    .unwrap_or("turn failed")
                    .to_string();
                events.push(AdapterEvent::Error {
                    message: message.clone(),
                });
                self.terminal = Some(Terminal::Failed(message));
            }
            Some("error") => events.push(AdapterEvent::Error {
                message: json::str(&v, "message").unwrap_or_default().to_string(),
            }),
            Some(_) => {}
            None => {
                return Err(NormalizeError::Shape(format!(
                    "line without `type`: {}",
                    truncate(line, 200)
                )));
            }
        }
        Ok(events)
    }

    fn on_eof(&mut self, exit_code: Option<i32>) -> Vec<AdapterEvent> {
        if self.terminal.is_some() {
            return Vec::new();
        }
        let message = format!(
            "codex exited ({}) before `turn.completed`",
            exit_code.map_or("signal".to_string(), |c| c.to_string())
        );
        self.terminal = Some(Terminal::Failed(message.clone()));
        vec![AdapterEvent::Error { message }]
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_codex_cli() {
        let a = CodexAdapter;
        let ok = ProbeOutput {
            stdout: "codex-cli 0.155.1\n".into(),
            ..Default::default()
        };
        assert_eq!(a.identify(&[ok]), Some("0.155.1".to_string()));
        let other = ProbeOutput {
            stdout: "0.155.1\n".into(),
            ..Default::default()
        };
        assert_eq!(a.identify(&[other]), None);
    }

    #[test]
    fn status_text_maps_without_leaking_key() {
        let a = CodexAdapter;
        let out = ProbeOutput {
            stderr: "Not logged in\n".into(),
            exit_code: Some(1),
            ..Default::default()
        };
        assert_eq!(a.interpret_status(&out), LoginState::SignIn);
        let out = ProbeOutput {
            stderr: "Logged in using ChatGPT\n".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        assert_eq!(
            a.interpret_status(&out),
            LoginState::Ready {
                method: Some("ChatGPT".into())
            }
        );
        let out = ProbeOutput {
            stderr: "Logged in using an API key - sk-proj-abc***xyz\n".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        let state = a.interpret_status(&out);
        assert_eq!(
            state,
            LoginState::Ready {
                method: Some("API key".into())
            }
        );
        assert!(!format!("{state:?}").contains("sk-proj"));
    }

    #[test]
    fn pinned_run_and_resume_flags() {
        let a = CodexAdapter;
        let req = RunRequest::new("hi", "/tmp");
        assert_eq!(
            a.run_args(&req),
            vec![
                "exec",
                "--json",
                "-s",
                "read-only",
                "--skip-git-repo-check",
                "-"
            ]
        );
        // Always-approve drops the sandbox (workspace-write would still deny network and
        // out-of-folder writes): every command runs, as with `--yolo`.
        let mut yolo = RunRequest::new("hi", "/tmp");
        yolo.permission = PermissionPolicy::AlwaysApprove;
        assert!(
            a.run_args(&yolo)
                .windows(2)
                .any(|w| w == ["-s", "danger-full-access"])
        );
        yolo.resume = Some("thread-1".into());
        assert!(
            a.run_args(&yolo)
                .windows(2)
                .any(|w| w == ["-c", "sandbox_mode=\"danger-full-access\""])
        );
        let mut req = RunRequest::new("hi", "/tmp");
        req.resume = Some("thread-1".into());
        req.permission = PermissionPolicy::WorkspaceWrite;
        req.model = Some("gpt-5-codex".into());
        assert_eq!(
            a.run_args(&req),
            vec![
                "exec",
                "resume",
                "thread-1",
                "--json",
                "--skip-git-repo-check",
                "-c",
                "sandbox_mode=\"workspace-write\"",
                "-m",
                "gpt-5-codex",
                "-"
            ]
        );
    }

    /// Thread items become the pager's rows: `command_execution` is a `Run` of the command
    /// without its `bash -lc` wrapper, carrying exit code and output; `file_change` an `Edit`
    /// (or `Creating`, when every change adds a file) on the changed paths; `web_search` a
    /// `Web search`; an MCP call its tool's own name.
    #[test]
    fn thread_items_use_the_pager_vocabulary() {
        assert_eq!(display_command("/bin/bash -lc ls"), "ls");
        assert_eq!(
            display_command("/bin/bash -lc 'cd /w && ls -la'"),
            "cd /w && ls -la"
        );
        assert_eq!(
            display_command("bash -lc \"echo \\\"hi\\\"\""),
            "echo \"hi\""
        );
        assert_eq!(display_command("python3 run.py"), "python3 run.py");

        let mut n = CodexNormalizer::default();
        let lines = [
            r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"/bin/bash -lc 'ls /nope'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"/bin/bash -lc 'ls /nope'","aggregated_output":"ls: cannot access '/nope'\n","exit_code":2,"status":"failed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"file_change","changes":[{"path":"README.md","kind":"update"}],"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i3","type":"file_change","changes":[{"path":"a.txt","kind":"add"},{"path":"b.txt","kind":"add"}],"status":"completed"}}"#,
            r#"{"type":"item.started","item":{"id":"i4","type":"web_search","query":"ghostty ubuntu","action":{"type":"search"}}}"#,
            r#"{"type":"item.completed","item":{"id":"i5","type":"mcp_tool_call","server":"fs","tool":"read_file","arguments":{"path":"/w/a"},"result":{"content":[{"type":"text","text":"hi"}]},"error":null,"status":"completed"}}"#,
        ];
        let events: Vec<AdapterEvent> = lines
            .iter()
            .flat_map(|l| n.on_line(l).expect("valid line"))
            .collect();
        assert_eq!(
            events,
            vec![
                AdapterEvent::ToolCall {
                    id: "i1".into(),
                    name: "bash".into(),
                    input: json!({"command": "ls /nope"}),
                },
                AdapterEvent::ToolDetail {
                    id: "i1".into(),
                    title: Some("ls /nope".into()),
                    metadata: json!({"output": "ls: cannot access '/nope'\n", "exit": 2}),
                },
                AdapterEvent::ToolResult {
                    id: "i1".into(),
                    output: "ls: cannot access '/nope'\n".into(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "i2".into(),
                    name: "edit".into(),
                    input: json!({"filePath": "README.md", "changes": [{"path": "README.md", "kind": "update"}]}),
                },
                AdapterEvent::ToolResult {
                    id: "i2".into(),
                    output: json!([{"path": "README.md", "kind": "update"}]).to_string(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "i3".into(),
                    name: "write".into(),
                    input: json!({"filePath": "a.txt, b.txt", "changes": [{"path": "a.txt", "kind": "add"}, {"path": "b.txt", "kind": "add"}]}),
                },
                AdapterEvent::ToolResult {
                    id: "i3".into(),
                    output:
                        json!([{"path": "a.txt", "kind": "add"}, {"path": "b.txt", "kind": "add"}])
                            .to_string(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "i4".into(),
                    name: "websearch".into(),
                    input: json!({"query": "ghostty ubuntu"}),
                },
                AdapterEvent::ToolCall {
                    id: "i5".into(),
                    name: "read_file".into(),
                    input: json!({"path": "/w/a"}),
                },
                AdapterEvent::ToolResult {
                    id: "i5".into(),
                    output: json!({"content": [{"type": "text", "text": "hi"}]}).to_string(),
                    is_error: false,
                },
            ]
        );
        // A declined command never ran: no exit code, and the row's error.
        let declined = n
            .on_line(r#"{"type":"item.completed","item":{"id":"i6","type":"command_execution","command":"/bin/bash -lc 'rm -rf /'","aggregated_output":"","exit_code":null,"status":"declined"}}"#)
            .unwrap();
        assert!(matches!(
            declined.last(),
            Some(AdapterEvent::ToolResult { is_error: true, .. })
        ));
    }
}
