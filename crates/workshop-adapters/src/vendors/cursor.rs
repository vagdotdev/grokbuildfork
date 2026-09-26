//! Cursor Agent CLI (`cursor-agent`, also installed as `agent`).
//!
//! Verified against Cursor Agent 2026.09.23-86fc751 (`cursor-agent --help`,
//! `cursor-agent status --help`, the headless stream-json emitter and the
//! `agent.v1` tool protobuf descriptors in the installed bundle). The installer
//! symlinks both `~/.local/bin/cursor-agent` and `~/.local/bin/agent` to the same
//! binary, so `agent` is a legitimate name — but only after the `--help` banner
//! proves it is the Cursor Agent. `--version` alone prints a bare
//! `YYYY.MM.DD-hash` and is not an identity.
//!
//! * identity, status, login: `workshop-detect` (the one detection stack, shared with the
//!   picker) verifies the binary, asks the official status command and runs the login.
//! * run:      `cursor-agent -p --output-format stream-json --stream-partial-output
//!   --trust [--mode plan | --force] [--model M] [--resume=ID] <prompt>`; prompt is
//!   the final positional argument (stdin prompt delivery is not verified for
//!   this CLI), stdin is `/dev/null`.
//!
//! Headless (`-p`) mode has no approval prompt. Without `--force` the CLI runs in
//! its allowlist mode: the read-only commands it knows run, every other command is
//! rejected and the model is told (`ShellRejected`). `--force` (`--yolo`, "Run
//! Everything") approves every tool call — Workshop's always-approve. Questions
//! (`askQuestionToolCall`) are answered by the CLI itself with "Questions skipped
//! by the user, continue with the information you already have"; the normalizer
//! raises them as [`AdapterEvent::Question`] and the host resumes the chat with
//! the user's answers as the next prompt.
//!
//! Stream shapes (headless emitter, 2026.09.23):
//! * `assistant` — with `--stream-partial-output` every text delta is one
//!   `assistant` message; the CLI also flushes the text accumulated since the
//!   last flush as one more `assistant` message before each tool call, each
//!   interaction query and the final `result`. That flush repeats what the deltas
//!   already carried and is dropped here.
//! * `tool_call` `started` / `completed` — `tool_call` is the `agent.v1.ToolCall`
//!   oneof as one protobuf-JSON key (`shellToolCall`, `editToolCall`,
//!   `readToolCall`, …) holding `args` and, once completed, `result` (itself a
//!   oneof: `success` | `failure` | `rejected` | `error` | `permissionDenied` | …).
//! * `thinking` `delta` / `completed`, `result`, `system`/`init`, `user`,
//!   `retry`, `connection`, `interaction_query`.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::claude::truncate;
use super::{json, tool};
use crate::adapter::{
    Adapter, AdapterId, NormalizeError, Normalizer, PermissionPolicy, PromptDelivery, RunRequest,
    Terminal, VersionPin,
};
use crate::event::{AdapterEvent, QuestionChoice, QuestionPrompt, Usage};

pub struct CursorAdapter;

impl Adapter for CursorAdapter {
    fn id(&self) -> AdapterId {
        AdapterId::Cursor
    }

    fn version_pin(&self) -> VersionPin {
        VersionPin {
            min_supported: "2026.09.18",
            max_tested: "2026.09.23",
        }
    }

    fn prompt_delivery(&self) -> PromptDelivery {
        PromptDelivery::Argument
    }

    fn run_args(&self, req: &RunRequest) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--trust",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        match req.permission {
            PermissionPolicy::ReadOnly => {
                args.push("--mode".into());
                args.push("plan".into());
            }
            // The CLI's own allowlist mode: edits apply, only its read-only commands run.
            PermissionPolicy::WorkspaceWrite => {}
            // "Run Everything": every command is approved, as `--yolo`.
            PermissionPolicy::AlwaysApprove => args.push("--force".into()),
        }
        if let Some(model) = &req.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(id) = &req.resume {
            // `--resume [chatId]` takes an optional value; `=` keeps the id
            // from being parsed as the prompt.
            args.push(format!("--resume={id}"));
        }
        args
    }

    fn normalizer(&self) -> Box<dyn Normalizer> {
        Box::new(CursorNormalizer::default())
    }
}

