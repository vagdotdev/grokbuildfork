//! Cursor Agent `-p --output-format stream-json` stream.
//!
//! Documented shapes (cursor.com/docs/cli/headless): `{"type":"system","subtype":"init","model":…}`,
//! `{"type":"assistant","message":{"content":[{"type":"text","text":…}]}}`,
//! `{"type":"tool_call","subtype":"started"|"completed","tool_call":{"<name>ToolCall":{"args":…,"result":…}}}`,
//! `{"type":"result","duration_ms":…, …}`. The session id field name was not observable without a
//! signed-in account; the normalizer accepts `session_id`, `chat_id`, and `chatId` on any event and
//! reports resume as unavailable when none appears.

use serde_json::Value;
use workshop_detect::Vendor;

use super::{Normalizer, compact, str_field};
use crate::error::FailureReason;
use crate::event::{AdapterEvent, Usage};

#[derive(Debug, Default)]
pub struct CursorNormalizer {
    session_id: Option<String>,
    last_text: Option<String>,
    completed: bool,
}

impl CursorNormalizer {
    fn note_session(&mut self, v: &Value, out: &mut Vec<AdapterEvent>) {
        let id = ["session_id", "chat_id", "chatId"]
            .iter()
            .find_map(|k| str_field(v, k));
        if let Some(id) = id
            && self.session_id.as_deref() != Some(id)
        {
            self.session_id = Some(id.to_string());
            out.push(AdapterEvent::Session { id: id.to_string() });
        }
    }
}

impl Normalizer for CursorNormalizer {
    fn vendor(&self) -> Vendor {
        Vendor::Cursor
    }

    fn object(&mut self, v: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        self.note_session(v, &mut out);
        let kind = str_field(v, "type").unwrap_or("");
        match kind {
            "system" | "user" => {}
            "assistant" => {
                if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                    for block in blocks {
                        if str_field(block, "type") == Some("text")
                            && let Some(text) = str_field(block, "text")
                        {
                            self.last_text = Some(text.to_string());
                            out.push(AdapterEvent::Text { text: text.to_string() });
                        }
                    }
                }
            }
            "tool_call" => {
                let subtype = str_field(v, "subtype").unwrap_or("");
                let (name, body) = v
                    .get("tool_call")
                    .and_then(Value::as_object)
                    .and_then(|m| m.iter().next())
                    .map(|(k, b)| (k.trim_end_matches("ToolCall").to_string(), b))
                    .unwrap_or_else(|| ("tool".to_string(), v));
                let id = str_field(v, "call_id").or_else(|| str_field(v, "id")).map(str::to_string);
                match subtype {
                    "started" => out.push(AdapterEvent::ToolStarted {
                        id,
                        name,
                        detail: body.get("args").and_then(compact),
                    }),
                    "completed" => {
                        let ok = body.get("result").and_then(Value::as_object).map(|r| r.contains_key("success"));
                        out.push(AdapterEvent::ToolCompleted {
                            id,
                            name,
                            ok,
                            detail: body.get("result").and_then(compact),
                        });
                    }
                    other => out.push(AdapterEvent::Unknown { kind: format!("tool_call/{other}") }),
                }
            }
            "result" => {
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                    || str_field(v, "subtype").is_some_and(|s| s.starts_with("error"));
                let result_text = str_field(v, "result").map(str::to_string);
                out.push(AdapterEvent::Usage(Usage {
                    input_tokens: v.pointer("/usage/input_tokens").and_then(Value::as_u64),
                    output_tokens: v.pointer("/usage/output_tokens").and_then(Value::as_u64),
                    cost_usd: None,
                }));
                if is_error {
                    out.push(AdapterEvent::Failed {
                        reason: FailureReason::VendorError {
                            detail: result_text.unwrap_or_else(|| "result reported an error".into()),
                        },
                    });
                } else {
                    if let Some(t) = &result_text {
                        self.last_text = Some(t.clone());
                    }
                    self.completed = true;
                    out.push(AdapterEvent::Completed {
                        final_text: self.last_text.clone(),
                        session_id: self.session_id.clone(),
                    });
                }
            }
            "thinking" => {
                if let Some(text) = str_field(v, "text") {
                    out.push(AdapterEvent::Thinking { text: text.to_string() });
                }
            }
            other => out.push(AdapterEvent::Unknown { kind: other.to_string() }),
        }
        out
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn last_text(&self) -> Option<&str> {
        self.last_text.as_deref()
    }

    fn completed(&self) -> bool {
        self.completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::LineResult;

    #[test]
    fn documented_headless_shapes() {
        let mut n = CursorNormalizer::default();
        let lines = [
            r#"{"type":"system","subtype":"init","model":"Auto","session_id":"chat-1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working on it"}]}}"#,
            r#"{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"writeToolCall":{"args":{"path":"analysis.txt"}}}}"#,
            r#"{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"writeToolCall":{"args":{"path":"analysis.txt"},"result":{"success":{"linesCreated":3,"fileSize":40}}}}}"#,
            r#"{"type":"result","duration_ms":1200,"result":"Wrote analysis.txt","session_id":"chat-1"}"#,
        ];
        let mut all = Vec::new();
        for l in lines {
            let LineResult::Events(e) = n.line(l) else { panic!("{l}") };
            all.extend(e);
        }
        assert_eq!(all[0], AdapterEvent::Session { id: "chat-1".into() });
        assert_eq!(all[1], AdapterEvent::Text { text: "Working on it".into() });
        assert!(matches!(&all[2], AdapterEvent::ToolStarted { name, id: Some(id), .. } if name == "write" && id == "c1"));
        assert!(matches!(&all[3], AdapterEvent::ToolCompleted { name, ok: Some(true), .. } if name == "write"));
        assert!(matches!(all.last(), Some(AdapterEvent::Completed { final_text: Some(t), session_id: Some(s) }) if t == "Wrote analysis.txt" && s == "chat-1"));
        assert!(n.completed());
    }

    #[test]
    fn result_with_error_fails() {
        let mut n = CursorNormalizer::default();
        let LineResult::Events(e) = n.line(r#"{"type":"result","is_error":true,"result":"Authentication required"}"#) else { panic!() };
        assert!(matches!(e.last(), Some(AdapterEvent::Failed { reason: FailureReason::VendorError { detail } }) if detail == "Authentication required"));
        assert!(!n.completed());
    }
}
