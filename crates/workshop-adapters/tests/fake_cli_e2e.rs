//! detect -> status -> spawn -> stream -> cancel, end to end against fake
//! vendor CLIs that replay each vendor's real stream shapes.

#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{ALL, CLAUDE, CODEX, CURSOR, FakeVendor, OPENCODE, Sandbox};
use serde_json::json;
use workshop_adapters::vendors;
use workshop_adapters::{
    AdapterEvent, Detection, InstalledCli, LoginState, PermissionPolicy, PinStatus, RailPill,
    RunOutcome, RunRequest, SpawnError, Usage, detect, probe_login, rail_status, spawn,
};

const CLAUDE_SUCCESS: &str = include_str!("fixtures/claude_success.jsonl");
const CLAUDE_ASK: &str = include_str!("fixtures/claude_ask.jsonl");
const CLAUDE_LOGGED_OUT: &str = include_str!("fixtures/claude_logged_out.jsonl");
const CODEX_SUCCESS: &str = include_str!("fixtures/codex_success.jsonl");
const CODEX_LOGGED_OUT: &str = include_str!("fixtures/codex_logged_out.jsonl");
const CURSOR_SUCCESS: &str = include_str!("fixtures/cursor_success.jsonl");
const CURSOR_QUESTION: &str = include_str!("fixtures/cursor_question.jsonl");
const OPENCODE_SUCCESS: &str = include_str!("fixtures/opencode_success.jsonl");
const OPENCODE_ERROR: &str = include_str!("fixtures/opencode_error.jsonl");

async fn detect_installed(sandbox: &Sandbox, vendor: &FakeVendor) -> InstalledCli {
    let adapter = vendors::by_id(vendor.id);
    match detect(adapter.as_ref(), &sandbox.detect_options()).await {
        Detection::Installed(cli) => cli,
        other => panic!("{}: expected Installed, got {other:?}", vendor.id),
    }
}

async fn collect(
    sandbox: &Sandbox,
    vendor: &FakeVendor,
    req: RunRequest,
) -> (Vec<AdapterEvent>, RunOutcome) {
    let adapter = vendors::by_id(vendor.id);
    let cli = detect_installed(sandbox, vendor).await;
    let mut run = spawn(adapter.as_ref(), &cli, req, &sandbox.supervisor_options())
        .await
        .expect("spawn");
    let mut events = Vec::new();
    while let Some(ev) = run.next_event().await {
        events.push(ev);
    }
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.wait())
        .await
        .expect("run.wait() hung");
    (events, outcome)
}

fn td(text: &str) -> AdapterEvent {
    AdapterEvent::TextDelta { text: text.into() }
}

fn last_is_terminal(events: &[AdapterEvent]) -> bool {
    matches!(
        events.last(),
        Some(AdapterEvent::Done { .. }) | Some(AdapterEvent::Error { .. })
    )
}

// ---------------------------------------------------------------- detection

#[tokio::test]
async fn detects_every_fake_vendor_on_path() {
    let sandbox = Sandbox::new();
    for vendor in ALL {
        sandbox.install(vendor);
    }
    for vendor in ALL {
        let cli = detect_installed(&sandbox, vendor).await;
        assert_eq!(cli.adapter, vendor.id);
        assert_eq!(cli.path, sandbox.bin().join(vendor.binary));
        assert_eq!(cli.pin, PinStatus::Tested, "{}: {}", vendor.id, cli.version);
    }
}

