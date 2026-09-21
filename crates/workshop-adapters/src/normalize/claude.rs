//! Claude Code `-p --output-format stream-json --verbose` stream.
//!
//! Observed shapes (Claude Code 2.1.278):
//! * `{"type":"system","subtype":"init","session_id":"…","model":"…", …}`
//! * `{"type":"assistant","message":{"content":[{"type":"text","text":"…"},{"type":"tool_use","id":"…","name":"Read","input":{…}}]},"session_id":"…","error":"authentication_failed"?}`
//! * `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"…","content":…,"is_error":false}]}}`
//! * `{"type":"result","subtype":"success"|"error_during_execution"|…,"is_error":bool,"result":"…","session_id":"…","total_cost_usd":0.0,"usage":{"input_tokens":…,"output_tokens":…},"num_turns":1}`

use serde_json::Value;
use workshop_detect::Vendor;

use super::{Normalizer, compact, str_field, u64_field};
use crate::error::FailureReason;
use crate::event::{AdapterEvent, Usage};

#[derive(Debug, Default)]
pub struct ClaudeNormalizer {
    session_id: Option<String>,
    last_text: Option<String>,
    completed: bool,
}

impl ClaudeNormalizer {
    fn note_session(&mut self, v: &Value, out: &mut Vec<AdapterEvent>) {
        if let Some(id) = str_field(v, "session_id")
            && self.session_id.as_deref() != Some(id)
        {
            self.session_id = Some(id.to_string());
            out.push(AdapterEvent::Session { id: id.to_string() });
        }
    }
}

impl Normalizer for ClaudeNormalizer {
    fn vendor(&self) -> Vendor {
        Vendor::Claude
    }

    fn object(&mut self, v: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        let kind = str_field(v, "type").unwrap_or("");
        match kind {
            "system" => {
                self.note_session(v, &mut out);
            }
            "assistant" => {
                self.note_session(v, &mut out);
                if let Some(err) = str_field(v, "error") {
                    let text = v
                        .pointer("/message/content/0/text")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    out.push(AdapterEvent::Failed {
                        reason: FailureReason::VendorError {
                            detail: format!("{err}: {text}").trim_end_matches(": ").to_string(),
                        },
                    });
                    return out;
                }
                if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                    for block in blocks {
                        match str_field(block, "type") {
                            Some("text") => {
                                if let Some(text) = str_field(block, "text") {
                                    self.last_text = Some(text.to_string());
                                    out.push(AdapterEvent::Text { text: text.to_string() });
                                }
                            }
                            Some("thinking") => {
                                if let Some(text) = str_field(block, "thinking") {
                                    out.push(AdapterEvent::Thinking { text: text.to_string() });
                                }
                            }
                            Some("tool_use") => out.push(AdapterEvent::ToolStarted {
                                id: str_field(block, "id").map(str::to_string),
                                name: str_field(block, "name").unwrap_or("tool").to_string(),
                                detail: block.get("input").and_then(compact),
                            }),
                            _ => {}
                        }
                    }
                }
            }
            "user" => {
                if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                    for block in blocks {
                        if str_field(block, "type") == Some("tool_result") {
                            out.push(AdapterEvent::ToolCompleted {
                                id: str_field(block, "tool_use_id").map(str::to_string),
                                name: "tool".to_string(),
                                ok: block.get("is_error").and_then(Value::as_bool).map(|e| !e),
                                detail: block.get("content").and_then(compact),
                            });
                        }
                    }
                }
            }
            "result" => {
                self.note_session(v, &mut out);
                let usage = v.get("usage");
                out.push(AdapterEvent::Usage(Usage {
                    input_tokens: usage.and_then(|u| u64_field(u, "input_tokens")),
                    output_tokens: usage.and_then(|u| u64_field(u, "output_tokens")),
                    cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
                }));
                let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                let subtype = str_field(v, "subtype").unwrap_or("");
                let result_text = str_field(v, "result").map(str::to_string);
                if is_error || subtype.starts_with("error") {
                    out.push(AdapterEvent::Failed {
                        reason: FailureReason::VendorError {
                            detail: format!(
                                "{subtype}: {}",
                                result_text.clone().unwrap_or_default()
                            )
                            .trim_end_matches(": ")
                            .to_string(),
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
            "stream_event" | "rate_limit_event" | "tool_use_summary" | "prompt_suggestion" => {}
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
    fn happy_path_yields_session_text_usage_completed() {
        let mut n = ClaudeNormalizer::default();
        let init = r#"{"type":"system","subtype":"init","cwd":"/w","session_id":"s-1","tools":["Read"],"model":"claude"}"#;
        let msg = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]},"session_id":"s-1"}"#;
        let tool = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok","is_error":false}]},"session_id":"s-1"}"#;
        let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"Done","session_id":"s-1","total_cost_usd":0.01,"usage":{"input_tokens":10,"output_tokens":5},"num_turns":1}"#;
        let LineResult::Events(e) = n.line(init) else { panic!() };
        assert_eq!(e, vec![AdapterEvent::Session { id: "s-1".into() }]);
        let LineResult::Events(e) = n.line(msg) else { panic!() };
        assert_eq!(e[0], AdapterEvent::Text { text: "Hello".into() });
        assert!(matches!(&e[1], AdapterEvent::ToolStarted { name, .. } if name == "Read"));
        let LineResult::Events(e) = n.line(tool) else { panic!() };
        assert!(matches!(&e[0], AdapterEvent::ToolCompleted { ok: Some(true), .. }));
        let LineResult::Events(e) = n.line(result) else { panic!() };
        assert_eq!(
            e[0],
            AdapterEvent::Usage(Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_usd: Some(0.01)
            })
        );
        assert_eq!(
            e[1],
            AdapterEvent::Completed {
                final_text: Some("Done".into()),
                session_id: Some("s-1".into())
            }
        );
        assert!(n.completed());
        assert_eq!(n.session_id(), Some("s-1"));
    }

    #[test]
    fn logged_out_run_is_a_vendor_error_not_success() {
        // Real output from Claude Code 2.1.278 with no login.
        let mut n = ClaudeNormalizer::default();
        let msg = r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]},"session_id":"s-2","error":"authentication_failed","is_api_error_message":true}"#;
        let LineResult::Events(e) = n.line(msg) else { panic!() };
        assert!(matches!(&e[1], AdapterEvent::Failed { reason: FailureReason::VendorError { detail } } if detail.contains("authentication_failed")));
        let result = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","session_id":"s-2","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0},"num_turns":1}"#;
        let LineResult::Events(e) = n.line(result) else { panic!() };
        assert!(matches!(e.last(), Some(AdapterEvent::Failed { .. })));
        assert!(!n.completed());
    }

    #[test]
    fn unknown_type_is_surfaced() {
        let mut n = ClaudeNormalizer::default();
        let LineResult::Events(e) = n.line(r#"{"type":"something_new"}"#) else { panic!() };
        assert_eq!(e, vec![AdapterEvent::Unknown { kind: "something_new".into() }]);
    }
}
