//! Live proof against the real `opencode` binary and OpenCode's own free tier
//! (no key). Network + vendor dependent, so it only runs when
//! `WORKSHOP_LIVE_OPENCODE=1` is set; otherwise it skips with a note.
//!
//! Writes a JSONL transcript of every normalized event to the temp dir and
//! prints its path, so the run can be attached as evidence.

#![cfg(unix)]

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use workshop_adapters::opencode_engine::{
    EngineOptions, InstallOptions, InstallTarget, OpenCodeEngine, TurnHandle, TurnRequest,
    install_opencode,
};
use workshop_adapters::vendors::OpenCodeAdapter;
use workshop_adapters::{
    AdapterEvent, DetectOptions, Detection, PermissionPolicy, RunOutcome, detect,
};

fn gated() -> bool {
    if std::env::var_os("WORKSHOP_LIVE_OPENCODE").is_some() {
        return true;
    }
    eprintln!("[live] WORKSHOP_LIVE_OPENCODE not set; skipping live OpenCode engine test");
    false
}

fn git(cwd: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "git {args:?}");
}

struct Transcript {
    file: std::fs::File,
    path: std::path::PathBuf,
}

impl Transcript {
    fn new() -> Self {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let path = std::env::temp_dir().join(format!("workshop-opencode-live-{ts}.jsonl"));
        let file = std::fs::File::create(&path).unwrap();
        Self { file, path }
    }

    fn note(&mut self, kind: &str, detail: serde_json::Value) {
        let line = serde_json::json!({ "marker": kind, "detail": detail });
        writeln!(self.file, "{line}").unwrap();
    }

    fn event(&mut self, turn: &str, ev: &AdapterEvent) {
        let line = serde_json::json!({ "turn": turn, "event": ev });
        writeln!(self.file, "{line}").unwrap();
    }
}

