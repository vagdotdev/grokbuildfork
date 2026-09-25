//! Claude Code (`claude`).
//!
//! Verified against `@anthropic-ai/claude-code` 2.1.278 and 2.1.281 (`claude --help`,
//! `claude auth status --help`) and the `SDKMessage` types shipped in
//! `@anthropic-ai/claude-agent-sdk` 0.3.278 (`sdk.d.ts`).
//!
//! * identity: `claude --version` -> `2.1.278 (Claude Code)`
//! * status:   `claude auth status --json` -> `{"loggedIn": bool, "authMethod": ...}`
//!   (exit 1 when logged out). Workshop never reads `~/.claude`.
//! * login:    `claude auth login` in the user's terminal.
//! * run:      `claude -p --output-format stream-json --verbose --include-partial-messages
//!   --input-format stream-json --permission-prompt-tool stdio --permission-mode <mode>
//!   [--model M] [--resume ID]`; the prompt is a `user` message line on stdin, which stays
//!   open as the control channel ([`PromptDelivery::Channel`]).
//!
//! The control channel is the Agent SDK's own wiring (`@anthropic-ai/claude-agent-sdk` 0.3.281
//! passes exactly these flags when it has a `canUseTool` callback; `workshop-detect` already
//! lists models over it): whatever the permission mode would prompt for arrives on stdout as
//! `{"type":"control_request","request_id":…,"request":{"subtype":"can_use_tool","tool_name":…,
//! "input":…,"tool_use_id":…,"permission_suggestions":[…]}}` and the CLI waits for
//! `{"type":"control_response","response":{"subtype":"success","request_id":…,"response":
//! {"behavior":"allow"|"deny",…}}}` on stdin. So `plan` asks nothing and edits nothing,
//! `acceptEdits` (Normal) applies edits and asks before commands — the host shows its approval
//! card — and `bypassPermissions` (always-approve) asks nothing. `AskUserQuestion` also arrives
//! as `can_use_tool`; it is answered with `updatedInput.answers`, the SDK's way. A build that
//! denies it instead (the `tool_result` is an error) gets the answers as the next prompt of the
//! resumed session.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{json, tool};
use crate::adapter::{
    Adapter, AdapterId, AskReply, LoginState, NormalizeError, Normalizer, PermissionPolicy,
    ProbeOutput, PromptDelivery, RunRequest, Terminal, VersionPin,
};
use crate::event::{AdapterEvent, QuestionChoice, QuestionPrompt, Usage};

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn id(&self) -> AdapterId {
        AdapterId::Claude
    }

    fn binary_names(&self) -> &'static [&'static str] {
        &["claude"]
    }

    fn extra_install_dirs(&self, home: &Path) -> Vec<PathBuf> {
        // Older native installer location; the current one is ~/.local/bin.
        vec![home.join(".claude/local")]
    }

    fn identity_probes(&self) -> &'static [&'static [&'static str]] {
        &[&["--version"]]
    }

    fn identify(&self, outputs: &[ProbeOutput]) -> Option<String> {
        let out = outputs.first()?;
        let line = out.stdout.lines().next()?.trim();
        let version = line.strip_suffix("(Claude Code)")?.trim();
        (!version.is_empty() && version.chars().next()?.is_ascii_digit())
            .then(|| version.to_string())
    }

    fn version_pin(&self) -> VersionPin {
        VersionPin {
            min_supported: "2.1.278",
            max_tested: "2.1.281",
        }
    }

    fn status_args(&self) -> &'static [&'static str] {
        &["auth", "status", "--json"]
    }

    fn interpret_status(&self, output: &ProbeOutput) -> LoginState {
        let Ok(v) = serde_json::from_str::<Value>(output.stdout.trim()) else {
            return LoginState::Unknown {
                reason: format!(
                    "`claude auth status --json` did not print JSON (exit {:?})",
                    output.exit_code
                ),
            };
        };
        match json::bool_of(&v, "loggedIn") {
            Some(true) => LoginState::Ready {
                method: Some(match json::str(&v, "authMethod") {
                    Some("claude.ai") => "Claude account".to_string(),
                    Some("console") | Some("apiKey") => "API key".to_string(),
                    _ => "Signed in".to_string(),
                }),
            },
            Some(false) => LoginState::SignIn,
            None => LoginState::Unknown {
                reason: "`loggedIn` missing from auth status".to_string(),
            },
        }
    }

    fn login_args(&self) -> &'static [&'static str] {
        &["auth", "login"]
    }

    fn logout_args(&self) -> &'static [&'static str] {
        &["auth", "logout"]
    }

    fn prompt_delivery(&self) -> PromptDelivery {
        PromptDelivery::Channel
    }

    /// The SDK's opening: an `initialize` control request (answered with the account's models,
    /// ignored here) and the prompt as a `user` message.
    fn prompt_lines(&self, prompt: &str) -> Vec<String> {
        vec![
            json!({
                "type": "control_request",
                "request_id": "workshop-init",
                "request": { "subtype": "initialize" },
            })
            .to_string(),
            json!({
                "type": "user",
                "session_id": "",
                "message": { "role": "user", "content": [{ "type": "text", "text": prompt }] },
                "parent_tool_use_id": null,
            })
            .to_string(),
        ]
    }

    fn run_args(&self, req: &RunRequest) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            match req.permission {
                PermissionPolicy::ReadOnly => "plan",
                PermissionPolicy::WorkspaceWrite => "acceptEdits",
                PermissionPolicy::AlwaysApprove => "bypassPermissions",
            },
        ]
        .into_iter()
        .map(String::from)
        .collect();
        if let Some(model) = &req.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(id) = &req.resume {
            args.push("--resume".into());
            args.push(id.clone());
        }
        args
    }

    fn normalizer(&self) -> Box<dyn Normalizer> {
        Box::new(ClaudeNormalizer::default())
    }
}

