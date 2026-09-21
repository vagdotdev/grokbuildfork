//! OpenCode `run --format json` stream.
//!
//! Observed shapes (OpenCode 1.18.31): every line is
//! `{"type":"<part-type>","timestamp":…,"sessionID":"ses_…","part":{…}}` with part types
//! `step_start`, `text` (`part.text`), `reasoning`, `tool` (`part.tool`, `part.state.status`),
//! `step_finish` (`part.reason`, `part.tokens`, `part.cost`), and `error`.

use serde_json::Value;
use workshop_detect::Vendor;

use super::{Normalizer, compact, str_field, u64_field};
use crate::error::FailureReason;
use crate::event::{AdapterEvent, Usage};

#[derive(Debug, Default)]
pub struct OpenCodeNormalizer {
    session_id: Option<String>,
    last_text: Option<String>,
    completed: bool,
}

impl OpenCodeNormalizer {
    fn note_session(&mut self, v: &Value, out: &mut Vec<AdapterEvent>) {
        let id = str_field(v, "sessionID").or_else(|| v.pointer("/part/sessionID").and_then(Value::as_str));
        if let Some(id) = id
            && self.session_id.as_deref() != Some(id)
        {
            self.session_id = Some(id.to_string());
            out.push(AdapterEvent::Session { id: id.to_string() });
        }
    }
}

impl Normalizer for OpenCodeNormalizer {
    fn vendor(&self) -> Vendor {
        Vendor::OpenCode
    }

    fn object(&mut self, v: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        self.note_session(v, &mut out);
        let kind = str_field(v, "type").unwrap_or("");
        let part = v.get("part").unwrap_or(&Value::Null);
        match kind {
            "step_start" => {}
            "text" => {
                if let Some(text) = str_field(part, "text") {
                    self.last_text = Some(text.to_string());
                    out.push(AdapterEvent::Text { text: text.to_string() });
                }
            }
            "reasoning" => {
                if let Some(text) = str_field(part, "text") {
                    out.push(AdapterEvent::Thinking { text: text.to_string() });
                }
            }
            "tool" | "tool_use" | "tool-invocation" => {
                let name = str_field(part, "tool").unwrap_or("tool").to_string();
                let id = str_field(part, "callID").or_else(|| str_field(part, "id")).map(str::to_string);
                let state = part.get("state").unwrap_or(&Value::Null);
                match str_field(state, "status") {
                    Some("pending") | Some("running") => out.push(AdapterEvent::ToolStarted {
                        id,
                        name,
                        detail: state.get("input").and_then(compact),
                    }),
                    Some("completed") => out.push(AdapterEvent::ToolCompleted {
                        id,
                        name,
                        ok: Some(true),
                        detail: state.get("output").or_else(|| state.get("title")).and_then(compact),
                    }),
                    Some("error") => out.push(AdapterEvent::ToolCompleted {
                        id,
                        name,
                        ok: Some(false),
                        detail: state.get("error").and_then(compact),
                    }),
                    _ => out.push(AdapterEvent::Unknown { kind: format!("{kind}/?") }),
                }
            }
            "step_finish" => {
                let tokens = part.get("tokens");
                out.push(AdapterEvent::Usage(Usage {
                    input_tokens: tokens.and_then(|t| u64_field(t, "input")),
                    output_tokens: tokens.and_then(|t| u64_field(t, "output")),
                    cost_usd: part.get("cost").and_then(Value::as_f64),
                }));
                // "tool-calls" means another step follows; anything else ends the turn.
                if str_field(part, "reason") != Some("tool-calls") {
                    self.completed = true;
                    out.push(AdapterEvent::Completed {
                        final_text: self.last_text.clone(),
                        session_id: self.session_id.clone(),
                    });
                }
            }
            "error" => {
                let detail = v
                    .pointer("/error/data/message")
                    .or_else(|| v.pointer("/error/message"))
                    .or_else(|| v.pointer("/part/error"))
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string();
                out.push(AdapterEvent::Failed {
                    reason: FailureReason::VendorError { detail },
                });
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
    fn real_run_output_shapes() {
        // Captured from OpenCode 1.18.31 `opencode run --format json "say hi"`.
        let mut n = OpenCodeNormalizer::default();
        let lines = [
            r#"{"type":"step_start","timestamp":1790020980662,"sessionID":"ses_f3a6","part":{"id":"prt_1","messageID":"msg_1","sessionID":"ses_f3a6","snapshot":"4b82","type":"step-start"}}"#,
            r#"{"type":"text","timestamp":1790020980702,"sessionID":"ses_f3a6","part":{"id":"prt_2","messageID":"msg_1","sessionID":"ses_f3a6","type":"text","text":"Hi!","time":{"start":1,"end":2}}}"#,
            r#"{"type":"step_finish","timestamp":1790020980714,"sessionID":"ses_f3a6","part":{"id":"prt_3","reason":"stop","snapshot":"4b82","messageID":"msg_1","sessionID":"ses_f3a6","type":"step-finish","tokens":{"total":7901,"input":6105,"output":4,"reasoning":0,"cache":{"write":0,"read":1792}},"cost":0}}"#,
        ];
        let mut all = Vec::new();
        for l in lines {
            let LineResult::Events(e) = n.line(l) else { panic!("{l}") };
            all.extend(e);
        }
        assert_eq!(all[0], AdapterEvent::Session { id: "ses_f3a6".into() });
        assert_eq!(all[1], AdapterEvent::Text { text: "Hi!".into() });
        assert_eq!(all[2], AdapterEvent::Usage(Usage { input_tokens: Some(6105), output_tokens: Some(4), cost_usd: Some(0.0) }));
        assert!(matches!(&all[3], AdapterEvent::Completed { final_text: Some(t), session_id: Some(s) } if t == "Hi!" && s == "ses_f3a6"));
        assert!(n.completed());
    }

    #[test]
    fn tool_calls_step_does_not_complete_the_run() {
        let mut n = OpenCodeNormalizer::default();
        let LineResult::Events(e) = n.line(r#"{"type":"tool","sessionID":"s","part":{"tool":"read","callID":"c1","state":{"status":"running","input":{"filePath":"a"}}}}"#) else { panic!() };
        assert!(matches!(&e[1], AdapterEvent::ToolStarted { name, .. } if name == "read"));
        let LineResult::Events(e) = n.line(r#"{"type":"step_finish","sessionID":"s","part":{"reason":"tool-calls","tokens":{"input":1,"output":1},"cost":0}}"#) else { panic!() };
        assert!(!e.iter().any(|e| matches!(e, AdapterEvent::Completed { .. })));
        assert!(!n.completed());
        let LineResult::Events(e) = n.line(r#"{"type":"error","sessionID":"s","error":{"name":"ProviderAuthError","data":{"message":"no key"}}}"#) else { panic!() };
        assert!(matches!(&e[0], AdapterEvent::Failed { reason: FailureReason::VendorError { detail } } if detail == "no key"));
    }
}
