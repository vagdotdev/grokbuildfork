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
//! * `permission.updated` / `permission.asked` — the agent wants approval.
//! * `session.error { error }` — turn-level failure (also emitted on abort).
//! * `session.status { status: { type } }` / `session.idle` — `idle` after
//!   `busy` ends the turn.

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
    pub title: String,
    pub patterns: Vec<String>,
    pub call_id: Option<String>,
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
    streamed_parts: HashSet<String>,
    announced_calls: HashSet<String>,
    saw_busy: bool,
    last_text: Option<String>,
    error: Option<String>,
    aborted: bool,
    terminal: Option<Terminal>,
    pending_permissions: Vec<PermissionRequest>,
}

impl ServeTurn {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            streamed_parts: HashSet::new(),
            announced_calls: HashSet::new(),
            saw_busy: false,
            last_text: None,
            error: None,
            aborted: false,
            terminal: None,
            pending_permissions: Vec::new(),
        }
    }

    pub fn terminal(&self) -> Option<&Terminal> {
        self.terminal.as_ref()
    }

    /// True once the server reported the turn as aborted.
    pub fn aborted(&self) -> bool {
        self.aborted
    }

    /// Permission requests seen since the last drain.
    pub fn take_permissions(&mut self) -> Vec<PermissionRequest> {
        std::mem::take(&mut self.pending_permissions)
    }

    /// Feed one SSE event (any session; foreign sessions are ignored).
    pub fn on_event(&mut self, event: &Value) -> Vec<AdapterEvent> {
        if session_of(event) != Some(self.session_id.as_str()) {
            return Vec::new();
        }
        let props = event.get("properties").unwrap_or(&Value::Null);
        let mut out = Vec::new();
        match json::str(event, "type") {
            Some("message.part.delta") => {
                if let Some(part_id) = json::str(props, "partID") {
                    self.streamed_parts.insert(part_id.to_string());
                }
                let delta = json::str(props, "delta").unwrap_or_default().to_string();
                if !delta.is_empty() {
                    match json::str(props, "field") {
                        Some("reasoning") => out.push(AdapterEvent::Thinking { text: delta }),
                        _ => out.push(AdapterEvent::TextDelta { text: delta }),
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
                        match status {
                            "running" => announce(self, &mut out),
                            "completed" => {
                                announce(self, &mut out);
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
            Some("permission.updated") | Some("permission.asked") => {
                let patterns = match props.get("pattern") {
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    _ => Vec::new(),
                };
                self.pending_permissions.push(PermissionRequest {
                    id: json::str(props, "id").unwrap_or_default().to_string(),
                    session_id: self.session_id.clone(),
                    kind: json::str(props, "type").unwrap_or_default().to_string(),
                    title: json::str(props, "title").unwrap_or_default().to_string(),
                    patterns,
                    call_id: json::str(props, "callID").map(str::to_string),
                });
            }
            _ => {}
        }
        out
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
}