/// Translates Cursor Agent `stream-json` lines.
#[derive(Default)]
pub struct CursorNormalizer {
    session_id: Option<String>,
    terminal: Option<Terminal>,
    /// Text streamed as deltas since the CLI's last flush: the flush (one `assistant` message
    /// carrying the same text again) is recognized against it and dropped.
    streamed: String,
    /// Tool call ids announced with the pager's tool name, so the `completed` half maps the same
    /// way; question calls are raised as [`AdapterEvent::Question`] and their completion dropped.
    calls: std::collections::HashMap<String, String>,
    questions: HashSet<String>,
}

impl CursorNormalizer {
    fn on_assistant_text(&mut self, text: &str, events: &mut Vec<AdapterEvent>) {
        // A flush repeats the accumulated deltas verbatim (the CLI clears its buffer after each
        // flush); anything beyond them is new text the deltas did not carry.
        if !self.streamed.is_empty() && text.starts_with(self.streamed.as_str()) {
            let rest = &text[self.streamed.len()..];
            self.streamed.clear();
            if !rest.is_empty() {
                events.push(AdapterEvent::TextDelta {
                    text: rest.to_string(),
                });
            }
            return;
        }
        self.streamed.push_str(text);
        events.push(AdapterEvent::TextDelta {
            text: text.to_string(),
        });
    }

    fn on_tool_call(&mut self, v: &Value, events: &mut Vec<AdapterEvent>) {
        let id = json::str(v, "call_id").unwrap_or_default().to_string();
        // `tool_call` is a protobuf oneof rendered as a single-key object, e.g.
        // {"readToolCall": {"args": {...}, "result": {...}}}.
        let (key, body) = v
            .get("tool_call")
            .and_then(json::oneof)
            .map(|(k, b)| (k.to_string(), b.clone()))
            .unwrap_or_else(|| ("unknown".to_string(), Value::Null));
        let args = body.get("args").cloned().unwrap_or(Value::Null);
        match json::str(v, "subtype") {
            Some("started") => {
                if key == "askQuestionToolCall" {
                    self.questions.insert(id.clone());
                    events.push(AdapterEvent::Question {
                        id,
                        questions: questions_of(&args),
                    });
                    return;
                }
                let (name, input) = canonical_call(&key, &args, &body);
                self.calls.insert(id.clone(), name.clone());
                events.push(AdapterEvent::ToolCall { id, name, input });
            }
            Some("completed") => {
                if self.questions.remove(&id) || key == "askQuestionToolCall" {
                    return;
                }
                let name = match self.calls.remove(&id) {
                    Some(name) => name,
                    None => {
                        // A call whose start was never seen (resumed mid-stream) is still one row.
                        let (name, input) = canonical_call(&key, &args, &body);
                        events.push(AdapterEvent::ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        });
                        name
                    }
                };
                let result = body.get("result").unwrap_or(&Value::Null);
                let outcome = canonical_result(&name, &args, result);
                if outcome.title.is_some() || !outcome.metadata.is_null() {
                    events.push(AdapterEvent::ToolDetail {
                        id: id.clone(),
                        title: outcome.title,
                        metadata: outcome.metadata,
                    });
                }
                events.push(AdapterEvent::ToolResult {
                    id,
                    output: outcome.output,
                    is_error: outcome.is_error,
                });
            }
            _ => {}
        }
    }
}