#[tokio::test]
async fn detects_in_known_dirs_when_not_on_path() {
    let sandbox = Sandbox::new();
    let known = sandbox.root.path().join("opt-homebrew-bin");
    sandbox.install_as(&known, &CODEX, "codex");
    let mut opts = sandbox.detect_options();
    opts.path_env = Some("/nonexistent".into());
    opts.known_dirs = Some(vec![known.clone()]);
    match detect(vendors::by_id(CODEX.id).as_ref(), &opts).await {
        Detection::Installed(cli) => assert_eq!(cli.path, known.join("codex")),
        other => panic!("expected Installed, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let sandbox = Sandbox::new();
    for vendor in ALL {
        let d = detect(
            vendors::by_id(vendor.id).as_ref(),
            &sandbox.detect_options(),
        )
        .await;
        assert_eq!(d, Detection::NotInstalled, "{}", vendor.id);
    }
}

#[tokio::test]
async fn unrelated_agent_binary_is_not_cursor() {
    let sandbox = Sandbox::new();
    let impostor = sandbox.install_impostor("agent");
    let d = detect(
        vendors::by_id(CURSOR.id).as_ref(),
        &sandbox.detect_options(),
    )
    .await;
    match &d {
        Detection::Unverified { path, reason } => {
            assert_eq!(*path, impostor);
            assert!(
                reason.contains("did not identify itself as Cursor"),
                "{reason}"
            );
        }
        other => panic!("expected Unverified, got {other:?}"),
    }
    // The rail treats it as not installed.
    let rail = rail_status(CURSOR.id, Some(&d), None, false);
    assert!(!rail.installed);
    assert_eq!(rail.pill, RailPill::SignIn);
}

#[tokio::test]
async fn genuine_agent_alias_is_cursor_and_symlink_alias_is_probed_once() {
    let sandbox = Sandbox::new();
    // Only the `agent` name, but the --help banner proves it is Cursor Agent.
    sandbox.install_as(&sandbox.bin(), &CURSOR, "agent");
    let cli = detect_installed(&sandbox, &CURSOR).await;
    assert_eq!(cli.path, sandbox.bin().join("agent"));
    assert_eq!(cli.version, "2026.09.18-9a7762b");

    // Installer layout: cursor-agent plus an `agent` symlink to the same file.
    let sandbox = Sandbox::new();
    let real = sandbox.install(&CURSOR);
    std::os::unix::fs::symlink(&real, sandbox.bin().join("agent")).unwrap();
    let cli = detect_installed(&sandbox, &CURSOR).await;
    assert_eq!(cli.path, real);
}

#[tokio::test]
async fn old_version_is_detected_but_refused_at_spawn() {
    let sandbox = Sandbox::new();
    let old = FakeVendor {
        version_line: "2.0.9 (Claude Code)",
        ..CLAUDE
    };
    sandbox.install(&old);
    let cli = detect_installed(&sandbox, &old).await;
    assert_eq!(cli.pin, PinStatus::OlderThanSupported);
    let err = spawn(
        vendors::by_id(CLAUDE.id).as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &sandbox.supervisor_options(),
    )
    .await
    .err()
    .expect("old version must be refused");
    assert!(
        matches!(err, SpawnError::UnsupportedVersion { .. }),
        "{err}"
    );
}

// ------------------------------------------------------------------- status

#[tokio::test]
async fn status_reports_sign_in_then_ready_for_every_vendor() {
    let sandbox = Sandbox::new();
    for vendor in ALL {
        sandbox.install(vendor);
    }
    let env = sandbox.probe_env();
    for vendor in ALL {
        let adapter = vendors::by_id(vendor.id);
        let cli = detect_installed(&sandbox, vendor).await;

        sandbox.set_logged_in(false);
        let state = probe_login(adapter.as_ref(), &cli, Some(&env), Duration::from_secs(10)).await;
        assert_eq!(state, LoginState::SignIn, "{}", vendor.id);
        let d = Detection::Installed(cli.clone());
        let rail = rail_status(vendor.id, Some(&d), Some(&state), false);
        assert_eq!(rail.pill, RailPill::SignIn);
        assert!(rail.installed && rail.show_connect);

        sandbox.set_logged_in(true);
        let state = probe_login(adapter.as_ref(), &cli, Some(&env), Duration::from_secs(10)).await;
        assert!(
            matches!(state, LoginState::Ready { .. }),
            "{}: {state:?}",
            vendor.id
        );
        let rail = rail_status(vendor.id, Some(&d), Some(&state), false);
        assert_eq!(rail.pill, RailPill::Ready);
    }
}

// ------------------------------------------------------------ spawn/stream

#[tokio::test]
async fn claude_stream_normalizes_and_uses_pinned_flags() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    sandbox.set_fixture(CLAUDE_SUCCESS);
    let (events, outcome) = collect(
        &sandbox,
        &CLAUDE,
        RunRequest::new("summarize README", sandbox.work()),
    )
    .await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        events,
        vec![
            AdapterEvent::Thinking {
                text: "Need to read the file.".into()
            },
            td("Let me look at "),
            td("the README."),
            AdapterEvent::ToolCall {
                id: "toolu_01".into(),
                name: "read".into(),
                input: json!({"filePath": "/work/README.md"}),
            },
            AdapterEvent::ToolResult {
                id: "toolu_01".into(),
                output: "# Demo\n".into(),
                is_error: false
            },
            td("The README says Demo."),
            AdapterEvent::Usage(Usage {
                input_tokens: 30,
                output_tokens: 60,
                cache_read_tokens: 2000,
                cache_write_tokens: 100,
                reasoning_tokens: 0,
                cost_usd: Some(0.0123),
            }),
            AdapterEvent::Done {
                session_id: Some("11111111-2222-4333-8444-555555555555".into()),
                result: Some("The README says Demo.".into()),
            },
        ]
    );
    assert_eq!(
        sandbox.argv(),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "plan"
        ]
    );
    // The prompt goes over stdin as the SDK's `user` message, after its `initialize`.
    let stdin: Vec<serde_json::Value> = sandbox
        .stdin()
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdin lines are JSON"))
        .collect();
    assert_eq!(stdin.len(), 2, "{stdin:?}");
    assert_eq!(stdin[0]["request"]["subtype"], "initialize");
    assert_eq!(stdin[1]["type"], "user");
    assert_eq!(
        stdin[1]["message"]["content"][0]["text"],
        "summarize README"
    );
    assert_eq!(sandbox.child_cwd(), sandbox.work().to_string_lossy());
}

