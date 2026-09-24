//! Normalize one session's `opencode serve` event stream (`GET /event`) into
//! [`AdapterEvent`]s for a single turn.
//!
//! Shapes verified live against opencode 1.18.31 (see the fixtures under
//! `tests/fixtures/opencode_serve_*.jsonl`, captured from a real keyless
//! Big Pickle turn):
//!
//! * `message.part.delta { sessionID, messageID, partID, field, delta }` —
//!   streamed text (`field: "text"`) or reasoning.
//! * `message.part.updated { part }` — `tool` parts move
//!   `pending -> running -> completed | error`; `text`/`reasoning` parts carry
//!   the full text once `time.end` is set; `step-finish` carries `tokens`/`cost`.
//! * `message.updated { info }` — assistant `info.error` (e.g.
//!   `MessageAbortedError`) and `info.finish`.
//! * `permission.updated` / `permission.asked` — the agent wants approval. 1.18.31 emits
//!   `permission.asked { id, sessionID, permission, patterns, metadata, always, tool: { callID } }`
//!   (verified live); the tagged types still describe the older
//!   `permission.updated { id, type, title, pattern, callID }`. Both are accepted.
//! * `session.error { error }` — turn-level failure (also emitted on abort).
//! * `session.status { status: { type } }` / `session.idle` — `idle` after
//!   `busy` ends the turn.
//! * `session.created` / `session.updated { sessionID, info: { id, parentID } }` — a
//!   subagent's session (the `task` tool creates it with `parentID` = the turn's session, and
//!   resumes one with `task_id`). Its `permission.asked` / `question.asked` are the turn's to
//!   answer: the child runs under the same user, and an ask nobody answers hangs the `task` call
//!   for good. Everything else a child says (its text, tool parts, idle) stays with the child:
//!   the parent's `task` row is what the user sees.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::Terminal;
use crate::event::{AdapterEvent, Usage};
use crate::vendors::json;

/// A permission the agent asked for during a turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub session_id: String,
    /// OpenCode permission kind, e.g. `edit`, `bash`, `external_directory`.
    pub kind: String,
    /// What is being asked for, in the user's terms: the command for `bash`, the file for
    /// `edit`, else the patterns. Servers that send a `title` keep it.
    pub title: String,
    pub patterns: Vec<String>,
    pub call_id: Option<String>,
    /// The ask's own detail: `{ command }` for `bash`, `{ filepath, diff }` for `edit` (the
    /// unified diff the agent wants to apply). Empty object when the server sends none.
    #[serde(default)]
    pub metadata: Value,
    /// The patterns an `always` reply would approve for the rest of the session (`rm *`).
    #[serde(default)]
    pub always: Vec<String>,
}

impl PermissionRequest {
    /// The shell command behind a `bash` ask, when the server included it.
    pub fn command(&self) -> Option<&str> {
        json::str(&self.metadata, "command")
    }

    /// The file behind an `edit` ask, when the server included it.
    pub fn file_path(&self) -> Option<&str> {
        json::str(&self.metadata, "filepath").or_else(|| json::str(&self.metadata, "filePath"))
    }

    /// The unified diff an `edit` ask wants to apply, when the server included it.
    pub fn diff(&self) -> Option<&str> {
        json::str(&self.metadata, "diff").filter(|d| !d.trim().is_empty())
    }
}

/// One choice of a [`QuestionPrompt`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionChoice {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// One question of a [`QuestionRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionChoice>,
    /// More than one choice may be picked.
    #[serde(default)]
    pub multiple: bool,
}

/// The agent asked the user something (OpenCode's `question` tool, 1.18.31 `question.asked`):
/// answered with one list of chosen labels (or typed text) per question, or rejected.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionRequest {
    pub id: String,
    pub session_id: String,
    pub questions: Vec<QuestionPrompt>,
    pub call_id: Option<String>,
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn session_of(event: &Value) -> Option<&str> {
    let p = event.get("properties")?;
    json::str(p, "sessionID")
        .or_else(|| p.get("part").and_then(|part| json::str(part, "sessionID")))
        .or_else(|| p.get("info").and_then(|info| json::str(info, "sessionID")))
}