impl Normalizer for CursorNormalizer {
    fn on_line(&mut self, line: &str) -> Result<Vec<AdapterEvent>, NormalizeError> {
        let v: Value =
            serde_json::from_str(line).map_err(|_| NormalizeError::NotJson(truncate(line, 200)))?;
        if let Some(sid) = json::str(&v, "session_id")
            && self.session_id.is_none()
        {
            self.session_id = Some(sid.to_string());
        }
        let mut events = Vec::new();
        match json::str(&v, "type") {
            Some("assistant") => {
                let blocks = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array);
                for block in blocks.into_iter().flatten() {
                    if json::str(block, "type") == Some("text")
                        && let Some(text) = json::str(block, "text")
                    {
                        self.on_assistant_text(text, &mut events);
                    }
                }
            }
            Some("thinking") => {
                if json::str(&v, "subtype") == Some("delta")
                    && let Some(text) = json::str(&v, "text")
                {
                    events.push(AdapterEvent::Thinking {
                        text: text.to_string(),
                    });
                }
            }
            Some("tool_call") => self.on_tool_call(&v, &mut events),
            Some("result") => {
                if let Some(usage) = v.get("usage") {
                    events.push(AdapterEvent::Usage(Usage {
                        input_tokens: json::u64_of(usage, "inputTokens"),
                        output_tokens: json::u64_of(usage, "outputTokens"),
                        cache_read_tokens: json::u64_of(usage, "cacheReadTokens"),
                        cache_write_tokens: json::u64_of(usage, "cacheWriteTokens"),
                        reasoning_tokens: 0,
                        cost_usd: None,
                    }));
                }
                let result_text = json::str(&v, "result").map(str::to_string);
                if json::bool_of(&v, "is_error").unwrap_or(false)
                    || json::str(&v, "subtype").is_some_and(|s| s != "success")
                {
                    let message =
                        result_text.unwrap_or_else(|| "cursor-agent reported an error".into());
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
            Some("error") => events.push(AdapterEvent::Error {
                message: json::str(&v, "message").unwrap_or_default().to_string(),
            }),
            // system/init, user echo, retry, connection, interaction_query, ...
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
            "cursor-agent exited ({}) before a `result` event",
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

/// `AskQuestionArgs { title, questions: [{ id, prompt, options: [{ id, label }], allowMultiple }] }`
/// as the shared question shape; a question without its own header carries the call's title.
fn questions_of(args: &Value) -> Vec<QuestionPrompt> {
    let title = json::str(args, "title").unwrap_or_default();
    args.get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|q| QuestionPrompt {
            question: json::str(q, "prompt")
                .or_else(|| json::str(q, "question"))
                .unwrap_or_default()
                .to_string(),
            header: json::str(q, "header").unwrap_or(title).to_string(),
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
            multiple: json::bool_of(q, "allowMultiple").unwrap_or(false),
        })
        .collect()
}

/// The pager's tool name and OpenCode-shaped input for one `agent.v1.ToolCall` oneof key.
fn canonical_call(key: &str, args: &Value, body: &Value) -> (String, Value) {
    let s = |k: &str| json::str(args, k).map(Value::from).unwrap_or(Value::Null);
    let with = |pairs: &[(&str, Value)]| {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            if !v.is_null() {
                m.insert((*k).to_string(), v.clone());
            }
        }
        Value::Object(m)
    };
    match key {
        "shellToolCall" => {
            let description = json::str(body, "description")
                .or_else(|| json::str(args, "description"))
                .map(Value::from)
                .unwrap_or(Value::Null);
            (
                tool::BASH.into(),
                with(&[("command", s("command")), ("description", description)]),
            )
        }
        "editToolCall" => (tool::EDIT.into(), with(&[("filePath", s("path"))])),
        "readToolCall" => (tool::READ.into(), with(&[("filePath", s("path"))])),
        "deleteToolCall" => ("delete".into(), with(&[("filePath", s("path"))])),
        "lsToolCall" => (tool::LIST.into(), with(&[("path", s("path"))])),
        "globToolCall" => (
            tool::GLOB.into(),
            with(&[
                ("pattern", s("globPattern")),
                ("path", s("targetDirectory")),
            ]),
        ),
        "grepToolCall" => (
            tool::GREP.into(),
            with(&[("pattern", s("pattern")), ("path", s("path"))]),
        ),
        "semSearchToolCall" => (tool::GREP.into(), with(&[("pattern", s("query"))])),
        "webSearchToolCall" => (tool::WEB_SEARCH.into(), with(&[("query", s("searchTerm"))])),
        "fetchToolCall" | "webFetchToolCall" => {
            (tool::WEB_FETCH.into(), with(&[("url", s("url"))]))
        }
        "updateTodosToolCall" => (
            tool::TODO_WRITE.into(),
            with(&[("todos", args.get("todos").cloned().unwrap_or(Value::Null))]),
        ),
        "readTodosToolCall" => (tool::TODO_READ.into(), json!({})),
        "taskToolCall" => (
            tool::TASK.into(),
            with(&[("description", s("description")), ("prompt", s("prompt"))]),
        ),
        "mcpToolCall" => {
            // `McpArgs { name, args: string|object, providerIdentifier, toolName, ... }`: the
            // tool's own name is the row; its arguments (a JSON string on the wire) the input.
            let name = json::str(args, "toolName")
                .or_else(|| json::str(args, "name"))
                .filter(|n| !n.is_empty())
                .unwrap_or("mcp")
                .to_string();
            let input = match args.get("args") {
                Some(Value::String(raw)) => serde_json::from_str::<Value>(raw)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({ "arguments": raw })),
                Some(v) if v.is_object() => v.clone(),
                _ => json!({}),
            };
            (name, input)
        }
        other => (
            tool::snake_case(other),
            if args.is_object() {
                args.clone()
            } else {
                json!({})
            },
        ),
    }
}

struct Outcome {
    output: String,
    is_error: bool,
    title: Option<String>,
    metadata: Value,
}

/// The text an oneof error case carries, whatever the tool named it.
fn error_text(case: &str, value: &Value) -> String {
    for key in [
        "reason",
        "error",
        "errorMessage",
        "modelVisibleError",
        "clientVisibleError",
        "message",
    ] {
        if let Some(text) = json::str(value, key).filter(|t| !t.trim().is_empty()) {
            return text.to_string();
        }
    }
    match case {
        "fileNotFound" => format!(
            "file not found: {}",
            json::str(value, "path").unwrap_or_default()
        ),
        "timeout" => match value.get("timeoutMs").and_then(Value::as_u64) {
            Some(ms) => format!("timed out after {ms} ms"),
            None => "timed out".to_string(),
        },
        other => tool::snake_case(other).replace('_', " "),
    }
}

/// The text of a successful call: the field the tool keeps its text in, else the payload.
fn success_text(name: &str, value: &Value) -> String {
    if name == tool::GLOB
        && let Some(files) = value.get("files").and_then(Value::as_array)
    {
        return files
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n");
    }
    for key in ["content", "markdown", "output", "message", "results"] {
        if let Some(text) = json::str(value, key) {
            return text.to_string();
        }
    }
    if let Some(content) = value.get("content").filter(|c| c.is_array()) {
        return json::text_of(content);
    }
    if value.is_null() || value.as_object().is_some_and(|o| o.is_empty()) {
        return String::new();
    }
    value.to_string()
}

/// One finished call as the pager shows it: `bash` carries its exit code and combined output in
/// the detail (a non-zero exit reads `exit N` on the row, as the engine's does); `edit` its diff;
/// a rejection or denial is the row's error with the CLI's own reason.
fn canonical_result(name: &str, args: &Value, result: &Value) -> Outcome {
    let Some((case, value)) = json::oneof(result) else {
        return Outcome {
            output: String::new(),
            is_error: false,
            title: None,
            metadata: Value::Null,
        };
    };
    match (name, case) {
        (n, "success" | "failure") if n == tool::BASH => {
            let stdout = json::str(value, "stdout").unwrap_or_default();
            let stderr = json::str(value, "stderr").unwrap_or_default();
            let output = match json::str(value, "interleavedOutput") {
                Some(text) if !text.is_empty() => text.to_string(),
                _ => match (stdout.is_empty(), stderr.is_empty()) {
                    (false, false) => format!("{stdout}\n{stderr}"),
                    (false, true) => stdout.to_string(),
                    _ => stderr.to_string(),
                },
            };
            let exit = value
                .get("exitCode")
                .and_then(Value::as_i64)
                .unwrap_or(if case == "success" { 0 } else { 1 });
            Outcome {
                output: output.clone(),
                is_error: false,
                title: json::str(args, "command").map(str::to_string),
                metadata: json!({ "exit": exit, "output": output }),
            }
        }
        (n, "success") if n == tool::EDIT => {
            let path = json::str(value, "path")
                .or_else(|| json::str(args, "path"))
                .unwrap_or_default();
            let before = json::str(value, "beforeFullFileContent");
            let after = json::str(value, "afterFullFileContent");
            let diff = match json::str(value, "diffString").filter(|d| !d.trim().is_empty()) {
                Some(d) => Some(d.to_string()),
                None => match (before, after) {
                    (None | Some(""), Some(after)) if !after.is_empty() => {
                        Some(json::creation_diff(after))
                    }
                    _ => None,
                },
            };
            let mut metadata = json!({
                "filepath": path,
                "exists": !matches!(before, None | Some("")),
            });
            if let Some(d) = diff {
                metadata["diff"] = Value::from(d);
            }
            Outcome {
                output: json::str(value, "message")
                    .unwrap_or("Edit applied.")
                    .to_string(),
                is_error: false,
                title: Some(path.to_string()),
                metadata,
            }
        }
        (_, "success") => Outcome {
            output: success_text(name, value),
            is_error: json::bool_of(value, "isError").unwrap_or(false),
            title: None,
            metadata: Value::Null,
        },
        // `async`, `approved`, `accepted`, `modified`: the call went through in another form.
        (_, "async" | "approved" | "accepted" | "modified") => Outcome {
            output: success_text(name, value),
            is_error: false,
            title: None,
            metadata: Value::Null,
        },
        (_, case) => Outcome {
            output: error_text(case, value),
            is_error: true,
            title: None,
            metadata: Value::Null,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(n: &mut CursorNormalizer, lines: &[&str]) -> Vec<AdapterEvent> {
        lines
            .iter()
            .flat_map(|l| n.on_line(l).expect("valid line"))
            .collect()
    }

    #[test]
    fn pinned_run_flags() {
        let a = CursorAdapter;
        let mut req = RunRequest::new("hi", "/tmp");
        req.resume = Some("chat-1".into());
        assert_eq!(
            a.run_args(&req),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust",
                "--mode",
                "plan",
                "--resume=chat-1"
            ]
        );
        req.permission = PermissionPolicy::WorkspaceWrite;
        req.resume = None;
        assert_eq!(
            a.run_args(&req),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust"
            ]
        );
    }

    /// F3: always-approve is the CLI's "Run Everything" (`--force`, alias `--yolo`); `--trust`
    /// alone only skips the workspace-trust prompt and leaves commands auto-denied in `-p`.
    #[test]
    fn always_approve_forces_every_command() {
        let a = CursorAdapter;
        let mut req = RunRequest::new("copy the PNGs", "/tmp");
        req.permission = PermissionPolicy::AlwaysApprove;
        req.model = Some("composer-2.5".into());
        assert_eq!(
            a.run_args(&req),
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--stream-partial-output",
                "--trust",
                "--force",
                "--model",
                "composer-2.5"
            ]
        );
        assert!(!a.run_args(&req).iter().any(|f| f == "--mode"));
    }