/// Normal mode on the Claude rail (`acceptEdits` + the stdio prompt tool): the CLI asks before
/// the command and blocks until Workshop answers on its stdin; the user's approval (with
/// "always") and the answers to an `AskUserQuestion` reach the CLI as `control_response`
/// lines, and the run then completes.
#[tokio::test]
async fn claude_normal_mode_asks_on_the_channel_and_answers_reach_the_cli() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    sandbox.set_fixture(CLAUDE_ASK);
    let mut req = RunRequest::new("make the page use my wallpaper", sandbox.work());
    req.permission = PermissionPolicy::WorkspaceWrite;
    let adapter = vendors::by_id(CLAUDE.id);
    let cli = detect_installed(&sandbox, &CLAUDE).await;
    let mut run = spawn(adapter.as_ref(), &cli, req, &sandbox.supervisor_options())
        .await
        .expect("spawn");
    let replier = run.replier();
    let mut events = Vec::new();
    while let Some(ev) = tokio::time::timeout(Duration::from_secs(10), run.next_event())
        .await
        .expect("the run must not hang on an unanswered ask")
    {
        match &ev {
            AdapterEvent::Question { id, questions } => {
                assert_eq!(questions[0].options[1].label, "Ocean");
                assert!(
                    replier
                        .reply(workshop_adapters::AskReply::Answer {
                            id: id.clone(),
                            answers: vec![vec!["Ocean".into()]],
                        })
                        .await,
                    "the channel carries the answer"
                );
            }
            AdapterEvent::PermissionAsk { id, tool, input } => {
                assert_eq!(tool, "bash");
                assert_eq!(input["command"], "cp ~/Pictures/ocean.png assets/");
                assert!(
                    replier
                        .reply(workshop_adapters::AskReply::Allow {
                            id: id.clone(),
                            always: true,
                        })
                        .await
                );
            }
            _ => {}
        }
        events.push(ev);
    }
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.wait())
        .await
        .expect("run.wait() hung");
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        sandbox
            .argv()
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]),
        "{:?}",
        sandbox.argv()
    );
    // No plumbing row for the question; the command row is the `Run`.
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::ToolCall { name, input, .. }
            if name == "bash" && input["command"] == "cp ~/Pictures/ocean.png assets/"
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::ToolCall { id, .. } if id == "toolu_q")),
        "{events:#?}"
    );
    assert!(matches!(events.last(), Some(AdapterEvent::Done { .. })));

    let replies: Vec<serde_json::Value> = sandbox
        .replies()
        .iter()
        .map(|l| serde_json::from_str(l).expect("control responses are JSON"))
        .collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0]["response"]["request_id"], "req_q");
    assert_eq!(
        replies[0]["response"]["response"]["updatedInput"]["answers"]["Which wallpaper should the page use?"],
        "Ocean"
    );
    assert_eq!(replies[1]["response"]["request_id"], "req_cp");
    assert_eq!(replies[1]["response"]["response"]["behavior"], "allow");
    assert_eq!(
        replies[1]["response"]["response"]["updatedPermissions"][0]["rules"][0]["ruleContent"],
        "cp:*"
    );
}

