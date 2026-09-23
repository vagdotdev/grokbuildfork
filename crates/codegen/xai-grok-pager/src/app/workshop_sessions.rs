//! Workshop overlay: engine conversations on disk, so they can be resumed.
//!
//! An Engine (OpenCode) turn never enters the shell's session store — the shell session behind
//! the agent view stays an empty husk — so Workshop keeps its own record per engine session under
//! `$WORKSHOP_HOME/engine/sessions/<id>.json`: the prompts, the model's answers and reasoning,
//! and every tool call with its result. The resume picker lists these, `workshop --resume <id>`
//! / `workshop -c` replay one into a fresh agent and continue the same OpenCode session, and
//! the quit hint names the id that actually resumes the conversation.
//!
//! Only sessions with at least one turn exist here: nothing is written at launch, so an idle
//! start leaves no session behind.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app::agent_view::AgentView;
use crate::app::app_view::{AppView, SessionPickerEntry};
use crate::scrollback::block::RenderBlock;

/// One engine conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSession {
    /// The OpenCode session id (`ses_…`), also the resume handle.
    pub id: String,
    pub cwd: String,
    pub model_ref: String,
    pub model_name: String,
    pub created_unix: u64,
    pub updated_unix: u64,
    /// The engine's last reported context usage, so a resumed session meters from where it was.
    #[serde(default)]
    pub context_used: Option<u64>,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub prompt: String,
    pub at_unix: u64,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// What the model produced during a turn, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Thinking {
        text: String,
    },
    Text {
        text: String,
    },
    Tool {
        name: String,
        input: Value,
        ok: bool,
        output: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        metadata: Value,
    },
}

impl EngineSession {
    /// Picker/quit title: the first prompt, one line, capped.
    pub fn title(&self) -> String {
        let first = self
            .turns
            .first()
            .map(|t| t.prompt.as_str())
            .unwrap_or_default();
        let line: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out: String = line.chars().take(72).collect();
        if line.chars().count() > 72 {
            out.push('…');
        }
        out
    }

    pub fn message_count(&self) -> usize {
        self.turns
            .iter()
            .map(|t| {
                1 + t
                    .items
                    .iter()
                    .filter(|i| matches!(i, Item::Text { .. }))
                    .count()
            })
            .sum()
    }
}

fn now_unix() -> u64 {
    crate::app::workshop_engine_state::now_unix()
}

/// Append answer text to the turn's record, continuing the current answer paragraph.
pub fn record_text(items: &mut Vec<Item>, delta: &str) {
    match items.last_mut() {
        Some(Item::Text { text }) => text.push_str(delta),
        _ => items.push(Item::Text {
            text: delta.to_owned(),
        }),
    }
}

/// Append reasoning to the turn's record, continuing the current thinking paragraph.
pub fn record_thinking(items: &mut Vec<Item>, delta: &str) {
    match items.last_mut() {
        Some(Item::Thinking { text }) => text.push_str(delta),
        _ => items.push(Item::Thinking {
            text: delta.to_owned(),
        }),
    }
}

/// `$WORKSHOP_HOME/engine/sessions`.
pub fn sessions_dir() -> PathBuf {
    crate::app::workshop::workshop_home()
        .join("engine")
        .join("sessions")
}

fn session_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