/// Translates Claude Code `stream-json` lines.
///
/// With `--include-partial-messages` text and thinking arrive as
/// `stream_event` deltas and are then repeated inside the complete
/// `assistant` message; the repeat is dropped when deltas were seen.
#[derive(Default)]
pub struct ClaudeNormalizer {
    session_id: Option<String>,
    terminal: Option<Terminal>,
    partial_text_seen: bool,
    partial_thinking_seen: bool,
    /// Tool use ids announced with the pager's tool name, so their `tool_result` renders the
    /// same row.
    calls: HashMap<String, String>,
    /// `AskUserQuestion` tool uses by id, with their questions, until the CLI either asks the
    /// host (`can_use_tool`, answered on the channel) or denies them itself (`tool_result`
    /// error: raised as [`AdapterEvent::Question`] for the resume path). Their results never
    /// render a row.
    pending_questions: HashMap<String, Vec<QuestionPrompt>>,
    answered_questions: HashSet<String>,
    /// Open `can_use_tool` requests by request id: the tool's original input (echoed back on
    /// allow, extended with the answers for a question) and the CLI's own "always" rules.
    asks: HashMap<String, ControlAsk>,
    stdin_lines: Vec<String>,
}

struct ControlAsk {
    input: Value,
    suggestions: Value,
    question: bool,
}

fn control_response(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
    .to_string()
}

fn control_error(request_id: &str, error: &str) -> String {
    json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": request_id, "error": error },
    })
    .to_string()
}

/// The pager's tool name and OpenCode-shaped input for one Claude Code tool use.
fn canonical_call(name: &str, input: &Value) -> (String, Value) {
    let s = |k: &str| json::str(input, k).map(Value::from).unwrap_or(Value::Null);
    let with = |pairs: &[(&str, Value)]| {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            if !v.is_null() {
                m.insert((*k).to_string(), v.clone());
            }
        }
        Value::Object(m)
    };
    match name {
        "Bash" => (
            tool::BASH.into(),
            with(&[("command", s("command")), ("description", s("description"))]),
        ),
        "Edit" => (
            tool::EDIT.into(),
            with(&[
                ("filePath", s("file_path")),
                ("oldString", s("old_string")),
                ("newString", s("new_string")),
            ]),
        ),
        "MultiEdit" => (tool::EDIT.into(), with(&[("filePath", s("file_path"))])),
        "NotebookEdit" => (tool::EDIT.into(), with(&[("filePath", s("notebook_path"))])),
        "Write" => (
            tool::WRITE.into(),
            with(&[("filePath", s("file_path")), ("content", s("content"))]),
        ),
        "Read" => (tool::READ.into(), with(&[("filePath", s("file_path"))])),
        "LS" => (tool::LIST.into(), with(&[("path", s("path"))])),
        "Glob" => (
            tool::GLOB.into(),
            with(&[("pattern", s("pattern")), ("path", s("path"))]),
        ),
        "Grep" => (
            tool::GREP.into(),
            with(&[("pattern", s("pattern")), ("path", s("path"))]),
        ),
        "WebFetch" => (tool::WEB_FETCH.into(), with(&[("url", s("url"))])),
        "WebSearch" => (tool::WEB_SEARCH.into(), with(&[("query", s("query"))])),
        "TodoWrite" => (
            tool::TODO_WRITE.into(),
            with(&[("todos", input.get("todos").cloned().unwrap_or(Value::Null))]),
        ),
        "Task" => (
            tool::TASK.into(),
            with(&[("description", s("description")), ("prompt", s("prompt"))]),
        ),
        // `mcp__<server>__<tool>`: the tool's own name is the row, its arguments the input.
        mcp if mcp.starts_with("mcp__") => (
            mcp.rsplit("__")
                .next()
                .filter(|t| !t.is_empty())
                .unwrap_or("mcp")
                .to_string(),
            if input.is_object() {
                input.clone()
            } else {
                json!({})
            },
        ),
        other => (
            tool::snake_case(other),
            if input.is_object() {
                input.clone()
            } else {
                json!({})
            },
        ),
    }
}