/// The three permission policies are each vendor's own modes: Plan is the read-only one,
/// Normal the write-capable one that still asks (where the CLI can), always-approve the
/// run-everything one. Nothing maps Normal or Auto to a plan mode.
#[tokio::test]
async fn every_vendor_maps_plan_normal_and_always_approve_to_its_own_modes() {
    let sandbox = Sandbox::new();
    for vendor in ALL {
        sandbox.install(vendor);
    }
    let fixtures = [
        (&CLAUDE, CLAUDE_SUCCESS),
        (&CODEX, CODEX_SUCCESS),
        (&CURSOR, CURSOR_SUCCESS),
        (&OPENCODE, OPENCODE_SUCCESS),
    ];
    let mut seen = Vec::new();
    for (vendor, fixture) in fixtures {
        for policy in [
            PermissionPolicy::ReadOnly,
            PermissionPolicy::WorkspaceWrite,
            PermissionPolicy::AlwaysApprove,
        ] {
            sandbox.set_fixture(fixture);
            let mut req = RunRequest::new("hi", sandbox.work());
            req.permission = policy;
            let (_, outcome) = collect(&sandbox, vendor, req).await;
            assert_eq!(outcome, RunOutcome::Completed, "{} {policy:?}", vendor.id);
            seen.push((vendor.id, policy, sandbox.argv().join(" ")));
        }
    }
    let argv = |id: workshop_adapters::AdapterId, policy: PermissionPolicy| -> String {
        seen.iter()
            .find(|(v, p, _)| *v == id && *p == policy)
            .map(|(_, _, a)| a.clone())
            .unwrap()
    };
    use PermissionPolicy::{AlwaysApprove, ReadOnly, WorkspaceWrite};
    use workshop_adapters::AdapterId::{Claude, Codex, Cursor, OpenCode};
    assert!(argv(Claude, ReadOnly).contains("--permission-mode plan"));
    assert!(argv(Claude, WorkspaceWrite).contains("--permission-mode acceptEdits"));
    assert!(argv(Claude, AlwaysApprove).contains("--permission-mode bypassPermissions"));
    assert!(argv(Codex, ReadOnly).contains("-s read-only"));
    assert!(argv(Codex, WorkspaceWrite).contains("-s workspace-write"));
    assert!(argv(Codex, AlwaysApprove).contains("-s danger-full-access"));
    assert!(argv(Cursor, ReadOnly).contains("--mode plan"));
    assert!(
        !argv(Cursor, WorkspaceWrite).contains("--mode")
            && !argv(Cursor, WorkspaceWrite).contains("--force")
    );
    assert!(
        argv(Cursor, AlwaysApprove).contains("--force")
            && !argv(Cursor, AlwaysApprove).contains("--mode")
    );
    assert!(argv(OpenCode, ReadOnly).contains("--agent plan"));
    assert!(!argv(OpenCode, WorkspaceWrite).contains("--agent plan"));
    assert!(!argv(OpenCode, AlwaysApprove).contains("--agent plan"));
}

#[tokio::test]
async fn codex_stream_normalizes_and_resume_uses_exec_resume() {
    let sandbox = Sandbox::new();
    sandbox.install(&CODEX);
    sandbox.set_fixture(CODEX_SUCCESS);
    let mut req = RunRequest::new("update README", sandbox.work());
    req.permission = PermissionPolicy::WorkspaceWrite;
    let (events, outcome) = collect(&sandbox, &CODEX, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        events,
        vec![
            AdapterEvent::Thinking {
                text: "Inspecting the repo layout.".into()
            },
            AdapterEvent::ToolCall {
                id: "item_1".into(),
                name: "bash".into(),
                input: json!({"command": "ls"}),
            },
            AdapterEvent::ToolDetail {
                id: "item_1".into(),
                title: Some("ls".into()),
                metadata: json!({"output": "README.md\nsrc\n", "exit": 0}),
            },
            AdapterEvent::ToolResult {
                id: "item_1".into(),
                output: "README.md\nsrc\n".into(),
                is_error: false
            },
            AdapterEvent::ToolCall {
                id: "item_2".into(),
                name: "edit".into(),
                input: json!({"filePath": "README.md", "changes": [{"path": "README.md", "kind": "update"}]}),
            },
            AdapterEvent::ToolResult {
                id: "item_2".into(),
                output: json!([{"path": "README.md", "kind": "update"}]).to_string(),
                is_error: false,
            },
            td("Updated the README."),
            AdapterEvent::Usage(Usage {
                input_tokens: 1200,
                output_tokens: 90,
                cache_read_tokens: 800,
                cache_write_tokens: 0,
                reasoning_tokens: 40,
                cost_usd: None,
            }),
            AdapterEvent::Done {
                session_id: Some("0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b".into()),
                result: Some("Updated the README.".into()),
            },
        ]
    );
    assert_eq!(
        sandbox.argv(),
        vec![
            "exec",
            "--json",
            "-s",
            "workspace-write",
            "--skip-git-repo-check",
            "-"
        ]
    );
    assert_eq!(sandbox.stdin(), "update README");

    // Resume the thread id the first run reported.
    let mut req = RunRequest::new("and the changelog", sandbox.work());
    req.resume = Some("0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b".into());
    let (_, outcome) = collect(&sandbox, &CODEX, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        sandbox.argv(),
        vec![
            "exec",
            "resume",
            "0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b",
            "--json",
            "--skip-git-repo-check",
            "-c",
            "sandbox_mode=\"read-only\"",
            "-"
        ]
    );
}

