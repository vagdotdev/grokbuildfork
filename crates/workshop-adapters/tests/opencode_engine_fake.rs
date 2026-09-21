//! OpenCode engine against a fake `opencode serve` (fixtures captured from the
//! real server): attach, catalog, turn streaming, cancel, permissions, and
//! the spawn path through a fake `opencode` binary.

#![cfg(unix)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use common::fake_serve::{FakeServe, SESSION_ID, Script};
use common::{OPENCODE, Sandbox};
use serde_json::json;
use workshop_adapters::opencode_engine::{
    EngineOptions, OpenCodeEngine, PermissionReply, TurnHandle, TurnRequest,
};
use workshop_adapters::vendors;
use workshop_adapters::{AdapterEvent, Detection, PermissionPolicy, RunOutcome, Usage, detect};

async fn attach(server: &FakeServe) -> OpenCodeEngine {
    let mut opts = EngineOptions::new("/work");
    opts.cancel_grace = Duration::from_secs(5);
    OpenCodeEngine::attach(server.addr, "opencode", "pw", opts)
        .await
        .expect("attach to fake serve")
}

async fn drain(turn: &mut TurnHandle) -> Vec<AdapterEvent> {
    let mut out = Vec::new();
    while let Some(ev) = turn.next_event().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn attach_mirrors_free_catalog_from_providers_endpoint() {
    let server = FakeServe::start().await;
    let engine = attach(&server).await;
    assert_eq!(engine.version(), "1.18.31");

    let catalog = engine.free_models().await.unwrap();
    assert_eq!(
        catalog.default_model.as_deref(),
        Some("opencode/big-pickle")
    );
    let ids: Vec<&str> = catalog.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids[0], "big-pickle", "default first");
    assert_eq!(ids.len(), 8, "{ids:?}");
    assert!(!ids.contains(&"old-thing-free"), "deprecated rows dropped");
    assert!(!ids.contains(&"gpt-5-nano"), "paid rows dropped");
    let bp = &catalog.models[0];
    assert!(bp.is_default && bp.tool_call && bp.reasoning);
    assert_eq!(bp.context_limit, Some(200_000));
    assert_eq!(bp.model_ref, "opencode/big-pickle");

    let providers_req = server
        .requests()
        .into_iter()
        .find(|r| r.path.starts_with("/config/providers"))
        .expect("providers request");
    assert!(
        providers_req.path.contains("directory=/work"),
        "{}",
        providers_req.path
    );
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("opencode:pw")
    );
    assert_eq!(
        providers_req.authorization.as_deref(),
        Some(expected.as_str())
    );
    server.stop();
}

#[tokio::test]
async fn turn_streams_captured_events_into_normalized_stream() {
    let server = FakeServe::start().await;
    let engine = attach(&server).await;
    let session = engine.create_session(Some("fake")).await.unwrap();
    assert_eq!(session, SESSION_ID);
    assert!(engine.session_exists(&session).await.unwrap());
    assert!(!engine.session_exists("ses_nope").await.unwrap());

    let mut req = TurnRequest::new("Create hello.txt");
    req.model = Some("opencode/big-pickle".into());
    req.permission = PermissionPolicy::WorkspaceWrite;
    let mut turn = engine.prompt(&session, req).await.unwrap();
    assert_eq!(turn.session_id(), SESSION_ID);
    let events = drain(&mut turn).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), turn.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed, "{events:?}");
    assert_eq!(
        events,
        vec![
            AdapterEvent::ToolCall {
                id: "call_febe5aee77f2467281fdec81".into(),
                name: "write".into(),
                input: json!({"filePath": "/work/hello.txt", "content": "hello from workshop"}),
            },
            AdapterEvent::ToolResult {
                id: "call_febe5aee77f2467281fdec81".into(),
                output: "Wrote file successfully.".into(),
                is_error: false,
            },
            AdapterEvent::Usage(Usage {
                input_tokens: 6518,
                output_tokens: 74,
                cache_read_tokens: 1792,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: Some(0.0),
            }),
            AdapterEvent::TextDelta {
                text: "Created hello.txt with".into()
            },
            AdapterEvent::TextDelta {
                text: " the exact line.".into()
            },
            AdapterEvent::Usage(Usage {
                input_tokens: 206,
                output_tokens: 10,
                cache_read_tokens: 8192,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: Some(0.0),
            }),
            AdapterEvent::Done {
                session_id: Some(SESSION_ID.into()),
                result: Some("Created hello.txt with the exact line.".into()),
            },
        ]
    );

    let prompt_req = server
        .requests()
        .into_iter()
        .find(|r| r.path.contains("/prompt_async"))
        .expect("prompt_async request");
    let body = prompt_req.body.unwrap();
    assert_eq!(body["agent"], "build");
    assert_eq!(
        body["model"],
        json!({"providerID": "opencode", "modelID": "big-pickle"})
    );
    assert_eq!(
        body["parts"],
        json!([{"type": "text", "text": "Create hello.txt"}])
    );
    assert!(prompt_req.path.contains("directory=/work"));

    // Read-only turns use the plan agent and can omit the model.
    let mut turn = engine
        .prompt(&session, TurnRequest::new("look around"))
        .await
        .unwrap();
    drain(&mut turn).await;
    assert_eq!(turn.wait().await, RunOutcome::Completed);
    let last = server
        .requests()
        .into_iter()
        .rev()
        .find(|r| r.path.contains("/prompt_async"))
        .unwrap();
    let body = last.body.unwrap();
    assert_eq!(body["agent"], "plan");
    assert!(body.get("model").is_none());
    server.stop();
}

