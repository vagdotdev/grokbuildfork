//! Claude Code (`claude`).
//!
//! Verified against `@anthropic-ai/claude-code` 2.1.278 (`claude --help`,
//! `claude auth status --help`) and the `SDKMessage` types shipped in
//! `@anthropic-ai/claude-agent-sdk` 0.3.278 (`sdk.d.ts`).
//!
//! * identity: `claude --version` -> `2.1.278 (Claude Code)`
//! * status:   `claude auth status --json` -> `{"loggedIn": bool, "authMethod": ...}`
//!   (exit 1 when logged out). Workshop never reads `~/.claude`.
//! * login:    `claude auth login` in the user's terminal.
//! * run:      `claude -p --output-format stream-json --verbose
//!   --include-partial-messages --permission-prompts none --permission-mode <mode>
//!   [--model M] [--resume ID]`, prompt on stdin.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::json;
use crate::adapter::{
    Adapter, AdapterId, LoginState, NormalizeError, Normalizer, PermissionPolicy, ProbeOutput,
    PromptDelivery, RunRequest, Terminal, VersionPin,
};
use crate::event::{AdapterEvent, Usage};

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
            max_tested: "2.1.278",
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
        PromptDelivery::Stdin
    }

    fn run_args(&self, req: &RunRequest) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts",
            "none",
            "--permission-mode",
            match req.permission {
                PermissionPolicy::ReadOnly => "plan",
                PermissionPolicy::WorkspaceWrite => "acceptEdits",
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
                            Some("tool_use") => events.push(AdapterEvent::ToolCall {
                                id: json::str(block, "id").unwrap_or_default().to_string(),
                                name: json::str(block, "name").unwrap_or_default().to_string(),
                                input: block.get("input").cloned().unwrap_or(Value::Null),
                            }),
                            _ => {}
                        }
                    }
                }
                self.partial_text_seen = false;
                self.partial_thinking_seen = false;
            }
            Some("user") => {
                let content = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array);
                for block in content.into_iter().flatten() {
                    if json::str(block, "type") == Some("tool_result") {
                        events.push(AdapterEvent::ToolResult {
                            id: json::str(block, "tool_use_id")
                                .unwrap_or_default()
                                .to_string(),
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
                "--permission-prompts",
                "none",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "opus",
                "--resume",
                "sess-1"
            ]
        );
    }
}