/// OpenCode session ids are `ses_` + base62; anything else is never looked up here.
fn plausible_id(id: &str) -> bool {
    id.starts_with("ses_")
        && id.len() < 80
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub fn load(id: &str) -> Option<EngineSession> {
    load_in(&sessions_dir(), id)
}

fn load_in(root: &Path, id: &str) -> Option<EngineSession> {
    if !plausible_id(id) {
        return None;
    }
    let raw = std::fs::read_to_string(session_path(root, id)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(session: &EngineSession) {
    save_in(&sessions_dir(), session);
}

fn save_in(root: &Path, session: &EngineSession) {
    let path = session_path(root, &session.id);
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(error = %e, path = %parent.display(), "engine sessions dir");
        return;
    }
    match serde_json::to_vec_pretty(session) {
        Ok(json) => {
            if let Err(e) = workshop_providers::atomic_write_private(&path, &json) {
                tracing::warn!(error = %e, path = %path.display(), "engine session write");
            }
        }
        Err(e) => tracing::warn!(error = %e, "engine session serialize"),
    }
}

/// Every recorded engine session for `cwd`, newest first. Sessions without a turn never exist.
pub fn list_for_cwd(cwd: &Path) -> Vec<EngineSession> {
    list_for_cwd_in(&sessions_dir(), cwd)
}

fn list_for_cwd_in(root: &Path, cwd: &Path) -> Vec<EngineSession> {
    let cwd = cwd.to_string_lossy();
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<EngineSession> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|raw| serde_json::from_str::<EngineSession>(&raw).ok())
        .filter(|s| s.cwd == cwd && !s.turns.is_empty())
        .collect();
    out.sort_by(|a, b| b.updated_unix.cmp(&a.updated_unix).then(b.id.cmp(&a.id)));
    out
}

pub fn most_recent_for_cwd(cwd: &Path) -> Option<EngineSession> {
    list_for_cwd(cwd).into_iter().next()
}

/// Append one finished turn to the session's record (created on its first turn).
pub fn record_turn(
    id: &str,
    cwd: &Path,
    model_ref: &str,
    model_name: &str,
    prompt: &str,
    items: Vec<Item>,
    context_used: Option<u64>,
) {
    record_turn_in(
        &sessions_dir(),
        id,
        cwd,
        model_ref,
        model_name,
        prompt,
        items,
        context_used,
    );
}

#[allow(clippy::too_many_arguments)]
fn record_turn_in(
    root: &Path,
    id: &str,
    cwd: &Path,
    model_ref: &str,
    model_name: &str,
    prompt: &str,
    items: Vec<Item>,
    context_used: Option<u64>,
) {
    if !plausible_id(id) {
        return;
    }
    let now = now_unix();
    let mut session = load_in(root, id).unwrap_or_else(|| EngineSession {
        id: id.to_owned(),
        cwd: cwd.to_string_lossy().into_owned(),
        model_ref: model_ref.to_owned(),
        model_name: model_name.to_owned(),
        created_unix: now,
        updated_unix: now,
        context_used: None,
        turns: Vec::new(),
    });
    session.model_ref = model_ref.to_owned();
    session.model_name = model_name.to_owned();
    session.updated_unix = now;
    if context_used.is_some() {
        session.context_used = context_used;
    }
    session.turns.push(Turn {
        prompt: prompt.to_owned(),
        at_unix: now,
        items,
    });
    save_in(root, &session);
}

/// Resume-picker rows for the engine sessions of `cwd` (`local` source, like the shell's own).
pub fn picker_entries(cwd: &Path) -> Vec<SessionPickerEntry> {
    picker_entries_in(&sessions_dir(), cwd)
}

fn picker_entries_in(root: &Path, cwd: &Path) -> Vec<SessionPickerEntry> {
    list_for_cwd_in(root, cwd)
        .into_iter()
        .map(|s| {
            let at = |secs: u64| {
                chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
                    .unwrap_or_else(chrono::Utc::now)
            };
            SessionPickerEntry {
                id: s.id.clone(),
                summary: s.title(),
                updated_at: at(s.updated_unix),
                created_at: at(s.created_unix),
                cwd: s.cwd.clone(),
                hostname: None,
                source: "local".into(),
                model_id: Some(s.model_name.clone()),
                num_messages: s.message_count(),
                last_active_at: Some(at(s.updated_unix)),
                branch: None,
                repo_name: crate::views::session_picker::repo_name_from_cwd(&s.cwd),
                worktree_label: None,
                last_turn_summary: s.turns.last().and_then(|t| {
                    t.items.iter().rev().find_map(|i| match i {
                        Item::Text { text } => Some(first_line(text)),
                        _ => None,
                    })
                }),
                last_recap: None,
                session_kind: None,
                card_detail: None,
            }
        })
        .collect()
}

fn first_line(text: &str) -> String {
    let line: String = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    line
}

/// The engine session `workshop --resume <id>` / `workshop -c` should open, if any: an explicit
/// id that is one of ours, or — for `-c` under an active Engine connection — the most recent
/// engine conversation in this directory. `None` leaves the flags to the shell's own resume.
pub fn startup_resume(
    resume_id: Option<&str>,
    most_recent: bool,
    cwd: &Path,
) -> Option<EngineSession> {
    if let Some(id) = resume_id {
        return load(id).filter(|s| !s.turns.is_empty());
    }
    if most_recent && crate::app::workshop::load_active_connection().is_engine() {
        return most_recent_for_cwd(cwd);
    }
    None
}

/// Replay a recorded conversation into `agent`'s transcript: prompts, reasoning (collapsed),
/// tool rows with their bodies, answers.
pub fn replay(agent: &mut AgentView, session: &EngineSession) {
    for turn in &session.turns {
        let at = chrono::DateTime::<chrono::Utc>::from_timestamp(turn.at_unix as i64, 0)
            .map(|t| t.with_timezone(&chrono::Local));
        let stamp = |agent: &mut AgentView, id: crate::scrollback::EntryId| {
            if let Some(at) = at
                && let Some(entry) = agent.scrollback.get_by_id_mut(id)
            {
                entry.created_at = Some(at);
            }
        };
        agent.record_prompt_in_history(turn.prompt.trim());
        let id = agent
            .scrollback
            .push_block(RenderBlock::user_prompt(turn.prompt.as_str()));
        stamp(agent, id);
        for item in &turn.items {
            match item {
                Item::Thinking { text } => {
                    let id = agent
                        .scrollback
                        .push_block(RenderBlock::thinking(text.as_str()));
                    agent.scrollback.finish_running(id);
                    stamp(agent, id);
                }
                Item::Text { text } => {
                    let id = agent
                        .scrollback
                        .push_block(RenderBlock::agent_message(text.as_str()));
                    stamp(agent, id);
                }
                Item::Tool {
                    name,
                    input,
                    ok,
                    output,
                    title,
                    metadata,
                } => {
                    let id = agent
                        .scrollback
                        .push_block(crate::app::workshop_tools::running_row(name, input));
                    let block = crate::app::workshop_tools::finished_row(
                        name,
                        input,
                        *ok,
                        output,
                        title.as_deref(),
                        metadata,
                    );
                    agent.scrollback.replace_tool_block(id, block, None);
                    agent.scrollback.finish_running(id);
                    stamp(agent, id);
                }
            }
        }
    }
    agent.scrollback.push_block(RenderBlock::system(format!(
        "Resumed {} · {} — the conversation continues where it left off.",
        session.title(),
        session.model_name
    )));
    // Land at the end of the conversation, following new output like a fresh session.
    agent.scrollback.enable_follow_mode();
}

/// Bind a just-created agent to the engine conversation a launch or the picker asked to resume:
/// replay the transcript, continue the same OpenCode session, restore the context meter.
pub fn apply_pending_resume(app: &mut AppView, agent_id: crate::app::agent::AgentId) {
    let Some(session) = app.workshop_engine_resume.take() else {
        return;
    };
    let engine_model = match &app.workshop_connection {
        crate::app::workshop::WorkshopConnection::Engine { model } => Some(model.model_ref.clone()),
        _ => None,
    };
    // A resumed conversation continues on the model it used; the picker can still switch it.
    if engine_model.is_some_and(|m| m != session.model_ref) {
        let cached = crate::app::workshop::cached_engine_models();
        if let Some(model) = cached
            .into_iter()
            .find(|m| m.model_ref == session.model_ref)
        {
            let conn = crate::app::workshop::WorkshopConnection::Engine { model };
            crate::app::workshop::save_active_connection(&conn);
            app.workshop_connection = conn;
        }
    }
    app.workshop_engine_session = Some(session.id.clone());
    app.workshop_context_used = session.context_used;
    crate::app::workshop::sync_agent_views(app);
    if let Some(agent) = app.agents.get_mut(&agent_id) {
        replay(agent, &session);
    }
}

/// The id the quit hint should name, or `None` when nothing was said this session (no hint):
/// the engine conversation for an Engine connection, the shell session otherwise.
pub fn exit_resume_id(app: &AppView, agent: &AgentView, shell_session_id: &str) -> Option<String> {
    if app.workshop_connection.is_engine() {
        let id = app.workshop_engine_session.as_deref()?;
        return load(id).filter(|s| !s.turns.is_empty()).map(|s| s.id);
    }
    crate::views::session_title::last_user_prompt_line(agent).map(|_| shell_session_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(id: &str, cwd: &str, updated: u64) -> EngineSession {
        EngineSession {
            id: id.into(),
            cwd: cwd.into(),
            model_ref: "opencode/big-pickle".into(),
            model_name: "Big Pickle".into(),
            created_unix: updated - 10,
            updated_unix: updated,
            context_used: Some(8627),
            turns: vec![Turn {
                prompt: "Create a file named hello.txt\ncontaining hi".into(),
                at_unix: updated,
                items: vec![
                    Item::Thinking {
                        text: "Keep it short.".into(),
                    },
                    Item::Tool {
                        name: "write".into(),
                        input: json!({"filePath": "/w/hello.txt", "content": "hi"}),
                        ok: true,
                        output: "Wrote file successfully.".into(),
                        title: Some("hello.txt".into()),
                        metadata: json!({"exists": false}),
                    },
                    Item::Text {
                        text: "Created hello.txt.".into(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn round_trips_and_lists_newest_first_per_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("engine").join("sessions");
        let a = sample("ses_aaaa", "/w", 100);
        let b = sample("ses_bbbb", "/w", 200);
        let other = sample("ses_cccc", "/elsewhere", 300);
        for s in [&a, &b, &other] {
            save_in(&root, s);
        }
        assert_eq!(load_in(&root, "ses_aaaa"), Some(a.clone()));
        assert_eq!(load_in(&root, "not-an-id"), None);
        let listed: Vec<String> = list_for_cwd_in(&root, Path::new("/w"))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(listed, vec!["ses_bbbb", "ses_aaaa"]);
        assert_eq!(a.title(), "Create a file named hello.txt containing hi");
        assert_eq!(a.message_count(), 2);
        let rows = picker_entries_in(&root, Path::new("/w"));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "ses_bbbb");
        assert_eq!(rows[0].source, "local");
        assert_eq!(rows[0].model_id.as_deref(), Some("Big Pickle"));
        assert_eq!(
            rows[0].last_turn_summary.as_deref(),
            Some("Created hello.txt.")
        );
        // Appending a turn keeps the record and bumps the usage; a session without a turn is
        // never created (no file for an idle launch).
        record_turn_in(
            &root,
            "ses_aaaa",
            Path::new("/w"),
            "opencode/big-pickle",
            "Big Pickle",
            "and now?",
            vec![Item::Text {
                text: "Done.".into(),
            }],
            Some(9000),
        );
        let a2 = load_in(&root, "ses_aaaa").unwrap();
        assert_eq!(a2.turns.len(), 2);
        assert_eq!(a2.context_used, Some(9000));
        assert!(!root.join("ses_none.json").exists());
        assert!(
            !plausible_id("0199-shell-uuid"),
            "shell ids never hit the store"
        );
    }
}
