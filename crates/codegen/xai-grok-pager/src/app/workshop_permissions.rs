//! Workshop overlay: the engine's permission asks in the pager's own approval prompt.
//!
//! `opencode serve` runs with an `ask` policy for edits and commands (see
//! `workshop_adapters::opencode_engine::ask_before_edit_and_bash`). Each ask reaches the UI thread
//! as `WorkshopTurnMsg::PermissionAsk`; in Normal and Plan mode it is turned into the same
//! ACP-shaped request the shell sends for its own tools and queued on the agent, so the user sees
//! the familiar prompt (title, the command or the diff, Yes / Yes-always / No, followup text) and
//! the answer flows back to the server as `once` / `always` / `reject`.

use agent_client_protocol as acp;
use tokio::sync::oneshot;
use workshop_adapters::opencode_engine::{PermissionReply, PermissionRequest};

use crate::app::agent_view::AgentView;
use crate::app::workshop::WorkshopTurnMsg;

const ALLOW_ONCE: &str = "allow-once";
const ALLOW_ALWAYS: &str = "allow-always";
const REJECT_ONCE: &str = "reject-once";
/// Most diff lines shown inside the prompt; the row's expanded body has the whole diff later.
const MAX_DIFF_LINES: usize = 24;

/// Map the user's pick in the approval prompt to the engine's reply vocabulary. Anything that is
/// not an explicit allow — a reject, a cancel (Esc/Ctrl-C), a dropped prompt — is `reject`.
pub fn reply_for_response(
    response: Option<&acp::RequestPermissionResponse>,
) -> (PermissionReply, Option<String>) {
    let Some(response) = response else {
        return (PermissionReply::Reject, None);
    };
    let followup = response
        .meta
        .as_ref()
        .and_then(|m| m.get("followup_message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    match &response.outcome {
        acp::RequestPermissionOutcome::Selected(sel) => {
            let id = sel.option_id.0.as_ref();
            if id == ALLOW_ONCE
                || id == xai_grok_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID
            {
                (PermissionReply::Once, None)
            } else if id == ALLOW_ALWAYS {
                (PermissionReply::Always, None)
            } else {
                (PermissionReply::Reject, followup)
            }
        }
        _ => (PermissionReply::Reject, None),
    }
}

/// The ACP request the pager's permission view renders for an engine ask: an Execute call with
/// the command for `bash`, an Edit call with the file for `edit`, a generic call otherwise.
pub fn acp_request(req: &PermissionRequest, session_id: &str) -> acp::RequestPermissionRequest {
    let mut fields = acp::ToolCallUpdateFields::default();
    let mut options = Vec::new();
    let always_label = |what: &str| {
        if req.always.is_empty() || req.always.iter().any(|p| p == "*") {
            format!("Yes, and don't ask again for {what} this session")
        } else {
            format!(
                "Yes, and don't ask again for `{}` this session",
                req.always.join("`, `")
            )
        }
    };
    match req.kind.as_str() {
        "bash" => {
            let command = req.command().unwrap_or(req.title.as_str()).to_owned();
            fields.kind = Some(acp::ToolKind::Execute);
            fields.title = Some(format!("Execute `{command}`"));
            fields.raw_input = Some(serde_json::json!({ "command": command }));
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes, proceed",
                acp::PermissionOptionKind::AllowOnce,
            ));
            options.push(acp::PermissionOption::new(
                ALLOW_ALWAYS,
                always_label("commands like this"),
                acp::PermissionOptionKind::AllowAlways,
            ));
        }
        "edit" => {
            let path = req.file_path().unwrap_or(req.title.as_str()).to_owned();
            fields.kind = Some(acp::ToolKind::Edit);
            fields.title = Some(format!("Edit {path}"));
            fields.raw_input = Some(serde_json::json!({ "file_path": path }));
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes",
                acp::PermissionOptionKind::AllowOnce,
            ));
            options.push(acp::PermissionOption::new(
                ALLOW_ALWAYS,
                "Yes, allow all edits during this session",
                acp::PermissionOptionKind::AllowAlways,
            ));
        }
        other => {
            fields.title = Some(if req.title.is_empty() {
                other.to_owned()
            } else {
                format!("{other}: {}", req.title)
            });
            options.push(acp::PermissionOption::new(
                ALLOW_ONCE,
                "Yes",
                acp::PermissionOptionKind::AllowOnce,
            ));
            if !req.always.is_empty() {
                options.push(acp::PermissionOption::new(
                    ALLOW_ALWAYS,
                    always_label("this"),
                    acp::PermissionOptionKind::AllowAlways,
                ));
            }
        }
    }
    options.push(acp::PermissionOption::new(
        REJECT_ONCE,
        "No, and tell Workshop what to do differently",
        acp::PermissionOptionKind::RejectOnce,
    ));
    let tool_call = acp::ToolCallUpdate::new(
        acp::ToolCallId::new(req.call_id.clone().unwrap_or_else(|| req.id.clone())),
        fields,
    );
    acp::RequestPermissionRequest::new(
        acp::SessionId::new(session_id.to_owned()),
        tool_call,
        options,
    )
}

/// The changed lines of an `edit` ask's diff, for the prompt body (capped).
pub fn diff_preview_lines(req: &PermissionRequest) -> Vec<String> {
    let Some(diff) = req.diff() else {
        return Vec::new();
    };
    let mut lines: Vec<String> = diff
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .filter(|l| !l.starts_with('\\'))
        .map(str::to_owned)
        .collect();
    if lines.len() > MAX_DIFF_LINES {
        let hidden = lines.len() - MAX_DIFF_LINES;
        lines.truncate(MAX_DIFF_LINES);
        lines.push(format!("… (+{hidden} more lines)"));
    }
    lines
}

