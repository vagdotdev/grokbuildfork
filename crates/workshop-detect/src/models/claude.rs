//! Claude Code: the stream-json `initialize` control request.
//!
//! The Agent SDK's `supportedModels()` / `accountInfo()` read this same response; its fields are
//! the SDK's documented `ModelInfo` / `AccountInfo` types
//! (<https://code.claude.com/docs/en/agent-sdk/typescript>). The wire format itself is not
//! documented, so it is pinned here against Claude Code 2.1.280 (2026-09-23), which answers signed
//! out and offline in under a second:
//!
//! ```text
//! → {"type":"control_request","request_id":"workshop-models","request":{"subtype":"initialize"}}
//! ← {"type":"control_response","response":{"subtype":"success","request_id":"workshop-models",
//!     "response":{"models":[{"value":"default","displayName":"Default (recommended)",…},…],
//!                 "account":{"email":…,"subscriptionType":…,"tokenSource":…},…}}}
//! ```
//!
//! `--strict-mcp-config` keeps the listing from starting the user's MCP servers (Traycer's
//! catalog query does the same with `skipMcpDiscovery`, `claude-adapter.ts`), and
//! `--no-session-persistence` keeps it out of the user's resumable sessions. Closing stdin ends
//! the CLI.

use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use super::{Account, Answer, ModelsError, SubscriptionModel};
use crate::locate::DetectConfig;
use crate::model::Vendor;
use crate::process::{Next, Session};

pub(crate) const ARGS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--no-session-persistence",
    "--strict-mcp-config",
];

const REQUEST_ID: &str = "workshop-models";

pub(crate) fn request() -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": REQUEST_ID,
        "request": { "subtype": "initialize" },
    })
    .to_string()
}

/// The `--model` aliases Claude Code documents
/// (<https://code.claude.com/docs/en/model-config>). Shown only when a CLI build does not answer
/// `initialize`, labelled as aliases: they are what the CLI accepts, not what the account has.
pub const DOCUMENTED_ALIASES: &[&str] = &[
    "default",
    "fable",
    "opus",
    "opus[1m]",
    "sonnet",
    "sonnet[1m]",
    "haiku",
    "opusplan",
];

fn documented_aliases() -> Answer {
    Answer {
        models: DOCUMENTED_ALIASES
            .iter()
            .map(|alias| SubscriptionModel {
                id: (*alias).to_owned(),
                label: format!("{alias} (alias)"),
                is_default: *alias == "default",
            })
            .collect(),
        account: None,
        documented_aliases: true,
    }
}

pub(super) fn fetch(
    bin: &Path,
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    cfg: &DetectConfig,
) -> Result<Answer, ModelsError> {
    let deadline = Instant::now() + cfg.models_timeout;
    let mut session = Session::start(
        Vendor::Claude,
        bin,
        ARGS,
        cwd,
        env,
        cfg.models_timeout,
        cfg.kill_grace(Vendor::Claude),
    )?;
    // A build that rejects these flags exits at once; its EOF below is the "unsupported" answer.
    let _ = session.send(&request());
    loop {
        match session.next_line(deadline) {
            Next::Line(line) => {
                if let Some(answer) = interpret_line(&line) {
                    return Ok(answer);
                }
            }
            Next::Eof => return Ok(documented_aliases()),
            Next::TimedOut => return Err(ModelsError::TimedOut),
        }
    }
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `Some` once `line` is the answer to our request: the account's models, or (the request was
/// refused, or answered without `models`) the documented aliases.
pub(crate) fn interpret_line(line: &str) -> Option<Answer> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v["type"] != "control_response" || v["response"]["request_id"] != REQUEST_ID {
        return None;
    }
    let response = &v["response"];
    let body = &response["response"];
    let Some(list) = body["models"]
        .as_array()
        .filter(|_| response["subtype"] == "success")
    else {
        return Some(documented_aliases());
    };
    let models = list
        .iter()
        .filter_map(|m| {
            let id = non_empty(&m["value"])?;
            Some(SubscriptionModel {
                label: non_empty(&m["displayName"]).unwrap_or_else(|| id.clone()),
                is_default: id == "default",
                id,
            })
        })
        .collect();
    let account = Account {
        email: non_empty(&body["account"]["email"]),
        plan: non_empty(&body["account"]["subscriptionType"]),
    };
    Some(Answer {
        models,
        account: (account.email.is_some() || account.plan.is_some()).then_some(account),
        documented_aliases: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code 2.1.280, signed out, `models` and `account` verbatim (other keys trimmed).
    const SIGNED_OUT: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"workshop-models","pending_permission_requests":[],"response":{"commands":[],"agents":[],"models":[{"value":"default","resolvedModel":"claude-opus-5-5[1m]","displayName":"Default (recommended)","description":"Use the default model (currently Opus 5.5 (1M context)) · $4/$20 per Mtok","supportsEffort":true},{"value":"opus[1m]","resolvedModel":"claude-opus-5-5[1m]","displayName":"Opus (1M context)","description":"Opus 5.5 with 1M context"},{"value":"claude-fable-5-1","resolvedModel":"claude-fable-5-1","displayName":"Fable","description":"Fable 5.1"},{"value":"sonnet","resolvedModel":"claude-sonnet-5","displayName":"Sonnet","description":"Sonnet 5"},{"value":"haiku","resolvedModel":"claude-haiku-4-5-20251001","displayName":"Haiku","description":"Haiku 4.5"}],"account":{"tokenSource":"none","apiProvider":"firstParty"},"pid":1234}}}"#;

    #[test]
    fn request_is_the_sdk_initialize_control_request() {
        let v: Value = serde_json::from_str(&request()).unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request"]["subtype"], "initialize");
        assert_eq!(v["request_id"], REQUEST_ID);
    }

    #[test]
    fn real_signed_out_answer_lists_five_models_and_no_account() {
        let a = interpret_line(SIGNED_OUT).expect("our response");
        assert!(!a.documented_aliases);
        let ids: Vec<&str> = a.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            ["default", "opus[1m]", "claude-fable-5-1", "sonnet", "haiku"]
        );
        assert_eq!(a.models[0].label, "Default (recommended)");
        assert!(a.models[0].is_default);
        assert!(a.models[1..].iter().all(|m| !m.is_default));
        assert_eq!(a.account, None, "tokenSource none carries no email or plan");
    }

    #[test]
    fn signed_in_account_fields_are_the_sdk_account_info() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"workshop-models","response":{"models":[{"value":"sonnet","displayName":"Sonnet"}],"account":{"email":"user@example.com","organization":"Org","subscriptionType":"max","tokenSource":"claude.ai"}}}}"#;
        let a = interpret_line(line).unwrap();
        assert_eq!(
            a.account,
            Some(Account {
                email: Some("user@example.com".into()),
                plan: Some("max".into()),
            })
        );
    }

    #[test]
    fn other_lines_are_skipped_and_refusals_mean_aliases() {
        assert!(interpret_line(r#"{"type":"system","subtype":"init"}"#).is_none());
        assert!(interpret_line("not json").is_none());
        assert!(
            interpret_line(
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"other"}}"#
            )
            .is_none()
        );
        let refused = interpret_line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"workshop-models","error":"Unknown request subtype"}}"#,
        )
        .unwrap();
        assert!(refused.documented_aliases);
        let ids: Vec<&str> = refused.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, DOCUMENTED_ALIASES);
        assert!(refused.models.iter().all(|m| m.label.ends_with(" (alias)")));
        assert_eq!(refused.models.iter().filter(|m| m.is_default).count(), 1);
    }
}
