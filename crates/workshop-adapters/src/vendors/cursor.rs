//! Cursor Agent CLI (`cursor-agent`, also installed as `agent`).
//!
//! Verified against Cursor Agent 2026.09.18-9a7762b (`cursor-agent --help`,
//! `cursor-agent status --help`, and the stream-json emitters in the
//! installed bundle). The installer symlinks both `~/.local/bin/cursor-agent`
//! and `~/.local/bin/agent` to the same binary, so `agent` is a legitimate
//! name — but only after the `--help` banner proves it is the Cursor Agent.
//! `--version` alone prints a bare `YYYY.MM.DD-hash` and is not an identity.
//!
//! * identity: `--version` -> `2026.09.18-9a7762b`; `--help` contains
//!   `Start the Cursor Agent`.
//! * status:   `cursor-agent status --format json` ->
//!   `{"status":"authenticated"|"partially-authenticated"|"unauthenticated","isAuthenticated":bool,...}`
//!   (exit 0 either way). Workshop never reads `~/.cursor/sdk/auth.json`.
//! * login:    `cursor-agent login` in the user's terminal.
//! * run:      `cursor-agent -p --output-format stream-json --stream-partial-output
//!   --trust [--mode plan] [--model M] [--resume=ID] <prompt>`; prompt is the
//!   final positional argument (stdin prompt delivery is not verified for
//!   this CLI), stdin is `/dev/null`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude::truncate;
use super::json;
use crate::adapter::{
    Adapter, AdapterId, LoginState, NormalizeError, Normalizer, PermissionPolicy, ProbeOutput,
    PromptDelivery, RunRequest, Terminal, VersionPin,
};
use crate::event::{AdapterEvent, Usage};

pub struct CursorAdapter;

impl Adapter for CursorAdapter {
    fn id(&self) -> AdapterId {
        AdapterId::Cursor
    }

    fn binary_names(&self) -> &'static [&'static str] {
        &["cursor-agent", "agent"]
    }

    fn extra_install_dirs(&self, home: &Path) -> Vec<PathBuf> {
        let _ = home;
        vec![PathBuf::from(
            "/Applications/Cursor.app/Contents/Resources/app/bin",
        )]
    }

    fn identity_probes(&self) -> &'static [&'static [&'static str]] {
        &[&["--version"], &["--help"]]
    }

    fn identify(&self, outputs: &[ProbeOutput]) -> Option<String> {
        let [version_out, help_out] = outputs else {
            return None;
        };
        if !help_out.stdout.contains("Start the Cursor Agent") {
            return None;
        }
        let version = version_out.stdout.lines().next()?.trim();
        let mut parts = version.splitn(3, '.');
        let looks_dated = parts
            .next()
            .is_some_and(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()))
            && parts
                .next()
                .is_some_and(|m| m.len() == 2 && m.chars().all(|c| c.is_ascii_digit()))
            && parts
                .next()
                .is_some_and(|d| d.len() >= 2 && d.starts_with(|c: char| c.is_ascii_digit()));
        looks_dated.then(|| version.to_string())
    }

    fn version_pin(&self) -> VersionPin {
        VersionPin {
            min_supported: "2026.09.18",
            max_tested: "2026.09.18",
        }
    }

    fn status_args(&self) -> &'static [&'static str] {
        &["status", "--format", "json"]
    }

    fn interpret_status(&self, output: &ProbeOutput) -> LoginState {
        let Ok(v) = serde_json::from_str::<Value>(output.stdout.trim()) else {
            return LoginState::Unknown {
                reason: format!(
                    "`cursor-agent status --format json` did not print JSON (exit {:?})",
                    output.exit_code
                ),
            };
        };
        match json::bool_of(&v, "isAuthenticated") {
            Some(true) => LoginState::Ready {
                method: Some("Cursor account".to_string()),
            },
            Some(false) => LoginState::SignIn,
            None => LoginState::Unknown {
                reason: "`isAuthenticated` missing from status JSON".to_string(),
            },
        }
    }

    fn login_args(&self) -> &'static [&'static str] {
        &["login"]
    }

    fn logout_args(&self) -> &'static [&'static str] {
        &["logout"]
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
        if req.permission == PermissionPolicy::ReadOnly {
            args.push("--mode".into());
            args.push("plan".into());
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
                        events.push(AdapterEvent::TextDelta {
                            text: text.to_string(),
                        });
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
            Some("tool_call") => {
                let id = json::str(&v, "call_id").unwrap_or_default().to_string();
                // `tool_call` is a protobuf oneof rendered as a single-key
                // object, e.g. {"readToolCall": {"args": {...}, "result": {...}}}.
                let (name, body) = v
                    .get("tool_call")
                    .and_then(Value::as_object)
                    .and_then(|o| o.iter().next())
                    .map(|(k, b)| (k.clone(), b.clone()))
                    .unwrap_or_else(|| ("unknown".to_string(), Value::Null));
                match json::str(&v, "subtype") {
                    Some("started") => events.push(AdapterEvent::ToolCall {
                        id,
                        name,
                        input: body.get("args").cloned().unwrap_or(Value::Null),
                    }),
                    Some("completed") => {
                        let result = body.get("result").cloned().unwrap_or(Value::Null);
                        let is_error = result
                            .as_object()
                            .is_some_and(|r| !r.is_empty() && !r.contains_key("success"));
                        events.push(AdapterEvent::ToolResult {
                            id,
                            output: result.to_string(),
                            is_error,
                        });
                    }
                    _ => {}
                }
            }
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
            // system/init, user echo, interaction_query, ...
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

#[cfg(test)]
mod tests {
    use super::*;

    fn out(stdout: &str) -> ProbeOutput {
        ProbeOutput {
            stdout: stdout.into(),
            exit_code: Some(0),
            ..Default::default()
        }
    }

    #[test]
    fn a_bare_agent_binary_is_not_cursor() {
        let a = CursorAdapter;
        assert_eq!(
            a.identify(&[
                out("2026.09.18-9a7762b\n"),
                out("Usage: agent [options] [command] [prompt...]\n\nStart the Cursor Agent\n")
            ]),
            Some("2026.09.18-9a7762b".to_string())
        );
        assert_eq!(
            a.identify(&[out("1.2.3\n"), out("Usage: agent\nSome other agent\n")]),
            None
        );
        assert_eq!(a.identify(&[out("2026.09.18-9a7762b\n")]), None);
    }

    #[test]
    fn status_json_maps_to_login_state() {
        let a = CursorAdapter;
        assert_eq!(
            a.interpret_status(&out(
                r#"{"status":"unauthenticated","isAuthenticated":false,"hasAccessToken":false,"hasRefreshToken":false,"message":"Not logged in"}"#
            )),
            LoginState::SignIn
        );
        assert_eq!(
            a.interpret_status(&out(
                r#"{"status":"authenticated","isAuthenticated":true,"hasAccessToken":true,"hasRefreshToken":true}"#
            )),
            LoginState::Ready {
                method: Some("Cursor account".into())
            }
        );
        assert_eq!(
            a.interpret_status(&out(
                r#"{"status":"partially-authenticated","isAuthenticated":false,"hasAccessToken":true,"hasRefreshToken":false}"#
            )),
            LoginState::SignIn
        );
        assert!(matches!(
            a.interpret_status(&out("Not logged in\n")),
            LoginState::Unknown { .. }
        ));
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
}