/// `AskUserQuestion { questions: [{ question, header, options: [{ label, description }],
/// multiSelect }] }` as the shared question shape.
fn questions_of(input: &Value) -> Vec<QuestionPrompt> {
    input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|q| QuestionPrompt {
            question: json::str(q, "question").unwrap_or_default().to_string(),
            header: json::str(q, "header").unwrap_or_default().to_string(),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| QuestionChoice {
                    label: json::str(o, "label").unwrap_or_default().to_string(),
                    description: json::str(o, "description").unwrap_or_default().to_string(),
                })
                .collect(),
            multiple: json::bool_of(q, "multiSelect").unwrap_or(false),
        })
        .collect()
}

/// The `tool_use_result` of a finished `Edit` / `Write` as the `Edit` row's detail: the file and
/// the unified diff built from Claude Code's `structuredPatch` hunks (`{oldStart, oldLines,
/// newStart, newLines, lines: [" ctx", "-old", "+new"]}`); `exists` says whether the file was
/// there before (a `Write` of a new file reads `Creating`).
fn edit_detail(name: &str, result: &Value) -> Option<(Option<String>, Value)> {
    if name != tool::EDIT && name != tool::WRITE {
        return None;
    }
    let path = json::str(result, "filePath")?;
    let mut metadata = json!({ "filepath": path });
    let created = json::str(result, "type") == Some("create");
    metadata["exists"] = Value::from(!created);
    let mut diff = String::new();
    for hunk in result
        .get("structuredPatch")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            json::u64_of(hunk, "oldStart"),
            json::u64_of(hunk, "oldLines"),
            json::u64_of(hunk, "newStart"),
            json::u64_of(hunk, "newLines")
        ));
        for line in hunk
            .get("lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            diff.push_str(line);
            diff.push('\n');
        }
    }
    if !diff.is_empty() {
        metadata["diff"] = Value::from(diff);
    }
    Some((Some(path.to_string()), metadata))
}