#[tokio::test]
async fn cursor_stream_normalizes_and_prompt_is_positional() {
    let sandbox = Sandbox::new();
    sandbox.install(&CURSOR);
    sandbox.set_fixture(CURSOR_SUCCESS);
    let mut req = RunRequest::new("summarize README", sandbox.work());
    req.model = Some("sonnet-4".into());
    let (events, outcome) = collect(&sandbox, &CURSOR, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        events,
        vec![
            AdapterEvent::Thinking {
                text: "Reading the README first.".into()
            },
            AdapterEvent::ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                input: json!({"filePath": "README.md"}),
            },
            AdapterEvent::ToolResult {
                id: "call_1".into(),
                output: "# Demo\n".into(),
                is_error: false,
            },
            td("The README "),
            td("is a demo."),
            AdapterEvent::Usage(Usage {
                input_tokens: 500,
                output_tokens: 20,
                cache_read_tokens: 300,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                cost_usd: None,
            }),
            AdapterEvent::Done {
                session_id: Some("c0ffee00-1111-4222-8333-444455556666".into()),
                result: Some("The README is a demo.".into()),
            },
        ]
    );
    assert_eq!(
        sandbox.argv(),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--stream-partial-output",
            "--trust",
            "--mode",
            "plan",
            "--model",
            "sonnet-4",
            "summarize README"
        ]
    );
    assert_eq!(
        sandbox.stdin(),
        "",
        "stdin is /dev/null for positional prompts"
    );

    let mut req = RunRequest::new("go on", sandbox.work());
    req.resume = Some("c0ffee00-1111-4222-8333-444455556666".into());
    let (_, outcome) = collect(&sandbox, &CURSOR, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        sandbox
            .argv()
            .contains(&"--resume=c0ffee00-1111-4222-8333-444455556666".to_string())
    );
}