    /// F5: with `--stream-partial-output` the CLI streams each delta as an `assistant` message and
    /// then flushes the accumulated text as one more `assistant` message (before a tool call and
    /// before `result`); the flush must not print the paragraph a second time.
    #[test]
    fn flushed_assistant_text_is_not_repeated() {
        let mut n = CursorNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Writing the page "}]},"session_id":"s","timestamp_ms":1}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first."}]},"session_id":"s","timestamp_ms":2}"#,
                // The flush right before a tool call carries `model_call_id`.
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Writing the page first."}]},"session_id":"s","model_call_id":"mc_1","timestamp_ms":3}"#,
                r#"{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"readToolCall":{"args":{"path":"index.html"}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"readToolCall":{"args":{"path":"index.html"},"result":{"success":{"content":"<html>","isEmpty":false}}}},"session_id":"s"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]},"session_id":"s","timestamp_ms":4}"#,
                // The final flush before `result` has no timestamp.
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]},"session_id":"s"}"#,
                r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"result":"Writing the page first.Done.","session_id":"s","request_id":"r"}"#,
            ],
        );
        let text: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AdapterEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, vec!["Writing the page ", "first.", "Done."]);
        assert!(matches!(events.last(), Some(AdapterEvent::Done { .. })));
    }

    /// Without `--stream-partial-output` (or when a flush carries more than the deltas did) the
    /// text still arrives exactly once.
    #[test]
    fn whole_messages_and_longer_flushes_still_arrive_once() {
        let mut n = CursorNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello."}]},"session_id":"s"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello. And more."}]},"session_id":"s"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Unrelated."}]},"session_id":"s"}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                AdapterEvent::TextDelta {
                    text: "Hello.".into()
                },
                AdapterEvent::TextDelta {
                    text: " And more.".into()
                },
                AdapterEvent::TextDelta {
                    text: "Unrelated.".into()
                },
            ]
        );
    }

    /// F4: the protobuf oneof keys become the pager's own tool rows with OpenCode's argument
    /// keys, so a Cursor turn reads `Run cp …`, `Edit index.html`, `Read …`, `Web search: …`.
    #[test]
    fn tool_calls_use_the_pager_vocabulary() {
        let cases: Vec<(&str, Value, &str, Value)> = vec![
            (
                "shellToolCall",
                json!({"command": "cp a.png assets/", "workingDirectory": "/w", "timeout": 30000}),
                tool::BASH,
                json!({"command": "cp a.png assets/"}),
            ),
            (
                "editToolCall",
                json!({"path": "/w/index.html", "streamContent": "<html>"}),
                tool::EDIT,
                json!({"filePath": "/w/index.html"}),
            ),
            (
                "readToolCall",
                json!({"path": "/w/README.md", "offset": 1}),
                tool::READ,
                json!({"filePath": "/w/README.md"}),
            ),
            (
                "lsToolCall",
                json!({"path": "/w", "ignore": []}),
                tool::LIST,
                json!({"path": "/w"}),
            ),
            (
                "globToolCall",
                json!({"globPattern": "**/*.png", "targetDirectory": "/w"}),
                tool::GLOB,
                json!({"pattern": "**/*.png", "path": "/w"}),
            ),
            (
                "grepToolCall",
                json!({"pattern": "TODO", "path": "/w", "outputMode": "content"}),
                tool::GREP,
                json!({"pattern": "TODO", "path": "/w"}),
            ),
            (
                "webSearchToolCall",
                json!({"searchTerm": "ghostty ubuntu"}),
                tool::WEB_SEARCH,
                json!({"query": "ghostty ubuntu"}),
            ),
            (
                "fetchToolCall",
                json!({"url": "https://example.org"}),
                tool::WEB_FETCH,
                json!({"url": "https://example.org"}),
            ),
            (
                "webFetchToolCall",
                json!({"url": "https://example.org/a"}),
                tool::WEB_FETCH,
                json!({"url": "https://example.org/a"}),
            ),
            (
                "updateTodosToolCall",
                json!({"todos": [{"content": "x", "status": "pending"}], "merge": true}),
                tool::TODO_WRITE,
                json!({"todos": [{"content": "x", "status": "pending"}]}),
            ),
            (
                "mcpToolCall",
                json!({"name": "filesystem/read_file", "toolName": "read_file", "args": "{\"path\":\"/w/a\"}"}),
                "read_file",
                json!({"path": "/w/a"}),
            ),
            (
                "switchModeToolCall",
                json!({"targetModeId": "agent"}),
                "switch_mode",
                json!({"targetModeId": "agent"}),
            ),
        ];
        for (key, args, want_name, want_input) in cases {
            let (name, input) = canonical_call(key, &args, &Value::Null);
            assert_eq!(name, want_name, "{key}");
            assert_eq!(input, want_input, "{key}");
        }
        // The shell call's description sits beside `args` on the wire.
        let (_, input) = canonical_call(
            "shellToolCall",
            &json!({"command": "ls"}),
            &json!({"args": {"command": "ls"}, "description": "List files"}),
        );
        assert_eq!(input, json!({"command": "ls", "description": "List files"}));
    }

    /// A shell result carries its exit code and output for the `Run` row (`exit 2` on a
    /// failure); a rejection — the auto-deny of a non-allowlisted command in `-p` — is the row's
    /// error with the CLI's own reason; an edit result carries its diff for the `Edit` row.
    #[test]
    fn results_carry_exit_diff_and_rejections() {
        let mut n = CursorNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"ls /nope"}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"ls /nope"},"result":{"failure":{"command":"ls /nope","exitCode":2,"stdout":"","stderr":"ls: cannot access '/nope'"}}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"started","call_id":"c2","tool_call":{"shellToolCall":{"args":{"command":"cp a b"}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"c2","tool_call":{"shellToolCall":{"args":{"command":"cp a b"},"result":{"rejected":{"command":"cp a b","reason":"Command requires approval, which is unavailable in headless mode","isReadonly":false}}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"started","call_id":"c3","tool_call":{"editToolCall":{"args":{"path":"/w/index.html"}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"c3","tool_call":{"editToolCall":{"args":{"path":"/w/index.html"},"result":{"success":{"path":"/w/index.html","linesAdded":1,"linesRemoved":0,"afterFullFileContent":"<html>\n","message":"Created /w/index.html"}}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"started","call_id":"c4","tool_call":{"shellToolCall":{"args":{"command":"echo hi"}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"c4","tool_call":{"shellToolCall":{"args":{"command":"echo hi"},"result":{"success":{"command":"echo hi","exitCode":0,"stdout":"hi\n","stderr":"","interleavedOutput":"hi\n"}}}},"session_id":"s"}"#,
            ],
        );
        assert_eq!(
            events,
            vec![
                AdapterEvent::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    input: json!({"command": "ls /nope"}),
                },
                AdapterEvent::ToolDetail {
                    id: "c1".into(),
                    title: Some("ls /nope".into()),
                    metadata: json!({"exit": 2, "output": "ls: cannot access '/nope'"}),
                },
                AdapterEvent::ToolResult {
                    id: "c1".into(),
                    output: "ls: cannot access '/nope'".into(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "c2".into(),
                    name: "bash".into(),
                    input: json!({"command": "cp a b"}),
                },
                AdapterEvent::ToolResult {
                    id: "c2".into(),
                    output: "Command requires approval, which is unavailable in headless mode"
                        .into(),
                    is_error: true,
                },
                AdapterEvent::ToolCall {
                    id: "c3".into(),
                    name: "edit".into(),
                    input: json!({"filePath": "/w/index.html"}),
                },
                AdapterEvent::ToolDetail {
                    id: "c3".into(),
                    title: Some("/w/index.html".into()),
                    metadata: json!({
                        "filepath": "/w/index.html",
                        "exists": false,
                        "diff": "@@ -0,0 +1,1 @@\n+<html>\n",
                    }),
                },
                AdapterEvent::ToolResult {
                    id: "c3".into(),
                    output: "Created /w/index.html".into(),
                    is_error: false,
                },
                AdapterEvent::ToolCall {
                    id: "c4".into(),
                    name: "bash".into(),
                    input: json!({"command": "echo hi"}),
                },
                AdapterEvent::ToolDetail {
                    id: "c4".into(),
                    title: Some("echo hi".into()),
                    metadata: json!({"exit": 0, "output": "hi\n"}),
                },
                AdapterEvent::ToolResult {
                    id: "c4".into(),
                    output: "hi\n".into(),
                    is_error: false,
                },
            ]
        );
        // An edit that changed an existing file keeps the CLI's own diff.
        let outcome = canonical_result(
            tool::EDIT,
            &json!({"path": "/w/a.txt"}),
            &json!({"success": {"path": "/w/a.txt", "beforeFullFileContent": "hi\n", "afterFullFileContent": "hello\n", "diffString": "@@ -1,1 +1,1 @@\n-hi\n+hello\n"}}),
        );
        assert_eq!(outcome.metadata["diff"], "@@ -1,1 +1,1 @@\n-hi\n+hello\n");
        assert_eq!(outcome.metadata["exists"], true);
        // Every other oneof error case is the row's error text.
        let denied = canonical_result(
            tool::READ,
            &json!({"path": "/etc/shadow"}),
            &json!({"permissionDenied": {"path": "/etc/shadow", "error": "outside the workspace"}}),
        );
        assert!(denied.is_error);
        assert_eq!(denied.output, "outside the workspace");
        let missing = canonical_result(
            tool::READ,
            &json!({"path": "/w/x"}),
            &json!({"fileNotFound": {"path": "/w/x"}}),
        );
        assert!(missing.is_error);
        assert_eq!(missing.output, "file not found: /w/x");
    }

    /// The CLI rejects `askQuestionToolCall` itself in `-p` mode; Workshop raises the question
    /// (as the engine's `question` tool) and shows no plumbing row for the call.
    #[test]
    fn ask_question_becomes_a_question_event() {
        let mut n = CursorNormalizer::default();
        let events = feed(
            &mut n,
            &[
                r#"{"type":"tool_call","subtype":"started","call_id":"q1","tool_call":{"askQuestionToolCall":{"args":{"title":"Wallpaper","questions":[{"id":"q_1","prompt":"Which wallpaper should the page use?","options":[{"id":"o_1","label":"Mountains"},{"id":"o_2","label":"Ocean"}],"allowMultiple":false}]}}},"session_id":"s"}"#,
                r#"{"type":"tool_call","subtype":"completed","call_id":"q1","tool_call":{"askQuestionToolCall":{"args":{"title":"Wallpaper","questions":[]},"result":{"rejected":{"reason":"Questions skipped by the user, continue with the information you already have"}}}},"session_id":"s"}"#,
            ],
        );
        assert_eq!(
            events,
            vec![AdapterEvent::Question {
                id: "q1".into(),
                questions: vec![QuestionPrompt {
                    question: "Which wallpaper should the page use?".into(),
                    header: "Wallpaper".into(),
                    options: vec![
                        QuestionChoice {
                            label: "Mountains".into(),
                            description: String::new()
                        },
                        QuestionChoice {
                            label: "Ocean".into(),
                            description: String::new()
                        },
                    ],
                    multiple: false,
                }],
            }]
        );
    }
}