/// Show the engine's ask in the agent's approval prompt and wire the pick back to `reply`. A
/// "No" with typed text also queues that text as the next turn (`WorkshopTurnMsg::FollowUp`).
pub fn enqueue_engine_permission(
    agent: &mut AgentView,
    req: PermissionRequest,
    reply: oneshot::Sender<PermissionReply>,
    ui_tx: Option<tokio::sync::mpsc::UnboundedSender<WorkshopTurnMsg>>,
) {
    let session_id = agent
        .session
        .session_id
        .as_ref()
        .map(|s| s.0.to_string())
        .unwrap_or_else(|| req.session_id.clone());
    let request = acp_request(&req, &session_id);
    let (response_tx, response_rx) = oneshot::channel();
    let args = xai_acp_lib::AcpArgs {
        request,
        response_tx,
    };
    crate::app::acp_handler::enqueue_permission_for_workshop(args, agent);
    let preview = diff_preview_lines(&req);
    if !preview.is_empty()
        && let Some(front) = agent.permission_queue.back_mut()
    {
        front.description = preview;
    }
    tokio::spawn(async move {
        let response = response_rx.await.ok().and_then(Result::ok);
        let (decision, followup) = reply_for_response(response.as_ref());
        let _ = reply.send(decision);
        if let (Some(text), Some(tx)) = (followup, ui_tx) {
            let _ = tx.send(WorkshopTurnMsg::FollowUp(text));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask(kind: &str, metadata: serde_json::Value, always: &[&str]) -> PermissionRequest {
        PermissionRequest {
            id: "per_1".into(),
            session_id: "ses_1".into(),
            kind: kind.into(),
            title: "rm -rf tmp".into(),
            patterns: vec!["rm -rf tmp".into()],
            call_id: Some("call_1".into()),
            metadata,
            always: always.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn bash_ask_renders_as_an_execute_prompt_with_the_command() {
        let req = ask("bash", json!({"command": "rm -rf tmp"}), &["rm *"]);
        let acp_req = acp_request(&req, "sid");
        assert_eq!(acp_req.tool_call.fields.kind, Some(acp::ToolKind::Execute));
        assert_eq!(
            acp_req.tool_call.fields.title.as_deref(),
            Some("Execute `rm -rf tmp`")
        );
        assert_eq!(
            acp_req.tool_call.fields.raw_input,
            Some(json!({"command": "rm -rf tmp"}))
        );
        let names: Vec<&str> = acp_req.options.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Yes, proceed",
                "Yes, and don't ask again for `rm *` this session",
                "No, and tell Workshop what to do differently",
            ]
        );
        let kinds: Vec<acp::PermissionOptionKind> =
            acp_req.options.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            vec![
                acp::PermissionOptionKind::AllowOnce,
                acp::PermissionOptionKind::AllowAlways,
                acp::PermissionOptionKind::RejectOnce,
            ]
        );
    }

    #[test]
    fn edit_ask_shows_the_file_and_previews_the_diff() {
        let req = ask(
            "edit",
            json!({"filepath": "/w/hello.txt", "diff": "Index: x\n--- a\n+++ b\n@@ -0,0 +1,1 @@\n+hi\n\\ No newline at end of file\n"}),
            &["*"],
        );
        let acp_req = acp_request(&req, "sid");
        assert_eq!(acp_req.tool_call.fields.kind, Some(acp::ToolKind::Edit));
        assert_eq!(
            acp_req.tool_call.fields.title.as_deref(),
            Some("Edit /w/hello.txt")
        );
        assert_eq!(
            acp_req.tool_call.fields.raw_input,
            Some(json!({"file_path": "/w/hello.txt"}))
        );
        assert_eq!(
            diff_preview_lines(&req),
            vec!["@@ -0,0 +1,1 @@".to_owned(), "+hi".to_owned()]
        );
        assert!(
            acp_req
                .options
                .iter()
                .any(|o| o.name == "Yes, allow all edits during this session")
        );
    }

    #[test]
    fn picks_map_to_engine_replies() {
        let selected = |id: &str| {
            acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Selected(
                acp::SelectedPermissionOutcome::new(acp::PermissionOptionId::new(id)),
            ))
        };
        assert_eq!(
            reply_for_response(Some(&selected(ALLOW_ONCE))).0,
            PermissionReply::Once
        );
        assert_eq!(
            reply_for_response(Some(&selected(
                xai_grok_workspace::permission::ENABLE_ALWAYS_APPROVE_OPTION_ID
            )))
            .0,
            PermissionReply::Once
        );
        assert_eq!(
            reply_for_response(Some(&selected(ALLOW_ALWAYS))).0,
            PermissionReply::Always
        );
        let mut rejected = selected(REJECT_ONCE);
        let mut meta = acp::Meta::new();
        meta.insert("followup_message".into(), json!(" use trash instead "));
        rejected.meta = Some(meta);
        assert_eq!(
            reply_for_response(Some(&rejected)),
            (
                PermissionReply::Reject,
                Some("use trash instead".to_owned())
            )
        );
        let cancelled =
            acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
        assert_eq!(
            reply_for_response(Some(&cancelled)).0,
            PermissionReply::Reject
        );
        assert_eq!(reply_for_response(None).0, PermissionReply::Reject);
    }
}