#[tokio::test]
async fn cancel_posts_abort_and_reports_cancelled() {
    let server = FakeServe::start().await;
    server.set_script(Script::AbortAfterFirstDelta);
    let engine = attach(&server).await;
    let mut turn = engine
        .prompt(SESSION_ID, TurnRequest::new("essay"))
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(ev) = turn.next_event().await {
        let is_text = matches!(ev, AdapterEvent::TextDelta { .. });
        events.push(ev);
        if is_text {
            turn.cancel();
            break;
        }
    }
    events.extend(drain(&mut turn).await);
    let outcome = tokio::time::timeout(Duration::from_secs(10), turn.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Cancelled, "{events:?}");
    assert_eq!(
        events.first(),
        Some(&AdapterEvent::TextDelta {
            text: "\n\n".into()
        })
    );
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::Error {
            message: "run cancelled".into()
        })
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::Error { message } if message.contains("Aborted"))),
        "the server's MessageAbortedError is not surfaced as a vendor error: {events:?}"
    );
    assert!(
        server
            .requests()
            .iter()
            .any(|r| r.method == "POST" && r.path.contains("/abort"))
    );
    server.stop();
}

#[tokio::test]
async fn permission_asks_are_rejected_by_default_and_routed_to_a_handler() {
    let server = FakeServe::start().await;
    server.set_script(Script::PermissionThenTurn);
    let engine = attach(&server).await;
    let mut turn = engine
        .prompt(SESSION_ID, TurnRequest::new("dangerous"))
        .await
        .unwrap();
    drain(&mut turn).await;
    assert_eq!(turn.wait().await, RunOutcome::Completed);
    assert_eq!(
        server.state.lock().unwrap().permission_replies,
        vec![("perm_1".to_string(), "reject".to_string())]
    );

    server.state.lock().unwrap().permission_replies.clear();
    server.set_script(Script::PermissionThenTurn);
    let mut opts = EngineOptions::new("/work");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_in_handler = seen.clone();
    opts.permission_handler = Some(Arc::new(move |req| {
        seen_in_handler.lock().unwrap().push(req.clone());
        PermissionReply::Once
    }));
    let engine = OpenCodeEngine::attach(server.addr, "opencode", "pw", opts)
        .await
        .unwrap();
    let mut turn = engine
        .prompt(SESSION_ID, TurnRequest::new("dangerous"))
        .await
        .unwrap();
    drain(&mut turn).await;
    assert_eq!(turn.wait().await, RunOutcome::Completed);
    assert_eq!(
        server.state.lock().unwrap().permission_replies,
        vec![("perm_1".to_string(), "once".to_string())]
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].kind, "bash");
    assert_eq!(seen[0].patterns, vec!["rm -rf *"]);
    assert_eq!(seen[0].title, "rm -rf build");
    server.stop();
}

#[tokio::test]
async fn start_spawns_the_binary_parses_listening_line_and_shuts_down() {
    let server = FakeServe::start().await;
    let sandbox = Sandbox::new();
    sandbox.install(&OPENCODE);
    sandbox.set_serve_port(server.addr.port());
    let Detection::Installed(cli) = detect(
        vendors::by_id(OPENCODE.id).as_ref(),
        &sandbox.detect_options(),
    )
    .await
    else {
        panic!("fake opencode not detected");
    };

    let mut opts = EngineOptions::new(sandbox.work());
    let mut env = sandbox.probe_env();
    env.insert("OPENCODE_PERMISSION".into(), "{\"edit\":\"allow\"}".into());
    env.insert("OPENAI_API_KEY".into(), "sk-secret".into());
    opts.env = Some(env);
    let engine = OpenCodeEngine::start(&cli, opts)
        .await
        .expect("engine start via fake binary");
    assert_eq!(engine.version(), "1.18.31");
    assert_eq!(engine.base_url(), format!("http://{}", server.addr));

    let argv = sandbox.serve_argv();
    assert_eq!(&argv[..3], &["serve", "--hostname", "127.0.0.1"]);
    assert_eq!(argv[3], "--port");
    assert!(argv[4].parse::<u16>().is_ok(), "{argv:?}");

    let env = sandbox.serve_env();
    let password = env
        .iter()
        .find_map(|l| l.strip_prefix("OPENCODE_SERVER_PASSWORD="))
        .expect("server password set")
        .to_string();
    assert_eq!(password.len(), 48, "random hex password");
    assert!(
        !env.iter().any(|l| l.starts_with("OPENCODE_PERMISSION=")),
        "OPENCODE_PERMISSION must never reach opencode (it trips the free-tier 403)"
    );
    assert!(!env.iter().any(|l| l.starts_with("OPENAI_API_KEY=")));

    // The engine authenticated to the server with that generated password.
    let health = server
        .requests()
        .into_iter()
        .find(|r| r.path.starts_with("/global/health"))
        .unwrap();
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
    );
    assert_eq!(health.authorization.as_deref(), Some(expected.as_str()));

    let catalog = engine.free_models().await.unwrap();
    assert_eq!(catalog.models.len(), 8);
    engine.shutdown().await;
    server.stop();
}