fn error_text(err: &Value) -> String {
    err.get("data")
        .and_then(|d| json::str(d, "message"))
        .or_else(|| json::str(err, "message"))
        .or_else(|| json::str(err, "name"))
        .map(str::to_string)
        .unwrap_or_else(|| err.to_string())
}

fn error_name(err: &Value) -> Option<&str> {
    json::str(err, "name")
}

/// Per-turn state machine.
pub struct ServeTurn {
    session_id: String,
    /// Subagent sessions this turn started (or resumed), by `session.created` / `session.updated`
    /// events whose `info.parentID` is the turn's session or another tracked child.
    child_sessions: HashSet<String>,
    streamed_parts: HashSet<String>,
    /// Parts announced as `reasoning` (`message.part.updated` precedes their deltas): a delta on
    /// one of these is the model thinking, not its answer — the delta's own `field` is `text`
    /// for both kinds on 1.18.31, so the part type is what tells them apart.
    reasoning_parts: HashSet<String>,
    announced_calls: HashSet<String>,
    /// Tool calls announced and not yet finished: while one runs (a long `apt install`, a test
    /// suite) the server has nothing to say, and that silence is not a stall.
    running_calls: HashSet<String>,
    saw_busy: bool,
    last_text: Option<String>,
    error: Option<String>,
    aborted: bool,
    terminal: Option<Terminal>,
    pending_permissions: Vec<PermissionRequest>,
    pending_questions: Vec<QuestionRequest>,
}

