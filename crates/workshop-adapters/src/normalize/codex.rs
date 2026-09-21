//! Codex `exec --json` JSONL stream.
//!
//! Documented event types: `thread.started`, `turn.started`, `turn.completed`, `turn.failed`,
//! `item.started` / `item.updated` / `item.completed`, `error`. Item types include
//! `agent_message`, `reasoning`, `command_execution`, `file_change`, `mcp_tool_call`, `web_search`,
//! `todo_list`.

use serde_json::Value;
use workshop_detect::Vendor;

use super::{Normalizer, compact, str_field, u64_field};
use crate::error::FailureReason;
use crate::event::{AdapterEvent, Usage};

#[derive(Debug, Default)]
pub struct CodexNormalizer {
    thread_id: Option<String>,
    last_text: Option<String>,
    completed: bool,
}

fn item_detail(item: &Value) -> Option<String> {
    item.get("command")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("changes"))
        .or_else(|| item.get("query"))
        .or_else(|| item.get("arguments"))
        .and_then(compact)
}

impl Normalizer for CodexNormalizer {
    fn vendor(&self) -> Vendor {
        Vendor::Codex
    }

    fn object(&mut self, v: &Value) -> Vec<AdapterEvent> {
        let mut out = Vec::new();
        let kind = str_field(v, "type").unwrap_or("");
        match kind {
            "thread.started" => {
                if let Some(id) = str_field(v, "thread_id") {
                    self.thread_id = Some(id.to_string());
                    out.push(AdapterEvent::Session { id: id.to_string() });
                }
            }
            "turn.started" => {}
            "item.started" | "item.updated" => {
                if let Some(item) = v.get("item") {
                    let item_type = str_field(item, "type").unwrap_or("item");
                    if item_type != "agent_message" && item_type != "reasoning" && kind == "item.started" {
                        out.push(AdapterEvent::ToolStarted {
                            id: str_field(item, "id").map(str::to_string),
                            name: item_type.to_string(),
                            detail: item_detail(item),
                        });
                    }
                }
            }
            "item.completed" => {
                if let Some(item) = v.get("item") {
                    match str_field(item, "type") {
                        Some("agent_message") => {
                            if let Some(text) = str_field(item, "text") {
                                self.last_text = Some(text.to_string());
                                out.push(AdapterEvent::Text { text: text.to_string() });
                            }
                        }
                        Some("reasoning") => {
                            if let Some(text) = str_field(item, "text") {
                                out.push(AdapterEvent::Thinking { text: text.to_string() });
                            }
                        }
                        Some(other) => {
                            let ok = match (item.get("exit_code").and_then(Value::as_i64), str_field(item, "status")) {
                                (Some(code), _) => Some(code == 0),
                                (None, Some("completed")) => Some(true),
                                (None, Some("failed")) | (None, Some("error")) => Some(false),
                                _ => None,
                            };
                            out.push(AdapterEvent::ToolCompleted {
                                id: str_field(item, "id").map(str::to_string),
                                name: other.to_string(),
                                ok,
                                detail: item_detail(item),
                            });
                        }
                        None => out.push(AdapterEvent::Unknown { kind: "item.completed/?".into() }),
                    }
                }
            }
            "turn.completed" => {
                let usage = v.get("usage");
                out.push(AdapterEvent::Usage(Usage {
                    input_tokens: usage.and_then(|u| u64_field(u, "input_tokens")),
                    output_tokens: usage.and_then(|u| u64_field(u, "output_tokens")),
                    cost_usd: None,
                }));
                self.completed = true;
                out.push(AdapterEvent::Completed {
                    final_text: self.last_text.clone(),
                    session_id: self.thread_id.clone(),
                });
            }
            "turn.failed" | "error" => {
                let detail = v
                    .pointer("/error/message")
                    .or_else(|| v.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or(kind)
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
        self.thread_id.as_deref()
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
    fn documented_sample_stream() {
        let mut n = CodexNormalizer::default();
        let lines = [
            r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","status":"completed","exit_code":0}}"#,
            r#"{"type":"item.completed","item":{"id":"item_3","type":"agent_message","text":"Repo contains docs, sdk, and examples directories."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122,"reasoning_output_tokens":0}}"#,
        ];
        let mut all = Vec::new();
        for l in lines {
            let LineResult::Events(e) = n.line(l) else { panic!("{l}") };
            all.extend(e);
        }
        assert_eq!(all[0], AdapterEvent::Session { id: "0199a213-81c0-7800-8aa1-bbab2a035a53".into() });
        assert!(matches!(&all[1], AdapterEvent::ToolStarted { name, detail: Some(d), .. } if name == "command_execution" && d == "bash -lc ls"));
        assert!(matches!(&all[2], AdapterEvent::ToolCompleted { ok: Some(true), .. }));
        assert!(matches!(&all[3], AdapterEvent::Text { text } if text.starts_with("Repo contains")));
        assert_eq!(all[4], AdapterEvent::Usage(Usage { input_tokens: Some(24763), output_tokens: Some(122), cost_usd: None }));
        assert!(matches!(&all[5], AdapterEvent::Completed { session_id: Some(_), final_text: Some(_) }));
        assert!(n.completed());
    }

    #[test]
    fn turn_failed_and_error_are_vendor_errors() {
        let mut n = CodexNormalizer::default();
        let LineResult::Events(e) = n.line(r#"{"type":"turn.failed","error":{"message":"401 Unauthorized"}}"#) else { panic!() };
        assert!(matches!(&e[0], AdapterEvent::Failed { reason: FailureReason::VendorError { detail } } if detail == "401 Unauthorized"));
        let LineResult::Events(e) = n.line(r#"{"type":"error","message":"boom"}"#) else { panic!() };
        assert!(matches!(&e[0], AdapterEvent::Failed { reason: FailureReason::VendorError { detail } } if detail == "boom"));
        assert!(!n.completed());
    }
}