async fn drain(
    turn: &mut TurnHandle,
    label: &str,
    transcript: &mut Transcript,
) -> Vec<AdapterEvent> {
    let mut events = Vec::new();
    while let Some(ev) = turn.next_event().await {
        transcript.event(label, &ev);
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn live_free_tier_turn_with_tool_call_cancel_and_resume() {
    if !gated() {
        return;
    }
    let Detection::Installed(cli) = detect(&OpenCodeAdapter, &DetectOptions::default()).await
    else {
        eprintln!("[live] opencode not installed; skipping");
        return;
    };
    eprintln!("[live] opencode {} at {}", cli.version, cli.path.display());

    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "# Live proof repo\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);

    let mut transcript = Transcript::new();
    transcript.note(
        "start",
        serde_json::json!({ "opencode": cli.version, "path": cli.path, "workspace": repo.path() }),
    );

    let engine = OpenCodeEngine::start(&cli, EngineOptions::new(repo.path()))
        .await
        .expect("engine start");
    eprintln!(
        "[live] serve at {} version {}",
        engine.base_url(),
        engine.version()
    );

    // 1. Free catalog mirrors OpenCode's keyless list.
    let catalog = engine.free_models().await.expect("free models");
    eprintln!(
        "[live] free models ({}): {:?}; default {:?}",
        catalog.models.len(),
        catalog
            .models
            .iter()
            .map(|m| m.model_ref.as_str())
            .collect::<Vec<_>>(),
        catalog.default_model
    );
    transcript.note("catalog", serde_json::to_value(&catalog).unwrap());
    assert!(!catalog.models.is_empty());
    assert!(
        catalog
            .models
            .iter()
            .all(|m| m.model_ref.starts_with("opencode/"))
    );
    assert!(
        catalog.models.iter().any(|m| m.id == "big-pickle"),
        "Big Pickle expected in the free list"
    );
    let default = catalog.default_or_first().unwrap();
    assert!(default.is_default, "OpenCode reports a default free model");
    let model = default.model_ref.clone();

    // 2. A coding turn with a real tool call, streamed.
    let session = engine
        .create_session(Some("workshop live proof"))
        .await
        .unwrap();
    transcript.note("session", serde_json::json!({ "id": session }));
    let mut req = TurnRequest::new(
        "Create a file named hello.txt in the current directory containing exactly the line: hello from workshop. Use your write tool. Then reply with one short sentence.",
    );
    req.model = Some(model.clone());
    req.permission = PermissionPolicy::WorkspaceWrite;
    let mut turn = engine.prompt(&session, req).await.unwrap();
    let events = drain(&mut turn, "1-tool-call", &mut transcript).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), turn.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed, "{events:?}");
    let written = std::fs::read_to_string(repo.path().join("hello.txt"))
        .expect("hello.txt written by the agent");
    assert_eq!(written.trim(), "hello from workshop");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ToolCall { name, .. } if name == "write")),
        "{events:?}"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::ToolResult {
            is_error: false,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AdapterEvent::TextDelta { .. }))
    );
    assert!(events.iter().any(|e| matches!(e, AdapterEvent::Usage(_))));
    assert!(
        matches!(events.last(), Some(AdapterEvent::Done { session_id: Some(s), .. }) if s == &session)
    );

    // 3. Multi-turn memory, read-only plan agent.
    let mut req = TurnRequest::new(
        "Without using any tools, name the file you created earlier and its exact contents in one sentence.",
    );
    req.model = Some(model.clone());
    let mut turn = engine.prompt(&session, req).await.unwrap();
    let events = drain(&mut turn, "2-memory", &mut transcript).await;
    assert_eq!(turn.wait().await, RunOutcome::Completed, "{events:?}");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    eprintln!("[live] memory answer: {text:?}");
    assert!(text.contains("hello.txt"), "{text}");

    // 4. Cancel mid-stream; the session survives.
    let mut req = TurnRequest::new("Write a 600-word essay about pickles. Do not use any tools.");
    req.model = Some(model.clone());
    let mut turn = engine.prompt(&session, req).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = turn.next_event().await {
        transcript.event("3-cancel", &ev);
        let is_text = matches!(ev, AdapterEvent::TextDelta { .. });
        events.push(ev);
        if is_text {
            turn.cancel();
            break;
        }
    }
    events.extend(drain(&mut turn, "3-cancel", &mut transcript).await);
    let outcome = tokio::time::timeout(Duration::from_secs(30), turn.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Cancelled, "{events:?}");
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::Error {
            message: "run cancelled".into()
        })
    );

    // 5. Restart the engine and resume the same session id.
    engine.shutdown().await;
    transcript.note("restart", serde_json::json!({}));
    let engine = OpenCodeEngine::start(&cli, EngineOptions::new(repo.path()))
        .await
        .expect("engine restart");
    assert!(
        engine.session_exists(&session).await.unwrap(),
        "session persisted across restart"
    );
    let mut req = TurnRequest::new(
        "Without tools: what is the exact name of the file you created in this conversation? Answer with the file name only.",
    );
    req.model = Some(model);
    let mut turn = engine.prompt(&session, req).await.unwrap();
    let events = drain(&mut turn, "4-resume", &mut transcript).await;
    assert_eq!(turn.wait().await, RunOutcome::Completed, "{events:?}");
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    eprintln!("[live] resume answer: {text:?}");
    assert!(text.contains("hello.txt"), "{text}");
    let history = engine.session_messages(&session).await.unwrap();
    let count = history.as_array().map(Vec::len).unwrap_or(0);
    transcript.note("history", serde_json::json!({ "messages": count }));
    assert!(
        count >= 8,
        "expected the full multi-turn history, got {count}"
    );
    engine.shutdown().await;

    eprintln!("[live] transcript: {}", transcript.path.display());
}

#[tokio::test]
async fn live_official_installer_provisions_pinned_version() {
    if !gated() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        target: InstallTarget::Home(home.path().to_path_buf()),
        ..InstallOptions::default()
    };
    let cli = install_opencode(&opts).await.expect("official installer");
    eprintln!("[live] installed {} at {}", cli.version, cli.path.display());
    assert_eq!(cli.path, home.path().join(".opencode/bin/opencode"));
    assert_eq!(cli.version, opts.version);
    // The binary must run from the Workshop-owned location and be healthy.
    let engine = OpenCodeEngine::start(&cli, EngineOptions::new(home.path()))
        .await
        .expect("start installed engine");
    assert_eq!(engine.version(), opts.version);
    engine.shutdown().await;
}