/// The owner's bench turn (Cursor rail, always-approve): the CLI runs with `--force` so no
/// command is auto-denied; its `editToolCall` / `shellToolCall` become the pager's `edit` /
/// `bash` rows (a rejected command is the row's error, in the CLI's words); the text flushed
/// before each tool call and before `result` is not printed twice; `askQuestionToolCall` is a
/// question for the ask-user view, and the answers resume the same chat as the next prompt.
#[tokio::test]
async fn cursor_question_turn_dedupes_text_maps_rows_and_resumes_with_answers() {
    let sandbox = Sandbox::new();
    sandbox.install(&CURSOR);
    sandbox.set_fixture(CURSOR_QUESTION);
    let mut req = RunRequest::new("create a landing page with the wallpaper", sandbox.work());
    req.permission = PermissionPolicy::AlwaysApprove;
    let (events, outcome) = collect(&sandbox, &CURSOR, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert!(
        sandbox.argv().contains(&"--force".to_string()),
        "always-approve is the CLI's run-everything flag: {:?}",
        sandbox.argv()
    );
    assert!(!sandbox.argv().contains(&"--mode".to_string()));

    let text: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text,
        vec![
            "Writing the page first, ",
            "then I'll need one approval.",
            "Tell me which wallpaper you want and I'll finish."
        ],
        "each paragraph exactly once: {events:#?}"
    );
    let rows: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            AdapterEvent::ToolCall { name, input, .. } => Some((
                name.as_str(),
                input
                    .get("filePath")
                    .or_else(|| input.get("command"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("edit", "/work/index.html"),
            ("bash", "cp ~/Pictures/wallpaper.png assets/")
        ]
    );
    let debug = format!("{events:?}");
    for key in ["shellToolCall", "editToolCall", "askQuestionToolCall"] {
        assert!(
            !debug.contains(key),
            "protobuf oneof key {key} reached the host: {events:#?}"
        );
    }
    assert!(events.iter().any(|e| matches!(
        e,
        AdapterEvent::ToolResult { id, output, is_error: true }
            if id == "call_cp" && output.contains("requires approval")
    )));
    let question = events
        .iter()
        .find_map(|e| match e {
            AdapterEvent::Question { id, questions } => Some((id.clone(), questions.clone())),
            _ => None,
        })
        .expect("the CLI's question reaches the host");
    assert_eq!(question.0, "call_q");
    assert_eq!(
        question.1[0].question,
        "Which wallpaper should the page use?"
    );
    assert_eq!(question.1[0].options[1].label, "Ocean");
    let session = match events.last() {
        Some(AdapterEvent::Done { session_id, .. }) => session_id.clone().expect("session id"),
        other => panic!("expected Done, got {other:?}"),
    };

    // The user's answer continues the same chat: the answers prompt is the next positional
    // argument of a `--resume=<chat>` run.
    let answers = workshop_adapters::question_answers_prompt(&question.1, &[vec!["Ocean".into()]]);
    assert_eq!(
        answers,
        "My answers to your questions:\n- Which wallpaper should the page use?: Ocean\nContinue with the task using these answers."
    );
    sandbox.set_fixture(CURSOR_SUCCESS);
    let mut req = RunRequest::new(answers.clone(), sandbox.work());
    req.resume = Some(session.clone());
    req.permission = PermissionPolicy::AlwaysApprove;
    let (_, outcome) = collect(&sandbox, &CURSOR, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    let argv = sandbox.argv();
    assert!(argv.contains(&format!("--resume={session}")), "{argv:?}");
    // The fake records one argv line per `\n`, so the multi-line prompt is the joined tail.
    assert!(argv.join("\n").ends_with(&answers), "{argv:?}");
}

#[tokio::test]
async fn opencode_stream_normalizes_and_exit_zero_is_done() {
    let sandbox = Sandbox::new();
    sandbox.install(&OPENCODE);
    sandbox.set_fixture(OPENCODE_SUCCESS);
    let (events, outcome) = collect(
        &sandbox,
        &OPENCODE,
        RunRequest::new("summarize README", sandbox.work()),
    )
    .await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        events,
        vec![
            AdapterEvent::Thinking {
                text: "Check the README.".into()
            },
            AdapterEvent::ToolCall {
                id: "call_9".into(),
                name: "read".into(),
                input: json!({"filePath": "/work/README.md"}),
            },
            AdapterEvent::ToolResult {
                id: "call_9".into(),
                output: "# Demo\n".into(),
                is_error: false
            },
            td("The README is a demo."),
            AdapterEvent::Usage(Usage {
                input_tokens: 400,
                output_tokens: 25,
                cache_read_tokens: 100,
                cache_write_tokens: 0,
                reasoning_tokens: 10,
                cost_usd: Some(0.0),
            }),
            AdapterEvent::Done {
                session_id: Some("ses_abc123".into()),
                result: Some("The README is a demo.".into()),
            },
        ]
    );
    assert_eq!(
        sandbox.argv(),
        vec![
            "run",
            "--format",
            "json",
            "--thinking",
            "--agent",
            "plan",
            "summarize README"
        ]
    );

    let mut req = RunRequest::new("more", sandbox.work());
    req.resume = Some("ses_abc123".into());
    req.permission = PermissionPolicy::WorkspaceWrite;
    let (_, outcome) = collect(&sandbox, &OPENCODE, req).await;
    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        sandbox.argv(),
        vec![
            "run",
            "--format",
            "json",
            "--thinking",
            "--session",
            "ses_abc123",
            "more"
        ]
    );
}

#[tokio::test]
async fn child_env_is_minimal_and_has_no_api_keys() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    sandbox.set_fixture(CLAUDE_SUCCESS);
    let mut opts = sandbox.supervisor_options();
    let mut env = sandbox.probe_env();
    for (k, v) in [
        ("ANTHROPIC_API_KEY", "sk-ant-secret"),
        ("ANTHROPIC_BASE_URL", "http://127.0.0.1:3456"),
        ("OPENAI_API_KEY", "sk-secret"),
        ("XAI_API_KEY", "xai-secret"),
        ("CURSOR_API_KEY", "cur-secret"),
        ("GROK_HOME", "/tmp/grok"),
        ("CLAUDE_CONFIG_DIR", "/tmp/claude-config"),
    ] {
        env.insert(k.into(), v.into());
    }
    opts.env = Some(env);
    let adapter = vendors::by_id(CLAUDE.id);
    let cli = detect_installed(&sandbox, &CLAUDE).await;
    let run = spawn(
        adapter.as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(run.wait().await, RunOutcome::Completed);

    let child_env = sandbox.child_env();
    let has = |k: &str| child_env.iter().any(|l| l.starts_with(&format!("{k}=")));
    for leaked in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "XAI_API_KEY",
        "CURSOR_API_KEY",
        "GROK_HOME",
    ] {
        assert!(
            !has(leaked),
            "{leaked} leaked into the child: {child_env:?}"
        );
    }
    assert!(has("CLAUDE_CONFIG_DIR"), "vendor config dir passes through");
    assert!(child_env.contains(&"NO_COLOR=1".to_string()));
    assert!(
        !child_env.iter().any(|l| l.contains("secret")),
        "{child_env:?}"
    );

    // Secret-shaped extras are rejected before anything is spawned.
    let mut opts = sandbox.supervisor_options();
    opts.extra_env.push(("MY_SERVICE_TOKEN".into(), "x".into()));
    let err = spawn(
        adapter.as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &opts,
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, SpawnError::Env(_)), "{err}");
}