impl ServeTurn {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            child_sessions: HashSet::new(),
            streamed_parts: HashSet::new(),
            reasoning_parts: HashSet::new(),
            announced_calls: HashSet::new(),
            running_calls: HashSet::new(),
            saw_busy: false,
            last_text: None,
            error: None,
            aborted: false,
            terminal: None,
            pending_permissions: Vec::new(),
            pending_questions: Vec::new(),
        }
    }

    pub fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// True once the server reported the turn as aborted.
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// Tool calls the server announced and has not finished yet.
    pub fn tools_running(&self) -> usize {
        self.running_calls.len()
    }

    /// Permission requests seen since the last drain.
    pub fn take_permissions(&mut self) -> Vec<PermissionRequest> {
        std::mem::take(&mut self.pending_permissions)
    }

    /// Questions for the user seen since the last drain.
    pub fn take_questions(&mut self) -> Vec<QuestionRequest> {
        std::mem::take(&mut self.pending_questions)
    }

    /// Subagent sessions this turn has seen start (or resume).
    pub fn child_sessions(&self) -> &HashSet<String> {
        &self.child_sessions
    }

    /// Feed one SSE event (any session). The turn's own session is followed in full; a subagent
    /// session the turn started contributes only its asks (permissions, questions); every other
    /// session is ignored.
    pub fn on_event(&mut self, event: &Value) -> Vec<AdapterEvent> {
        let kind = json::str(event, "type");
        let props = event.get("properties").unwrap_or(&Value::Null);
        if matches!(kind, Some("session.created") | Some("session.updated")) {
            // `{ sessionID, info: { id, parentID } }`: a child of this turn's session (or of one
            // of its children) is the turn's to answer for.
            let info = props.get("info").unwrap_or(&Value::Null);
            if let (Some(id), Some(parent)) = (json::str(info, "id"), json::str(info, "parentID"))
                && id != self.session_id
                && (parent == self.session_id || self.child_sessions.contains(parent))
            {
                self.child_sessions.insert(id.to_string());
            }
            return Vec::new();
        }
        let Some(session) = session_of(event) else {
            return Vec::new();
        };
        if session != self.session_id {
            if self.child_sessions.contains(session)
                && matches!(
                    kind,
                    Some("permission.updated") | Some("permission.asked") | Some("question.asked")
                )
            {
                self.on_ask(kind, props, session);
            }
            return Vec::new();
        }
        let mut out = Vec::new();
        match kind {
            Some("message.part.delta") => {
                let part_id = json::str(props, "partID").unwrap_or_default();
                if !part_id.is_empty() {
                    self.streamed_parts.insert(part_id.to_string());
                }
                let delta = json::str(props, "delta").unwrap_or_default().to_string();
                if !delta.is_empty() {
                    let reasoning = json::str(props, "field") == Some("reasoning")
                        || self.reasoning_parts.contains(part_id);
                    if reasoning {
                        out.push(AdapterEvent::Thinking { text: delta });
                    } else {
                        out.push(AdapterEvent::TextDelta { text: delta });
                    }
                }
            }
            Some("message.part.updated") => {
                let part = props.get("part").unwrap_or(&Value::Null);
                let part_id = json::str(part, "id").unwrap_or_default();
                let finished = part
                    .get("time")
                    .and_then(|t| t.get("end"))
                    .is_some_and(|e| !e.is_null());
                if json::str(part, "type") == Some("reasoning") && !part_id.is_empty() {
                    self.reasoning_parts.insert(part_id.to_string());
                }
                match json::str(part, "type") {
                    Some("text") if finished => {
                        let text = json::str(part, "text").unwrap_or_default().to_string();
                        // Only count the assistant's own text; the echoed user
                        // prompt is a text part too but never streams deltas
                        // and precedes `busy`.
                        if self.saw_busy {
                            self.last_text = Some(text.clone());
                            if !self.streamed_parts.contains(part_id) && !text.is_empty() {
                                out.push(AdapterEvent::TextDelta { text });
                            }
                        }
                    }
                    Some("reasoning") if finished => {
                        if !self.streamed_parts.contains(part_id) {
                            let text = json::str(part, "text").unwrap_or_default().to_string();
                            if !text.is_empty() {
                                out.push(AdapterEvent::Thinking { text });
                            }
                        }
                    }
                    Some("tool") => {
                        let call_id = json::str(part, "callID").unwrap_or(part_id).to_string();
                        let state = part.get("state").unwrap_or(&Value::Null);
                        let status = json::str(state, "status").unwrap_or_default();
                        let announce = |this: &mut Self, out: &mut Vec<AdapterEvent>| {
                            if this.announced_calls.insert(call_id.clone()) {
                                out.push(AdapterEvent::ToolCall {
                                    id: call_id.clone(),
                                    name: json::str(part, "tool").unwrap_or_default().to_string(),
                                    input: state.get("input").cloned().unwrap_or(Value::Null),
                                });
                            }
                        };
                        let detail = |out: &mut Vec<AdapterEvent>| {
                            let metadata = state.get("metadata").cloned().unwrap_or(Value::Null);
                            let title = json::str(state, "title")
                                .filter(|t| !t.is_empty())
                                .map(str::to_string);
                            if title.is_some() || !metadata.is_null() {
                                out.push(AdapterEvent::ToolDetail {
                                    id: call_id.clone(),
                                    title,
                                    metadata,
                                });
                            }
                        };
                        match status {
                            "running" => {
                                announce(self, &mut out);
                                self.running_calls.insert(call_id);
                            }
                            "completed" => {
                                announce(self, &mut out);
                                self.running_calls.remove(&call_id);
                                detail(&mut out);
                                out.push(AdapterEvent::ToolResult {
                                    id: call_id,
                                    output: json::str(state, "output")
                                        .unwrap_or_default()
                                        .to_string(),
                                    is_error: false,
                                });
                            }
                            "error" => {
                                announce(self, &mut out);
                                self.running_calls.remove(&call_id);
                                detail(&mut out);
                                out.push(AdapterEvent::ToolResult {
                                    id: call_id,
                                    output: json::str(state, "error")
                                        .unwrap_or_default()
                                        .to_string(),
                                    is_error: true,
                                });
                            }
                            // pending: arguments still streaming
                            _ => {}
                        }
                    }
                    Some("step-finish") => {
                        let tokens = part.get("tokens").unwrap_or(&Value::Null);
                        let cache = tokens.get("cache").unwrap_or(&Value::Null);
                        out.push(AdapterEvent::Usage(Usage {
                            input_tokens: json::u64_of(tokens, "input"),
                            output_tokens: json::u64_of(tokens, "output"),
                            cache_read_tokens: json::u64_of(cache, "read"),
                            cache_write_tokens: json::u64_of(cache, "write"),
                            reasoning_tokens: json::u64_of(tokens, "reasoning"),
                            cost_usd: part.get("cost").and_then(Value::as_f64),
                        }));
                    }
                    // step-start, patch, snapshot, agent, retry, compaction, ...
                    _ => {}
                }
            }
            Some("message.updated") => {
                let info = props.get("info").unwrap_or(&Value::Null);
                if json::str(info, "role") == Some("assistant") {
                    self.saw_busy = true;
                    if let Some(err) = info.get("error").filter(|e| !e.is_null()) {
                        self.note_error(err);
                    }
                }
            }
            Some("session.status") => {
                match props.get("status").and_then(|s| json::str(s, "type")) {
                    Some("busy") | Some("retry") => self.saw_busy = true,
                    Some("idle") => self.on_idle(&mut out),
                    _ => {}
                }
            }
            Some("session.idle") => self.on_idle(&mut out),
            Some("session.error") => {
                if let Some(err) = props.get("error") {
                    let message = error_text(err);
                    self.note_error(err);
                    if !self.aborted {
                        out.push(AdapterEvent::Error { message });
                    }
                }
            }
            Some("question.asked") | Some("permission.updated") | Some("permission.asked") => {
                let session = self.session_id.clone();
                self.on_ask(kind, props, &session);
            }
            _ => {}
        }
        out
    }

    /// Queue an ask (`permission.asked` / `permission.updated` / `question.asked`) from
    /// `session` — the turn's own or a child's; the reply goes back to that session.
    fn on_ask(&mut self, kind: Option<&str>, props: &Value, session: &str) {
        match kind {
            Some("question.asked") => {
                let questions = props
                    .get("questions")
                    .cloned()
                    .and_then(|q| serde_json::from_value(q).ok())
                    .unwrap_or_default();
                self.pending_questions.push(QuestionRequest {
                    id: json::str(props, "id").unwrap_or_default().to_string(),
                    session_id: session.to_string(),
                    questions,
                    call_id: props
                        .get("tool")
                        .and_then(|t| json::str(t, "callID"))
                        .map(str::to_string),
                });
            }
            Some("permission.updated") | Some("permission.asked") => {
                // 1.18.31: `permission` + `patterns` + `metadata` + `always` + `tool.callID`;
                // older servers: `type` + `pattern` + `title` + `callID`.
                let patterns = string_list(props.get("patterns").or_else(|| props.get("pattern")));
                let metadata = props
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default()));
                let kind = json::str(props, "permission")
                    .or_else(|| json::str(props, "type"))
                    .unwrap_or_default()
                    .to_string();
                let title = json::str(props, "title")
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .or_else(|| json::str(&metadata, "command").map(str::to_string))
                    .or_else(|| json::str(&metadata, "filepath").map(str::to_string))
                    .unwrap_or_else(|| patterns.join(", "));
                let call_id = json::str(props, "callID")
                    .or_else(|| props.get("tool").and_then(|t| json::str(t, "callID")))
                    .map(str::to_string);
                self.pending_permissions.push(PermissionRequest {
                    id: json::str(props, "id").unwrap_or_default().to_string(),
                    session_id: session.to_string(),
                    kind,
                    title,
                    patterns,
                    call_id,
                    metadata,
                    always: string_list(props.get("always")),
                });
            }
            _ => {}
        }
    }

    fn note_error(&mut self, err: &Value) {
        if error_name(err) == Some("MessageAbortedError") {
            self.aborted = true;
        }
        if self.error.is_none() {
            self.error = Some(error_text(err));
        }
    }

    fn on_idle(&mut self, out: &mut Vec<AdapterEvent>) {
        // An idle before the turn even started busy is stale state from a
        // previous turn on this session.
        if !self.saw_busy || self.terminal.is_some() {
            return;
        }
        if let Some(err) = self.error.clone() {
            self.terminal = Some(Terminal::Failed(err));
        } else {
            out.push(AdapterEvent::Done {
                session_id: Some(self.session_id.clone()),
                result: self.last_text.clone(),
            });
            self.terminal = Some(Terminal::Completed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SID: &str = "ses_1";

    fn ev(t: &str, props: Value) -> Value {
        json!({"type": t, "properties": props})
    }

    #[test]
    fn full_turn_with_tool_call_normalizes() {
        let mut turn = ServeTurn::new(SID);
        let mut events = Vec::new();
        let stream = vec![
            ev(
                "message.updated",
                json!({"info": {"id": "m1", "sessionID": SID, "role": "user"}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p0", "sessionID": SID, "type": "text", "text": "prompt", "time": {"start": 1, "end": 2}}}),
            ),
            ev(
                "session.status",
                json!({"sessionID": SID, "status": {"type": "busy"}}),
            ),
            ev(
                "message.updated",
                json!({"info": {"id": "m2", "sessionID": SID, "role": "assistant"}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p1", "sessionID": SID, "type": "tool", "tool": "write", "callID": "c1", "state": {"status": "pending", "input": {}, "raw": ""}}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p1", "sessionID": SID, "type": "tool", "tool": "write", "callID": "c1", "state": {"status": "running", "input": {"filePath": "/w/hello.txt", "content": "hi"}, "time": {"start": 3}}}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p1", "sessionID": SID, "type": "tool", "tool": "write", "callID": "c1", "state": {"status": "completed", "input": {"filePath": "/w/hello.txt", "content": "hi"}, "output": "Wrote file successfully.", "title": "hello.txt", "metadata": {}, "time": {"start": 3, "end": 4}}}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p2", "sessionID": SID, "type": "step-finish", "reason": "tool-calls", "cost": 0, "tokens": {"total": 8384, "input": 6518, "output": 74, "reasoning": 0, "cache": {"read": 1792, "write": 0}}}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p3", "sessionID": SID, "type": "text", "text": "", "time": {"start": 5}}}),
            ),
            ev(
                "message.part.delta",
                json!({"sessionID": SID, "messageID": "m3", "partID": "p3", "field": "text", "delta": "Created "}),
            ),
            ev(
                "message.part.delta",
                json!({"sessionID": SID, "messageID": "m3", "partID": "p3", "field": "text", "delta": "hello.txt."}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p3", "sessionID": SID, "type": "text", "text": "Created hello.txt.", "time": {"start": 5, "end": 6}}}),
            ),
            ev(
                "message.part.updated",
                json!({"part": {"id": "p4", "sessionID": SID, "type": "step-finish", "reason": "stop", "cost": 0, "tokens": {"total": 8408, "input": 206, "output": 10, "reasoning": 0, "cache": {"read": 8192, "write": 0}}}}),
            ),
            ev(
                "session.status",
                json!({"sessionID": "other", "status": {"type": "idle"}}),
            ),
            ev(
                "session.status",
                json!({"sessionID": SID, "status": {"type": "idle"}}),
            ),
            ev("session.idle", json!({"sessionID": SID})),
        ];
        for e in &stream {
            events.extend(turn.on_event(e));
        }
        assert_eq!(
            events,
            vec![
                AdapterEvent::ToolCall {
                    id: "c1".into(),
                    name: "write".into(),
                    input: json!({"filePath": "/w/hello.txt", "content": "hi"})
                },
                AdapterEvent::ToolDetail {
                    id: "c1".into(),
                    title: Some("hello.txt".into()),
                    metadata: json!({}),
                },
                AdapterEvent::ToolResult {
                    id: "c1".into(),
                    output: "Wrote file successfully.".into(),
                    is_error: false
                },
                AdapterEvent::Usage(Usage {
                    input_tokens: 6518,
                    output_tokens: 74,
                    cache_read_tokens: 1792,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    cost_usd: Some(0.0)
                }),
                AdapterEvent::TextDelta {
                    text: "Created ".into()
                },
                AdapterEvent::TextDelta {
                    text: "hello.txt.".into()
                },
                AdapterEvent::Usage(Usage {
                    input_tokens: 206,
                    output_tokens: 10,
                    cache_read_tokens: 8192,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    cost_usd: Some(0.0)
                }),
                AdapterEvent::Done {
                    session_id: Some(SID.into()),
                    result: Some("Created hello.txt.".into())
                },
            ]
        );
        assert_eq!(turn.terminal(), Some(&Terminal::Completed));
    }

    #[test]
    fn abort_is_recognized_and_stale_idle_ignored() {
        let mut turn = ServeTurn::new(SID);
        assert!(
            turn.on_event(&ev("session.idle", json!({"sessionID": SID})))
                .is_empty()
        );
        assert!(turn.terminal().is_none(), "idle before busy is stale");
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        let out = turn.on_event(&ev("session.error", json!({"sessionID": SID, "error": {"name": "MessageAbortedError", "data": {"message": "Aborted"}}})));
        assert!(out.is_empty(), "abort is not surfaced as a vendor error");
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "idle"}}),
        ));
        assert!(turn.aborted());
        assert_eq!(turn.terminal(), Some(&Terminal::Failed("Aborted".into())));
    }

    #[test]
    fn provider_error_fails_turn_and_permissions_are_collected() {
        let mut turn = ServeTurn::new(SID);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        turn.on_event(&ev("permission.updated", json!({"id": "perm1", "type": "bash", "pattern": ["rm -rf *"], "sessionID": SID, "messageID": "m", "callID": "c9", "title": "rm -rf build", "metadata": {}, "time": {"created": 1}})));
        let perms = turn.take_permissions();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].kind, "bash");
        assert_eq!(perms[0].patterns, vec!["rm -rf *"]);
        assert_eq!(perms[0].title, "rm -rf build");
        assert_eq!(perms[0].call_id.as_deref(), Some("c9"));
        assert!(turn.take_permissions().is_empty());
        let out = turn.on_event(&ev("session.error", json!({"sessionID": SID, "error": {"name": "ProviderAuthError", "data": {"message": "Invalid API key"}}})));
        assert_eq!(
            out,
            vec![AdapterEvent::Error {
                message: "Invalid API key".into()
            }]
        );
        turn.on_event(&ev("session.idle", json!({"sessionID": SID})));
        assert_eq!(
            turn.terminal(),
            Some(&Terminal::Failed("Invalid API key".into()))
        );
    }

    /// The 1.18.31 ask shape, verbatim from a live keyless turn (`permission.asked` with
    /// `permission`/`patterns`/`metadata`/`always`/`tool.callID`, no `title`).
    #[test]
    fn permission_asked_1_18_31_shape_is_normalized() {
        let mut turn = ServeTurn::new(SID);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        turn.on_event(&ev("permission.asked", json!({
            "id": "per_0cc87824a0012w8t1Xbv4ROYWy", "sessionID": SID, "permission": "bash",
            "patterns": ["rm -rf tmp"], "metadata": {"command": "rm -rf tmp"}, "always": ["rm *"],
            "tool": {"messageID": "msg_1", "callID": "call_167b87f253a64db1b318af12"}
        })));
        turn.on_event(&ev("permission.asked", json!({
            "id": "per_edit", "sessionID": SID, "permission": "edit", "patterns": ["hello.txt"],
            "metadata": {"filepath": "/w/hello.txt", "diff": "--- a\n+++ b\n@@ -0,0 +1,1 @@\n+hi\n"},
            "always": ["*"], "tool": {"messageID": "msg_1", "callID": "call_w"}
        })));
        let perms = turn.take_permissions();
        assert_eq!(perms.len(), 2);
        assert_eq!(perms[0].kind, "bash");
        assert_eq!(perms[0].title, "rm -rf tmp");
        assert_eq!(perms[0].command(), Some("rm -rf tmp"));
        assert_eq!(perms[0].always, vec!["rm *"]);
        assert_eq!(
            perms[0].call_id.as_deref(),
            Some("call_167b87f253a64db1b318af12")
        );
        assert_eq!(perms[1].kind, "edit");
        assert_eq!(perms[1].title, "/w/hello.txt");
        assert_eq!(perms[1].file_path(), Some("/w/hello.txt"));
        assert!(perms[1].diff().is_some_and(|d| d.contains("+hi")));
        // A foreign session's ask is ignored.
        turn.on_event(&ev(
            "permission.asked",
            json!({"id": "x", "sessionID": "other", "permission": "bash", "patterns": []}),
        ));
        assert!(turn.take_permissions().is_empty());
    }

    /// The `task` tool's subagent runs in a child session (`session.created` with `parentID` =
    /// the turn's session, as 1.18.31 publishes it: `{ sessionID, info }`). Its permission and
    /// question asks are the turn's to answer, addressed to the child session; its text, tool
    /// parts and idle stay with the child and never end the parent's turn. A grandchild announced
    /// by `session.updated` counts too; a session with some other parent stays foreign.
    #[test]
    fn a_subagent_sessions_asks_are_the_turns() {
        const CHILD: &str = "ses_child";
        let mut turn = ServeTurn::new(SID);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        turn.on_event(&ev("message.part.updated", json!({"part": {"id": "p1", "sessionID": SID, "type": "tool",
            "tool": "task", "callID": "call_task", "state": {"status": "running", "input": {"subagent_type": "explore", "description": "Extract Wikimedia image candidates"}}}})));
        assert_eq!(turn.tools_running(), 1);
        turn.on_event(&ev("session.created", json!({"sessionID": CHILD, "info": {"id": CHILD, "parentID": SID,
            "title": "Extract Wikimedia image candidates (@explore subagent)", "agent": "explore"}})));
        assert!(turn.child_sessions().contains(CHILD));

        // The child's own activity is not the parent's answer, not its tool rows, not its end.
        let out = turn.on_event(&ev("message.part.updated", json!({"part": {"id": "p2", "sessionID": CHILD, "type": "tool",
            "tool": "read", "callID": "call_read", "state": {"status": "completed", "input": {"filePath": "/w/x"}, "output": "…"}}})));
        assert!(out.is_empty());
        assert_eq!(
            turn.tools_running(),
            1,
            "a child's tool parts are not the parent's"
        );
        assert!(turn.on_event(&ev("message.part.delta", json!({"sessionID": CHILD, "messageID": "m", "partID": "p3", "field": "text", "delta": "child text"}))).is_empty());
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": CHILD, "status": {"type": "idle"}}),
        ));
        turn.on_event(&ev("session.idle", json!({"sessionID": CHILD})));
        assert!(
            turn.terminal().is_none(),
            "a child going idle does not end the parent's turn"
        );

        // Its asks are ours, addressed to the child session.
        turn.on_event(&ev("permission.asked", json!({
            "id": "per_child", "sessionID": CHILD, "permission": "bash", "patterns": ["python3 extract.py"],
            "metadata": {"command": "python3 extract.py"}, "always": ["python3 *"],
            "tool": {"messageID": "msg_c", "callID": "call_py"}
        })));
        let perms = turn.take_permissions();
        assert_eq!(perms.len(), 1);
        assert_eq!(perms[0].id, "per_child");
        assert_eq!(perms[0].session_id, CHILD);
        assert_eq!(perms[0].command(), Some("python3 extract.py"));
        assert_eq!(perms[0].call_id.as_deref(), Some("call_py"));
        turn.on_event(&ev("question.asked", json!({"id": "que_child", "sessionID": CHILD, "tool": {"messageID": "msg_c", "callID": "call_q"},
            "questions": [{"question": "Which species first?", "header": "Order", "options": [{"label": "Lion"}, {"label": "Tiger"}]}]})));
        let questions = turn.take_questions();
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].session_id, CHILD);
        assert_eq!(questions[0].questions[0].question, "Which species first?");

        // A grandchild announced by `session.updated` (a resumed `task_id` never fires `created`).
        turn.on_event(&ev("session.updated", json!({"sessionID": "ses_grandchild", "info": {"id": "ses_grandchild", "parentID": CHILD}})));
        turn.on_event(&ev(
            "permission.asked",
            json!({"id": "per_gc", "sessionID": "ses_grandchild", "permission": "bash",
            "patterns": ["ls"], "metadata": {"command": "ls"}, "always": []}),
        ));
        assert_eq!(turn.take_permissions()[0].session_id, "ses_grandchild");

        // Some other session's child is not ours.
        turn.on_event(&ev(
            "session.created",
            json!({"sessionID": "ses_x", "info": {"id": "ses_x", "parentID": "ses_elsewhere"}}),
        ));
        turn.on_event(&ev(
            "permission.asked",
            json!({"id": "per_x", "sessionID": "ses_x", "permission": "bash", "patterns": []}),
        ));
        assert!(turn.take_permissions().is_empty());

        // The parent finishes as before.
        turn.on_event(&ev("message.part.updated", json!({"part": {"id": "p1", "sessionID": SID, "type": "tool",
            "tool": "task", "callID": "call_task", "state": {"status": "completed", "input": {}, "output": "15 candidates", "title": "Extract Wikimedia image candidates"}}})));
        assert_eq!(turn.tools_running(), 0);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "idle"}}),
        ));
        assert_eq!(turn.terminal(), Some(&Terminal::Completed));
    }

    /// 1.18.31 `question.asked` (the `question` tool): the questions with their options and the
    /// tool call they belong to; another session's question is not ours.
    #[test]
    fn question_asked_is_collected() {
        let mut turn = ServeTurn::new(SID);
        let asked = json!({"id": "que_1", "sessionID": SID, "tool": {"messageID": "m1", "callID": "call_9"},
            "questions": [{"question": "How should Ghostty be installed?", "header": "Install method",
                "options": [{"label": "PPA (Recommended)", "description": "Community apt repository"},
                            {"label": ".deb", "description": "One package file"}]}]});
        assert!(
            turn.on_event(&ev("question.asked", asked.clone()))
                .is_empty()
        );
        let mut other = asked.clone();
        other["sessionID"] = json!("ses_other");
        turn.on_event(&ev("question.asked", other));
        let questions = turn.take_questions();
        assert_eq!(questions.len(), 1);
        let q = &questions[0];
        assert_eq!(
            (q.id.as_str(), q.call_id.as_deref()),
            ("que_1", Some("call_9"))
        );
        assert_eq!(q.questions[0].header, "Install method");
        assert_eq!(q.questions[0].options[0].label, "PPA (Recommended)");
        assert!(!q.questions[0].multiple);
        assert!(turn.take_questions().is_empty());
    }

    /// Live ordering on 1.18.31: the reasoning part is announced (`message.part.updated`, type
    /// `reasoning`, no end) before its deltas, and those deltas say `field: "text"` exactly like
    /// answer deltas do. They must come out as `Thinking`, never as answer text.
    #[test]
    fn reasoning_deltas_are_thinking_not_answer_text() {
        let mut turn = ServeTurn::new(SID);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        turn.on_event(&ev(
            "message.part.updated",
            json!({"part": {"id": "pr", "sessionID": SID, "type": "reasoning", "text": "", "time": {"start": 1}}}),
        ));
        let thought = turn.on_event(&ev(
            "message.part.delta",
            json!({"sessionID": SID, "messageID": "m", "partID": "pr", "field": "text", "delta": "The user is just greeting me. Keep it short."}),
        ));
        assert_eq!(
            thought,
            vec![AdapterEvent::Thinking {
                text: "The user is just greeting me. Keep it short.".into()
            }]
        );
        // The finished reasoning part was streamed, so it is not repeated.
        let done = turn.on_event(&ev(
            "message.part.updated",
            json!({"part": {"id": "pr", "sessionID": SID, "type": "reasoning", "text": "The user is just greeting me. Keep it short.", "time": {"start": 1, "end": 2}}}),
        ));
        assert!(done.is_empty(), "{done:?}");
        turn.on_event(&ev(
            "message.part.updated",
            json!({"part": {"id": "pt", "sessionID": SID, "type": "text", "text": "", "time": {"start": 3}}}),
        ));
        let answer = turn.on_event(&ev(
            "message.part.delta",
            json!({"sessionID": SID, "messageID": "m", "partID": "pt", "field": "text", "delta": "I'm Workshop's assistant."}),
        ));
        assert_eq!(
            answer,
            vec![AdapterEvent::TextDelta {
                text: "I'm Workshop's assistant.".into()
            }]
        );
    }

    /// A finished `bash` part carries its exit code and combined output in `metadata`; the
    /// detail event precedes the result so a host can render `exit 1` next to the output.
    #[test]
    fn tool_detail_carries_bash_exit_and_output() {
        let mut turn = ServeTurn::new(SID);
        turn.on_event(&ev(
            "session.status",
            json!({"sessionID": SID, "status": {"type": "busy"}}),
        ));
        let out = turn.on_event(&ev(
            "message.part.updated",
            json!({"part": {"id": "p1", "sessionID": SID, "type": "tool", "tool": "bash", "callID": "c1", "state": {"status": "completed", "input": {"command": "ls /nope"}, "output": "ls: cannot access '/nope'", "metadata": {"output": "ls: cannot access '/nope'", "exit": 2, "truncated": false}, "title": "ls /nope", "time": {"start": 1, "end": 2}}}}),
        ));
        assert_eq!(
            out,
            vec![
                AdapterEvent::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    input: json!({"command": "ls /nope"})
                },
                AdapterEvent::ToolDetail {
                    id: "c1".into(),
                    title: Some("ls /nope".into()),
                    metadata: json!({"output": "ls: cannot access '/nope'", "exit": 2, "truncated": false}),
                },
                AdapterEvent::ToolResult {
                    id: "c1".into(),
                    output: "ls: cannot access '/nope'".into(),
                    is_error: false
                },
            ]
        );
    }
}
