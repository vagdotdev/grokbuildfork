//! OpenCode (`opencode`).
//!
//! Verified against opencode 1.18.31 (`opencode --help`, `opencode run --help`,
//! `opencode auth --help`) and `packages/opencode/src/cli/cmd/run.ts` +
//! `packages/opencode/src/cli/cmd/providers.ts` + `packages/sdk/js/src/gen/types.gen.ts`
//! at tag `v1.18.31`.
//!
//! * identity, status, login: `workshop-detect` (the one detection stack, shared with the
//!   picker) verifies the binary, asks the official status command and runs the login.
//! * run:      `opencode run --format json --thinking [--agent plan] [--model P/M]
//!   [--session ID] <message>`; message is positional, stdin is `/dev/null`.
//!   JSON mode has no terminal event: exit 0 with no `error` event is success.

use serde_json::Value;

use super::claude::truncate;
use super::json;
use crate::adapter::{
    Adapter, AdapterId, NormalizeError, Normalizer, PermissionPolicy, PromptDelivery, RunRequest,
    Terminal, VersionPin,
};
use crate::event::{AdapterEvent, Usage};

pub struct OpenCodeAdapter;

impl Adapter for OpenCodeAdapter {
    fn id(&self) -> AdapterId {
        AdapterId::OpenCode
    }

    fn version_pin(&self) -> VersionPin {
        VersionPin {
            min_supported: "1.18.31",
            max_tested: "1.18.31",
        }
    }

    fn prompt_delivery(&self) -> PromptDelivery {
        PromptDelivery::Argument
    }

    fn run_args(&self, req: &RunRequest) -> Vec<String> {
        let mut args: Vec<String> = ["run", "--format", "json", "--thinking"]
            .into_iter()
            .map(String::from)
            .collect();
        if req.permission == PermissionPolicy::ReadOnly {
            args.push("--agent".into());
            args.push("plan".into());
        }
        if let Some(model) = &req.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        if let Some(id) = &req.resume {
            args.push("--session".into());
            args.push(id.clone());
        }
        args
    }

    fn normalizer(&self) -> Box<dyn Normalizer> {
        Box::new(OpenCodeNormalizer::default())
    }
}

/// Translates `opencode run --format json` lines:
/// `{"type": ..., "timestamp": ms, "sessionID": ..., "part": {...} | "error": {...}}`.
#[derive(Default)]
pub struct OpenCodeNormalizer {
    session_id: Option<String>,
    terminal: Option<Terminal>,
    errors: Vec<String>,
    last_text: Option<String>,
}

fn error_message(err: &Value) -> String {
    err.get("data")
        .and_then(|d| json::str(d, "message"))
        .or_else(|| json::str(err, "message"))
        .or_else(|| json::str(err, "name"))
        .map(str::to_string)
        .unwrap_or_else(|| err.to_string())
}

impl Normalizer for OpenCodeNormalizer {
    fn on_line(&mut self, line: &str) -> Result<Vec<AdapterEvent>, NormalizeError> {
        let v: Value =
            serde_json::from_str(line).map_err(|_| NormalizeError::NotJson(truncate(line, 200)))?;
        if let Some(sid) = json::str(&v, "sessionID")
            && self.session_id.is_none()
        {
            self.session_id = Some(sid.to_string());
        }
        let part = v.get("part").unwrap_or(&Value::Null);
        let mut events = Vec::new();
        match json::str(&v, "type") {
            Some("text") => {
                let text = json::str(part, "text").unwrap_or_default().to_string();
                self.last_text = Some(text.clone());
                events.push(AdapterEvent::TextDelta { text });
            }
            Some("reasoning") => events.push(AdapterEvent::Thinking {
                text: json::str(part, "text").unwrap_or_default().to_string(),
            }),
            Some("tool_use") => {
                let id = json::str(part, "callID").unwrap_or_default().to_string();
                let state = part.get("state").unwrap_or(&Value::Null);
                events.push(AdapterEvent::ToolCall {
                    id: id.clone(),
                    name: json::str(part, "tool").unwrap_or_default().to_string(),
                    input: state.get("input").cloned().unwrap_or(Value::Null),
                });
                let is_error = json::str(state, "status") == Some("error");
                events.push(AdapterEvent::ToolResult {
                    id,
                    output: if is_error {
                        json::str(state, "error").unwrap_or_default().to_string()
                    } else {
                        json::str(state, "output").unwrap_or_default().to_string()
                    },
                    is_error,
                });
            }
            Some("step_finish") => {
                let tokens = part.get("tokens").unwrap_or(&Value::Null);
                let cache = tokens.get("cache").unwrap_or(&Value::Null);
                events.push(AdapterEvent::Usage(Usage {
                    input_tokens: json::u64_of(tokens, "input"),
                    output_tokens: json::u64_of(tokens, "output"),
                    cache_read_tokens: json::u64_of(cache, "read"),
                    cache_write_tokens: json::u64_of(cache, "write"),
                    reasoning_tokens: json::u64_of(tokens, "reasoning"),
                    cost_usd: part.get("cost").and_then(Value::as_f64),
                }));
            }
            Some("error") => {
                let message = v
                    .get("error")
                    .map(error_message)
                    .unwrap_or_else(|| "error".into());
                self.errors.push(message.clone());
                events.push(AdapterEvent::Error { message });
            }
            // step_start and future event kinds
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
        if exit_code == Some(0) && self.errors.is_empty() {
            self.terminal = Some(Terminal::Completed);
            return vec![AdapterEvent::Done {
                session_id: self.session_id.clone(),
                result: self.last_text.clone(),
            }];
        }
        let message = if let Some(last) = self.errors.last() {
            last.clone()
        } else {
            format!(
                "opencode exited ({})",
                exit_code.map_or("signal".to_string(), |c| c.to_string())
            )
        };
        self.terminal = Some(Terminal::Failed(message.clone()));
        if self.errors.is_empty() {
            vec![AdapterEvent::Error { message }]
        } else {
            Vec::new()
        }
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
    fn pinned_run_flags() {
        let a = OpenCodeAdapter;
        let mut req = RunRequest::new("hi", "/tmp");
        req.model = Some("anthropic/claude-sonnet-4".into());
        req.resume = Some("ses_1".into());
        assert_eq!(
            a.run_args(&req),
            vec![
                "run",
                "--format",
                "json",
                "--thinking",
                "--agent",
                "plan",
                "--model",
                "anthropic/claude-sonnet-4",
                "--session",
                "ses_1"
            ]
        );
    }
}