impl Normalizer for ClaudeNormalizer {
    fn on_line(&mut self, line: &str) -> Result<Vec<AdapterEvent>, NormalizeError> {
        let v: Value =
            serde_json::from_str(line).map_err(|_| NormalizeError::NotJson(truncate(line, 200)))?;
        if let Some(sid) = json::str(&v, "session_id")
            && self.session_id.is_none()
        {
            self.session_id = Some(sid.to_string());
        }
        // Subagent traffic is nested under a Task tool call; keep the main
        // transcript linear.
        if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        match json::str(&v, "type") {
            Some("stream_event") => {
                let event = v.get("event").unwrap_or(&Value::Null);
                if json::str(event, "type") == Some("content_block_delta") {
                    let delta = event.get("delta").unwrap_or(&Value::Null);
                    match json::str(delta, "type") {
                        Some("text_delta") => {
                            if let Some(text) = json::str(delta, "text") {
                                self.partial_text_seen = true;
                                events.push(AdapterEvent::TextDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(text) = json::str(delta, "thinking") {
                                self.partial_thinking_seen = true;
                                events.push(AdapterEvent::Thinking {
                                    text: text.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("assistant") => {
                let message = v.get("message").unwrap_or(&Value::Null);
                if let Some(code) = json::str(&v, "error") {
                    let text = message
                        .get("content")
                        .map(json::text_of)
                        .unwrap_or_default();
                    events.push(AdapterEvent::Error {
                        message: if text.is_empty() {
                            code.to_string()
                        } else {
                            format!("{code}: {text}")
                        },
                    });
                } else if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        match json::str(block, "type") {
                            Some("text") if !self.partial_text_seen => {
                                if let Some(text) = json::str(block, "text") {
                                    events.push(AdapterEvent::TextDelta {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            Some("thinking") if !self.partial_thinking_seen => {
                                if let Some(text) = json::str(block, "thinking") {
                                    events.push(AdapterEvent::Thinking {
                                        text: text.to_string(),
                                    });
                                }
                            }
                            Some("tool_use") => {
                                let id = json::str(block, "id").unwrap_or_default().to_string();
                                let name = json::str(block, "name").unwrap_or_default();
                                let input = block.get("input").cloned().unwrap_or(Value::Null);
                                if name == "AskUserQuestion" {
                                    // Raised once the CLI says how it handles it: as a
                                    // `can_use_tool` ask on the channel, or as its own denial.
                                    self.pending_questions.insert(id, questions_of(&input));
                                    continue;
                                }
                                let (name, input) = canonical_call(name, &input);
                                self.calls.insert(id.clone(), name.clone());
                                events.push(AdapterEvent::ToolCall { id, name, input });
                            }
                            _ => {}
                        }
                    }
                }
                self.partial_text_seen = false;
                self.partial_thinking_seen = false;
            }
            Some("control_request") => {
                let request_id = json::str(&v, "request_id").unwrap_or_default().to_string();
                let request = v.get("request").unwrap_or(&Value::Null);
                match json::str(request, "subtype") {
                    Some("can_use_tool") => {
                        let tool_name = json::str(request, "tool_name").unwrap_or_default();
                        let input = request.get("input").cloned().unwrap_or(Value::Null);
                        let suggestions = request
                            .get("permission_suggestions")
                            .cloned()
                            .unwrap_or(Value::Null);
                        let question = tool_name == "AskUserQuestion";
                        if question {
                            if let Some(tool_use_id) = json::str(request, "tool_use_id") {
                                self.pending_questions.remove(tool_use_id);
                                self.answered_questions.insert(tool_use_id.to_string());
                            }
                            events.push(AdapterEvent::Question {
                                id: request_id.clone(),
                                questions: questions_of(&input),
                            });
                        } else {
                            let (tool, input) = canonical_call(tool_name, &input);
                            events.push(AdapterEvent::PermissionAsk {
                                id: request_id.clone(),
                                tool,
                                input,
                            });
                        }
                        self.asks.insert(
                            request_id,
                            ControlAsk {
                                input: request.get("input").cloned().unwrap_or(Value::Null),
                                suggestions,
                                question,
                            },
                        );
                    }
                    // Hooks, MCP, dialogs, elicitations: nothing Workshop serves; an unanswered
                    // request would block the CLI.
                    Some(other) => self.stdin_lines.push(control_error(
                        &request_id,
                        &format!("Workshop does not handle `{other}` requests"),
                    )),
                    None => {}
                }
            }
            Some("user") => {
                let content = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array);
                for block in content.into_iter().flatten() {
                    if json::str(block, "type") == Some("tool_result") {
                        let id = json::str(block, "tool_use_id")
                            .unwrap_or_default()
                            .to_string();
                        if self.answered_questions.remove(&id) {
                            continue;
                        }
                        if let Some(questions) = self.pending_questions.remove(&id) {
                            // The CLI denied the question itself (no prompt surface): the host
                            // answers it as the next prompt of the resumed session.
                            events.push(AdapterEvent::Question { id, questions });
                            continue;
                        }
                        if let Some(name) = self.calls.remove(&id)
                            && let Some(result) = v.get("tool_use_result")
                            && let Some((title, metadata)) = edit_detail(&name, result)
                        {
                            events.push(AdapterEvent::ToolDetail {
                                id: id.clone(),
                                title,
                                metadata,
                            });
                        }
                        events.push(AdapterEvent::ToolResult {
                            id,
                            output: block.get("content").map(json::text_of).unwrap_or_default(),
                            is_error: json::bool_of(block, "is_error").unwrap_or(false),
                        });
                    }
                }
            }
            Some("result") => {
                if let Some(usage) = v.get("usage") {
                    events.push(AdapterEvent::Usage(Usage {
                        input_tokens: json::u64_of(usage, "input_tokens"),
                        output_tokens: json::u64_of(usage, "output_tokens"),
                        cache_read_tokens: json::u64_of(usage, "cache_read_input_tokens"),
                        cache_write_tokens: json::u64_of(usage, "cache_creation_input_tokens"),
                        reasoning_tokens: 0,
                        cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                    }));
                }
                let result_text = json::str(&v, "result").map(str::to_string);
                let is_error = json::bool_of(&v, "is_error").unwrap_or(false)
                    || json::str(&v, "subtype").is_some_and(|s| s != "success");
                if is_error {
                    let message = result_text
                        .clone()
                        .unwrap_or_else(|| json::str(&v, "subtype").unwrap_or("error").to_string());
                    events.push(AdapterEvent::Error {
                        message: message.clone(),
                    });
                    self.terminal = Some(Terminal::Failed(message));
                } else {
                    events.push(AdapterEvent::Done {
                        session_id: self.session_id.clone(),
                        result: result_text,
                    });
                    self.terminal = Some(Terminal::Completed);
                }
            }
            // system/init, rate_limit_event, tool_progress, keep-alives, ...
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
            "claude exited ({}) before a `result` event",
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

    fn reply(&mut self, reply: &AskReply) -> bool {
        // A permission ask takes allow/deny, a question takes answers (or a denial).
        let answerable = match (reply, self.asks.get(reply.id())) {
            (_, None) => false,
            (AskReply::Answer { .. }, Some(ask)) => ask.question,
            (AskReply::Allow { .. }, Some(ask)) => !ask.question,
            (AskReply::Deny { .. }, Some(_)) => true,
        };
        if !answerable {
            return false;
        }
        let ask = self.asks.remove(reply.id()).expect("checked above");
        let response = match reply {
            AskReply::Allow { always, .. } => {
                let mut response = json!({ "behavior": "allow", "updatedInput": ask.input });
                if *always && ask.suggestions.is_array() {
                    response["updatedPermissions"] = ask.suggestions;
                }
                response
            }
            AskReply::Deny { message, .. } => json!({ "behavior": "deny", "message": message }),
            AskReply::Answer { answers, .. } => {
                // The SDK's answer to `AskUserQuestion`: the original input plus
                // `answers: { <question>: <chosen labels> }`.
                let mut input = if ask.input.is_object() {
                    ask.input
                } else {
                    json!({})
                };
                let questions: Vec<String> = input
                    .get("questions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|q| json::str(q, "question").unwrap_or_default().to_string())
                    .collect();
                let mut map = serde_json::Map::new();
                for (question, answer) in questions.iter().zip(answers) {
                    map.insert(question.clone(), Value::from(answer.join(", ")));
                }
                input["answers"] = Value::Object(map);
                json!({ "behavior": "allow", "updatedInput": input })
            }
        };
        self.stdin_lines
            .push(control_response(reply.id(), response));
        true
    }

    fn take_stdin_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.stdin_lines)
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_only_claude_code() {
        let a = ClaudeAdapter;
        let ok = ProbeOutput {
            stdout: "2.1.278 (Claude Code)\n".into(),
            ..Default::default()
        };
        assert_eq!(a.identify(&[ok]), Some("2.1.278".to_string()));
        let other = ProbeOutput {
            stdout: "claude 1.0 (something else)\n".into(),
            ..Default::default()
        };
        assert_eq!(a.identify(&[other]), None);
    }

    #[test]
    fn status_json_maps_to_login_state() {
        let a = ClaudeAdapter;
        let out = ProbeOutput {
            stdout: r#"{"loggedIn": false, "authMethod": "none", "apiProvider": "firstParty"}"#
                .into(),
            exit_code: Some(1),
            ..Default::default()
        };
        assert_eq!(a.interpret_status(&out), LoginState::SignIn);
        let out = ProbeOutput {
            stdout: r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "x@y"}"#.into(),
            exit_code: Some(0),
            ..Default::default()
        };
        assert_eq!(
            a.interpret_status(&out),
            LoginState::Ready {
                method: Some("Claude account".into())
            }
        );
        let out = ProbeOutput {
            stdout: "Not logged in. Run claude auth login".into(),
            exit_code: Some(1),
            ..Default::default()
        };
        assert!(matches!(
            a.interpret_status(&out),
            LoginState::Unknown { .. }
        ));
    }

    #[test]
    fn pinned_run_flags() {
        let a = ClaudeAdapter;
        let mut req = RunRequest::new("hi", "/tmp");
        req.resume = Some("sess-1".into());
        req.model = Some("opus".into());
        req.permission = PermissionPolicy::WorkspaceWrite;
        assert_eq!(
            a.run_args(&req),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--input-format",
                "stream-json",
                "--permission-prompt-tool",
                "stdio",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "opus",
                "--resume",
                "sess-1"
            ]
        );
        // Plan is the read-only mode; always-approve is the CLI's own run-everything mode.
        req.permission = PermissionPolicy::ReadOnly;
        assert!(
            a.run_args(&req)
                .windows(2)
                .any(|w| w == ["--permission-mode", "plan"])
        );
        req.permission = PermissionPolicy::AlwaysApprove;
        assert!(
            a.run_args(&req)
                .windows(2)
                .any(|w| w == ["--permission-mode", "bypassPermissions"])
        );
        assert_eq!(a.prompt_delivery(), PromptDelivery::Channel);
        let lines = a.prompt_lines("summarize README");
        assert_eq!(lines.len(), 2);
        let init: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(init["type"], "control_request");
        assert_eq!(init["request"]["subtype"], "initialize");
        let user: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(user["type"], "user");
        assert_eq!(user["message"]["content"][0]["text"], "summarize README");
    }

    /// Normal mode: a command the permission mode would prompt for arrives as `can_use_tool`;
    /// the host's answer is the SDK's `control_response` (allow echoing the input, `always`
    /// carrying the CLI's own rule suggestions, or deny with the reason the model is told).
    #[test]
    fn can_use_tool_is_a_permission_ask_answered_on_the_channel() {
        let mut n = ClaudeNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cp a.png assets/","description":"Copy the wallpaper"}}]},"parent_tool_use_id":null,"session_id":"s"}"#,
                r#"{"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cp a.png assets/","description":"Copy the wallpaper"},"tool_use_id":"toolu_1","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cp:*"}],"behavior":"allow","destination":"session"}]}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                AdapterEvent::ToolCall {
                    id: "toolu_1".into(),
                    name: "bash".into(),
                    input: json!({"command": "cp a.png assets/", "description": "Copy the wallpaper"}),
                },
                AdapterEvent::PermissionAsk {
                    id: "req_1".into(),
                    tool: "bash".into(),
                    input: json!({"command": "cp a.png assets/", "description": "Copy the wallpaper"}),
                },
            ]
        );
        assert!(
            n.take_stdin_lines().is_empty(),
            "nothing is sent before the host decides"
        );
        // A question answer does not fit a permission ask; an unknown id is not ours.
        assert!(!n.reply(&AskReply::Answer {
            id: "req_1".into(),
            answers: vec![]
        }));
        assert!(!n.reply(&AskReply::Allow {
            id: "nope".into(),
            always: false
        }));
        assert!(n.reply(&AskReply::Allow {
            id: "req_1".into(),
            always: true
        }));
        let lines = n.take_stdin_lines();
        assert_eq!(lines.len(), 1);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["type"], "control_response");
        assert_eq!(v["response"]["subtype"], "success");
        assert_eq!(v["response"]["request_id"], "req_1");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(
            v["response"]["response"]["updatedInput"]["command"],
            "cp a.png assets/"
        );
        assert_eq!(
            v["response"]["response"]["updatedPermissions"][0]["rules"][0]["ruleContent"],
            "cp:*"
        );
        // Answered once.
        assert!(!n.reply(&AskReply::Allow {
            id: "req_1".into(),
            always: false
        }));

        n.on_line(r#"{"type":"control_request","request_id":"req_2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf /"},"tool_use_id":"toolu_2"}}"#).unwrap();
        assert!(n.reply(&AskReply::Deny {
            id: "req_2".into(),
            message: "The user declined this command.".into()
        }));
        let v: Value = serde_json::from_str(&n.take_stdin_lines()[0]).unwrap();
        assert_eq!(v["response"]["response"]["behavior"], "deny");
        assert_eq!(
            v["response"]["response"]["message"],
            "The user declined this command."
        );
        // A request Workshop does not serve is refused at once so the CLI never blocks on it.
        n.on_line(r#"{"type":"control_request","request_id":"req_3","request":{"subtype":"request_user_dialog","dialog":{}}}"#).unwrap();
        let v: Value = serde_json::from_str(&n.take_stdin_lines()[0]).unwrap();
        assert_eq!(v["response"]["subtype"], "error");
        assert_eq!(v["response"]["request_id"], "req_3");
    }

    /// `AskUserQuestion` on the channel: the question is raised under the request id and the
    /// answers go back as the SDK does it (`updatedInput.answers`); its `tool_use` and
    /// `tool_result` render no row.
    #[test]
    fn ask_user_question_on_the_channel_is_answered_inline() {
        let mut n = ClaudeNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{"questions":[{"question":"Which install method?","header":"Install","options":[{"label":"PPA","description":"apt repository"},{"label":".deb","description":"one file"}],"multiSelect":false}]}}]},"parent_tool_use_id":null,"session_id":"s"}"#,
                r#"{"type":"control_request","request_id":"req_q","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Which install method?","header":"Install","options":[{"label":"PPA","description":"apt repository"},{"label":".deb","description":"one file"}],"multiSelect":false}]},"tool_use_id":"toolu_q"}}"#,
            ],
        );
        assert_eq!(events.len(), 1, "{events:?}");
        let AdapterEvent::Question { id, questions } = &events[0] else {
            panic!("expected a question, got {events:?}");
        };
        assert_eq!(id, "req_q");
        assert_eq!(questions[0].options[0].label, "PPA");
        assert!(n.reply(&AskReply::Answer {
            id: "req_q".into(),
            answers: vec![vec!["PPA".into()]]
        }));
        let v: Value = serde_json::from_str(&n.take_stdin_lines()[0]).unwrap();
        assert_eq!(v["response"]["request_id"], "req_q");
        assert_eq!(v["response"]["response"]["behavior"], "allow");
        assert_eq!(
            v["response"]["response"]["updatedInput"]["answers"]["Which install method?"],
            "PPA"
        );
        assert_eq!(
            v["response"]["response"]["updatedInput"]["questions"][0]["header"],
            "Install"
        );
        // The tool then runs normally; its result is not a row.
        let after = n
            .on_line(r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_q","content":"User answered: PPA","is_error":false}]},"parent_tool_use_id":null,"session_id":"s"}"#)
            .unwrap();
        assert!(after.is_empty(), "{after:?}");
    }

    fn feed(n: &mut ClaudeNormalizer, lines: &[&str]) -> Vec<AdapterEvent> {
        lines
            .iter()
            .flat_map(|l| n.on_line(l).expect("valid line"))
            .collect()
    }

    /// Claude's `Bash` / `Edit` / `Write` / `Read` / `WebFetch` / … become the pager's rows with
    /// OpenCode's argument keys; a `Write` result says whether the file was created and an
    /// `Edit` result carries the unified diff built from `structuredPatch`.
    #[test]
    fn tool_uses_use_the_pager_vocabulary() {
        let cases: Vec<(&str, Value, &str, Value)> = vec![
            (
                "Bash",
                json!({"command": "ls -la", "description": "List files", "timeout": 5000}),
                tool::BASH,
                json!({"command": "ls -la", "description": "List files"}),
            ),
            (
                "Edit",
                json!({"file_path": "/w/a.rs", "old_string": "hi", "new_string": "hello", "replace_all": false}),
                tool::EDIT,
                json!({"filePath": "/w/a.rs", "oldString": "hi", "newString": "hello"}),
            ),
            (
                "Write",
                json!({"file_path": "/w/new.txt", "content": "one\n"}),
                tool::WRITE,
                json!({"filePath": "/w/new.txt", "content": "one\n"}),
            ),
            (
                "Read",
                json!({"file_path": "/w/README.md", "limit": 10}),
                tool::READ,
                json!({"filePath": "/w/README.md"}),
            ),
            (
                "Glob",
                json!({"pattern": "**/*.rs", "path": "/w"}),
                tool::GLOB,
                json!({"pattern": "**/*.rs", "path": "/w"}),
            ),
            (
                "Grep",
                json!({"pattern": "TODO", "output_mode": "content"}),
                tool::GREP,
                json!({"pattern": "TODO"}),
            ),
            (
                "WebFetch",
                json!({"url": "https://example.org", "prompt": "summarize"}),
                tool::WEB_FETCH,
                json!({"url": "https://example.org"}),
            ),
            (
                "WebSearch",
                json!({"query": "ghostty ubuntu"}),
                tool::WEB_SEARCH,
                json!({"query": "ghostty ubuntu"}),
            ),
            (
                "TodoWrite",
                json!({"todos": [{"content": "x", "status": "pending", "activeForm": "y"}]}),
                tool::TODO_WRITE,
                json!({"todos": [{"content": "x", "status": "pending", "activeForm": "y"}]}),
            ),
            (
                "mcp__filesystem__read_file",
                json!({"path": "/w/a"}),
                "read_file",
                json!({"path": "/w/a"}),
            ),
            (
                "ExitPlanMode",
                json!({"plan": "..."}),
                "exit_plan_mode",
                json!({"plan": "..."}),
            ),
        ];
        for (name, input, want_name, want_input) in cases {
            let (got_name, got_input) = canonical_call(name, &input);
            assert_eq!(got_name, want_name, "{name}");
            assert_eq!(got_input, want_input, "{name}");
        }

        let mut n = ClaudeNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/w/a.txt","old_string":"hi","new_string":"hello"}}]},"parent_tool_use_id":null,"session_id":"s"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"The file /w/a.txt has been updated.","is_error":false}]},"parent_tool_use_id":null,"session_id":"s","tool_use_result":{"filePath":"/w/a.txt","oldString":"hi","newString":"hello","originalFile":"hi\n","structuredPatch":[{"oldStart":1,"oldLines":1,"newStart":1,"newLines":1,"lines":["-hi","+hello"]}],"userModified":false,"replaceAll":false}}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/w/new.txt","content":"one\n"}}]},"parent_tool_use_id":null,"session_id":"s"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"File created successfully at: /w/new.txt","is_error":false}]},"parent_tool_use_id":null,"session_id":"s","tool_use_result":{"type":"create","filePath":"/w/new.txt","content":"one\n","structuredPatch":[]}}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                AdapterEvent::ToolCall {
                    id: "t1".into(),
                    name: "edit".into(),
                    input: json!({"filePath": "/w/a.txt", "oldString": "hi", "newString": "hello"}),
                },
                AdapterEvent::ToolDetail {
                    id: "t1".into(),
                    title: Some("/w/a.txt".into()),
                    metadata: json!({"filepath": "/w/a.txt", "exists": true, "diff": "@@ -1,1 +1,1 @@\n-hi\n+hello\n"}),
                },
                AdapterEvent::ToolResult {
                    id: "t1".into(),
                    output: "The file /w/a.txt has been updated.".into(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "t2".into(),
                    name: "write".into(),
                    input: json!({"filePath": "/w/new.txt", "content": "one\n"}),
                },
                AdapterEvent::ToolDetail {
                    id: "t2".into(),
                    title: Some("/w/new.txt".into()),
                    metadata: json!({"filepath": "/w/new.txt", "exists": false}),
                },
                AdapterEvent::ToolResult {
                    id: "t2".into(),
                    output: "File created successfully at: /w/new.txt".into(),
                    is_error: false,
                },
            ]
        );
    }

    /// A build that denies `AskUserQuestion` itself (no `can_use_tool` for it; the result is an
    /// error): the question is raised under the tool use id for the resume path, no plumbing
    /// row is shown, and the channel cannot carry the answer.
    #[test]
    fn ask_user_question_becomes_a_question_event() {
        let mut n = ClaudeNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"q1","name":"AskUserQuestion","input":{"questions":[{"question":"Which install method?","header":"Install","options":[{"label":"PPA","description":"apt repository"},{"label":".deb","description":"one file"}],"multiSelect":false}]}}]},"parent_tool_use_id":null,"session_id":"s"}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"q1","content":"Answering questions is not available in non-interactive mode","is_error":true}]},"parent_tool_use_id":null,"session_id":"s"}"#,
            ],
        );
        assert_eq!(
            events,
            vec![AdapterEvent::Question {
                id: "q1".into(),
                questions: vec![QuestionPrompt {
                    question: "Which install method?".into(),
                    header: "Install".into(),
                    options: vec![
                        QuestionChoice {
                            label: "PPA".into(),
                            description: "apt repository".into()
                        },
                        QuestionChoice {
                            label: ".deb".into(),
                            description: "one file".into()
                        },
                    ],
                    multiple: false,
                }],
            }]
        );
        assert!(!n.reply(&AskReply::Answer {
            id: "q1".into(),
            answers: vec![vec!["PPA".into()]]
        }));
        assert!(n.take_stdin_lines().is_empty());
    }
}