// ------------------------------------------------------------- fail closed

#[tokio::test]
async fn real_logged_out_claude_capture_fails_closed() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    sandbox.set_fixture(CLAUDE_LOGGED_OUT);
    sandbox.set_exit_code(1);
    let (events, outcome) = collect(&sandbox, &CLAUDE, RunRequest::new("hi", sandbox.work())).await;
    assert!(last_is_terminal(&events));
    assert_eq!(
        events[0],
        AdapterEvent::Error {
            message: "authentication_failed: Not logged in · Please run /login".into()
        }
    );
    assert!(matches!(events[1], AdapterEvent::Usage(_)));
    assert_eq!(
        events[2],
        AdapterEvent::Error {
            message: "Not logged in · Please run /login".into()
        }
    );
    match outcome {
        RunOutcome::Failed { reason, .. } => {
            assert_eq!(reason, "Not logged in · Please run /login")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn real_logged_out_codex_capture_fails_closed() {
    let sandbox = Sandbox::new();
    sandbox.install(&CODEX);
    sandbox.set_fixture(CODEX_LOGGED_OUT);
    sandbox.set_exit_code(1);
    let (events, outcome) = collect(&sandbox, &CODEX, RunRequest::new("hi", sandbox.work())).await;
    assert!(last_is_terminal(&events));
    assert!(
        events
            .iter()
            .all(|e| matches!(e, AdapterEvent::Error { .. })),
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AdapterEvent::Done { .. }))
    );
    match outcome {
        RunOutcome::Failed { reason, .. } => {
            assert!(reason.contains("401 Unauthorized"), "{reason}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn opencode_error_event_fails_closed() {
    let sandbox = Sandbox::new();
    sandbox.install(&OPENCODE);
    sandbox.set_fixture(OPENCODE_ERROR);
    sandbox.set_exit_code(1);
    let (events, outcome) =
        collect(&sandbox, &OPENCODE, RunRequest::new("hi", sandbox.work())).await;
    assert_eq!(
        events,
        vec![AdapterEvent::Error {
            message: "Invalid API key".into()
        }]
    );
    assert!(
        matches!(outcome, RunOutcome::Failed { ref reason, .. } if reason == "Invalid API key"),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn exit_zero_without_terminal_event_fails_closed() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    let truncated: String = CLAUDE_SUCCESS
        .lines()
        .take(9)
        .collect::<Vec<_>>()
        .join("\n");
    sandbox.set_fixture(&truncated);
    let (events, outcome) = collect(&sandbox, &CLAUDE, RunRequest::new("hi", sandbox.work())).await;
    assert!(last_is_terminal(&events));
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::Error {
            message: "claude exited (0) before a `result` event".into()
        })
    );
    assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
}

#[tokio::test]
async fn non_json_output_fails_closed_and_kills_child() {
    let sandbox = Sandbox::new();
    sandbox.install(&CURSOR);
    sandbox.set_fixture(
        "Welcome to a new CLI version with a different output format\n{\"type\":\"result\"}\n",
    );
    let (events, outcome) = collect(&sandbox, &CURSOR, RunRequest::new("hi", sandbox.work())).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        matches!(&events[0], AdapterEvent::Error { message } if message.contains("not JSON")),
        "{events:?}"
    );
    assert!(
        matches!(outcome, RunOutcome::Failed { ref reason, .. } if reason.contains("schema drift")),
        "{outcome:?}"
    );
}

// ------------------------------------------------------------------ cancel

#[tokio::test]
async fn cancel_sends_sigint_and_child_exits_gracefully() {
    let sandbox = Sandbox::new();
    sandbox.install(&CODEX);
    sandbox.set_fixture(CODEX_SUCCESS);
    sandbox.set_mode("hang");
    let adapter = vendors::by_id(CODEX.id);
    let cli = detect_installed(&sandbox, &CODEX).await;
    let mut run = spawn(
        adapter.as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &sandbox.supervisor_options(),
    )
    .await
    .unwrap();
    let sid = tokio::time::timeout(Duration::from_secs(10), run.wait_session_id())
        .await
        .expect("session id before cancel");
    assert_eq!(sid.as_deref(), Some("0199a1b2-c3d4-7e5f-8a6b-7c8d9e0f1a2b"));
    let grandchild = wait_for_grandchild(&sandbox).await;

    run.cancel();
    run.cancel(); // idempotent
    let mut events = Vec::new();
    while let Some(ev) = run.next_event().await {
        events.push(ev);
    }
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(
        events.last(),
        Some(&AdapterEvent::Error {
            message: "run cancelled".into()
        })
    );
    assert!(sandbox.got_sigint(), "fake must have handled SIGINT");
    assert!(
        common::wait_process_gone(grandchild, Duration::from_secs(3)).await,
        "descendant survived cancel"
    );
}

/// The fake records its background child's pid right after its first output
/// line; wait for that file so the descendant check is meaningful.
async fn wait_for_grandchild(sandbox: &Sandbox) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(pid) = sandbox.grandchild_pid() {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "grandchild pid never recorded"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn cancel_escalates_to_sigkill_for_the_whole_process_group() {
    let sandbox = Sandbox::new();
    sandbox.install(&CLAUDE);
    sandbox.set_fixture(CLAUDE_SUCCESS);
    sandbox.set_mode("hang-ignore-int");
    let adapter = vendors::by_id(CLAUDE.id);
    let cli = detect_installed(&sandbox, &CLAUDE).await;
    let mut run = spawn(
        adapter.as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &sandbox.supervisor_options(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), run.wait_session_id())
        .await
        .expect("session id before cancel");
    let leader = run.pid() as i32;
    let grandchild = wait_for_grandchild(&sandbox).await;

    let started = tokio::time::Instant::now();
    run.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.wait())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "SIGKILL must wait out the grace period"
    );
    assert!(!sandbox.got_sigint(), "fake ignored SIGINT by design");
    assert!(
        common::wait_process_gone(leader, Duration::from_secs(3)).await,
        "leader survived SIGKILL"
    );
    assert!(
        common::wait_process_gone(grandchild, Duration::from_secs(3)).await,
        "descendant survived group kill"
    );
}

#[tokio::test]
async fn idle_timeout_fails_the_run() {
    let sandbox = Sandbox::new();
    sandbox.install(&OPENCODE);
    sandbox.set_fixture(OPENCODE_SUCCESS);
    sandbox.set_mode("hang");
    let mut opts = sandbox.supervisor_options();
    opts.idle_timeout = Some(Duration::from_millis(300));
    let adapter = vendors::by_id(OPENCODE.id);
    let cli = detect_installed(&sandbox, &OPENCODE).await;
    let run = spawn(
        adapter.as_ref(),
        &cli,
        RunRequest::new("hi", sandbox.work()),
        &opts,
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.wait())
        .await
        .unwrap();
    assert!(
        matches!(outcome, RunOutcome::Failed { ref reason, .. } if reason.contains("no output for")),
        "{outcome:?}"
    );
}
