//! End-to-end conformance of the supervisor against fake vendor CLIs (`tests/fixtures/bin`).
//!
//! The fakes reproduce the documented JSON streams of Claude Code, Codex, Cursor Agent, and
//! OpenCode, and expose failure modes through `FAKE_CLI_MODE`. Every test drives the real
//! supervisor: locate → identify → spawn → normalize → finalize.

#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use workshop_adapters::{
    AdapterEvent, FailureReason, PermissionProfile, RunOutcome, RunRequest, RunStatus, Supervisor,
    SupervisorConfig, SupportMatrix, Vendor, Workdir,
};
use workshop_detect::DetectConfig;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

struct Harness {
    state: tempfile::TempDir,
    work: tempfile::TempDir,
    supervisor: Supervisor,
}

impl Harness {
    fn new(bin_dirs: &[&str]) -> Self {
        let state = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let path = std::env::join_paths(bin_dirs.iter().map(|d| fixtures().join(d))).unwrap();
        let mut detect = DetectConfig::hermetic(path, state.path().join("home"));
        detect.timeout = Duration::from_secs(10);
        let supervisor = Supervisor::new(SupervisorConfig {
            detect,
            kill_grace: Duration::from_millis(500),
            ..SupervisorConfig::default()
        });
        Self {
            state,
            work,
            supervisor,
        }
    }

    fn request(&self, vendor: Vendor, mode: &str) -> RunRequest {
        let mut req = RunRequest::new(
            vendor,
            "summarize the repository",
            Workdir::in_place_acknowledged(self.work.path()),
        );
        req.timeout = Duration::from_secs(20);
        req.idle_timeout = Duration::from_secs(10);
        req.extra_env.push((
            OsString::from("FAKE_CLI_STATE_DIR"),
            self.state.path().as_os_str().to_owned(),
        ));
        req.extra_env
            .push((OsString::from("FAKE_CLI_MODE"), OsString::from(mode)));
        req
    }

    async fn run(&self, req: RunRequest) -> (Vec<AdapterEvent>, RunOutcome) {
        let (tx, mut rx) = mpsc::channel(256);
        let cancel = CancellationToken::new();
        let sup = self.supervisor.clone();
        let outcome = tokio::spawn(async move { sup.run(req, tx, cancel).await });
        let mut events = Vec::new();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        (events, outcome.await.unwrap())
    }

    fn state_file(&self, name: &str) -> String {
        std::fs::read_to_string(self.state.path().join(name)).unwrap_or_default()
    }

    fn run_args(&self, vendor: Vendor) -> Vec<String> {
        self.state_file(&format!("run-args.{}", vendor.id()))
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn has<F: Fn(&AdapterEvent) -> bool>(events: &[AdapterEvent], f: F) -> bool {
    events.iter().any(f)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_and_stream_every_vendor() {
    for vendor in Vendor::ALL {
        let h = Harness::new(&["bin"]);
        let (events, outcome) = h.run(h.request(vendor, "ok")).await;
        assert_eq!(outcome.status, RunStatus::Completed, "{vendor:?}: {outcome:#?}");
        assert_eq!(outcome.exit_code, Some(0));

        // Event order: Started first, Session before any Text, Completed last.
        assert!(matches!(events.first(), Some(AdapterEvent::Started { vendor: v, pid: Some(_) }) if *v == vendor));
        let session_at = events.iter().position(|e| matches!(e, AdapterEvent::Session { .. })).expect("session id");
        let first_text = events.iter().position(|e| matches!(e, AdapterEvent::Text { .. })).expect("text");
        assert!(session_at < first_text, "{vendor:?}");
        assert!(matches!(events.last(), Some(AdapterEvent::Completed { .. })), "{vendor:?}: {:?}", events.last());
        assert!(has(&events, |e| matches!(e, AdapterEvent::ToolStarted { .. })), "{vendor:?}");
        assert!(has(&events, |e| matches!(e, AdapterEvent::ToolCompleted { .. })), "{vendor:?}");
        assert!(has(&events, |e| matches!(e, AdapterEvent::Usage(_))), "{vendor:?}");
        // Cursor's documented `result` event carries duration, not token usage.
        if vendor != Vendor::Cursor {
            assert!(has(&events, |e| matches!(e, AdapterEvent::Usage(u) if u.input_tokens.is_some())), "{vendor:?}");
        }
        assert!(!has(&events, |e| matches!(e, AdapterEvent::Failed { .. } | AdapterEvent::Cancelled)));

        let session = outcome.session_id.clone().expect("session id in outcome");
        assert_eq!(outcome.final_text.as_deref(), Some(format!("turn 1 of {session}").as_str()));
        assert_eq!(outcome.usage.input_tokens.is_some(), vendor != Vendor::Cursor);
        assert_eq!(outcome.unknown_events, 0, "{vendor:?}: fakes only emit modelled events");

        // The CLI ran in the requested directory with the documented flags.
        assert_eq!(
            h.state_file(&format!("run-cwd.{}", vendor.id())).trim(),
            dunce::canonicalize(h.work.path()).unwrap().to_string_lossy()
        );
        let args = h.run_args(vendor);
        assert_eq!(args.last().unwrap(), "summarize the repository");
        match vendor {
            Vendor::Claude => {
                assert_eq!(&args[..4], &["-p", "--output-format", "stream-json", "--verbose"]);
                assert!(args.contains(&"--permission-mode".to_string()));
            }
            Vendor::Codex => {
                assert_eq!(&args[..4], &["exec", "--json", "--color", "never"]);
                assert!(args.windows(2).any(|w| w == ["--sandbox", "read-only"]));
            }
            Vendor::Cursor => {
                assert_eq!(&args[..3], &["-p", "--output-format", "stream-json"]);
                assert!(args.contains(&"--trust".to_string()));
                assert!(!args.contains(&"--force".to_string()));
            }
            Vendor::OpenCode => {
                assert_eq!(&args[..3], &["run", "--format", "json"]);
                let env = h.state_file("run-env.opencode");
                assert!(env.contains("OPENCODE_PERMISSION="), "{env}");
                assert!(env.contains(r#""edit":"deny""#));
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_the_vendor_session() {
    for vendor in Vendor::ALL {
        let h = Harness::new(&["bin"]);
        let (_, first) = h.run(h.request(vendor, "ok")).await;
        let session = first.session_id.clone().unwrap();

        let (_, second) = h.run(h.request(vendor, "ok").with_resume(session.clone())).await;
        assert_eq!(second.status, RunStatus::Completed, "{vendor:?}");
        assert_eq!(second.session_id.as_deref(), Some(session.as_str()));
        assert_eq!(second.final_text.as_deref(), Some(format!("turn 2 of {session}").as_str()), "{vendor:?}");

        let args = h.run_args(vendor);
        match vendor {
            Vendor::Claude => assert!(args.windows(2).any(|w| w == ["--resume", session.as_str()])),
            Vendor::Codex => assert!(args.windows(2).any(|w| w == ["resume", session.as_str()])),
            Vendor::Cursor => assert!(args.contains(&format!("--resume={session}"))),
            Vendor::OpenCode => assert!(args.windows(2).any(|w| w == ["--session", session.as_str()])),
        }
    }
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_kills_the_whole_process_group() {
    for vendor in Vendor::ALL {
        let h = Harness::new(&["bin"]);
        let mut handle = h.supervisor.start(h.request(vendor, "slow")).unwrap();
        // Wait until the child is running and has spawned its grandchild.
        let grandchild = h.state.path().join("grandchild.pid");
        let started = Instant::now();
        while !grandchild.exists() {
            assert!(started.elapsed() < Duration::from_secs(10), "{vendor:?}: fake never spawned grandchild");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let gpid: i32 = std::fs::read_to_string(&grandchild).unwrap().trim().parse().unwrap();
        assert!(pid_alive(gpid), "{vendor:?}: grandchild should be alive before cancel");

        handle.cancel();
        let cancel_at = Instant::now();
        let mut seen = Vec::new();
        while let Some(e) = handle.events.recv().await {
            seen.push(e);
        }
        let outcome = handle.wait().await;
        assert_eq!(outcome.status, RunStatus::Cancelled, "{vendor:?}");
        assert!(matches!(seen.last(), Some(AdapterEvent::Cancelled)), "{vendor:?}: {:?}", seen.last());
        assert!(cancel_at.elapsed() < Duration::from_secs(5), "{vendor:?}: cancel took {:?}", cancel_at.elapsed());

        let deadline = Instant::now() + Duration::from_secs(3);
        while pid_alive(gpid) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!pid_alive(gpid), "{vendor:?}: grandchild {gpid} survived cancel");
        let _ = std::fs::remove_file(&grandchild);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overall_timeout_and_idle_timeout_fail_closed() {
    let h = Harness::new(&["bin"]);
    let mut req = h.request(Vendor::Claude, "idle");
    req.idle_timeout = Duration::from_millis(400);
    let started = Instant::now();
    let (events, outcome) = h.run(req).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::IdleTimeout { .. } }), "{outcome:#?}");
    assert!(matches!(events.last(), Some(AdapterEvent::Failed { .. })));
    assert!(started.elapsed() < Duration::from_secs(8));

    let mut req = h.request(Vendor::Codex, "slow");
    req.timeout = Duration::from_millis(600);
    req.idle_timeout = Duration::from_secs(30);
    let (_, outcome) = h.run(req).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::Timeout { .. } }), "{outcome:#?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_drift_fails_closed() {
    for vendor in Vendor::ALL {
        let h = Harness::new(&["bin"]);
        let (_, outcome) = h.run(h.request(vendor, "drift")).await;
        assert!(
            matches!(&outcome.status, RunStatus::Failed { reason: FailureReason::SchemaDrift { detail } } if detail.contains("non-JSON")),
            "{vendor:?}: {outcome:#?}"
        );
        let (_, outcome) = h.run(h.request(vendor, "no_terminal")).await;
        assert!(
            matches!(&outcome.status, RunStatus::Failed { reason: FailureReason::SchemaDrift { detail } } if detail.contains("terminal event")),
            "{vendor:?}: {outcome:#?}"
        );
        assert_eq!(outcome.exit_code, Some(0), "drift is detected even on exit 0");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vendor_errors_and_nonzero_exits_fail() {
    let h = Harness::new(&["bin"]);
    for vendor in [Vendor::Claude, Vendor::Codex, Vendor::OpenCode] {
        let (_, outcome) = h.run(h.request(vendor, "fail")).await;
        assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::VendorError { .. } }), "{vendor:?}: {outcome:#?}");
        assert_eq!(outcome.exit_code, Some(1));
    }
    // Cursor reports auth failure on stderr and exits 1 without JSON.
    let (_, outcome) = h.run(h.request(Vendor::Cursor, "fail")).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::NonZeroExit { code: Some(1) } }), "{outcome:#?}");
    assert!(outcome.stderr_tail.iter().any(|l| l.contains("Authentication required")), "{:?}", outcome.stderr_tail);

    for vendor in Vendor::ALL {
        let (_, outcome) = h.run(h.request(vendor, "exit_nonzero")).await;
        assert!(
            matches!(outcome.status, RunStatus::Failed { reason: FailureReason::NonZeroExit { code: Some(3) } }),
            "{vendor:?}: completed stream + exit 3 must still fail: {outcome:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_bounds_hold() {
    let h = Harness::new(&["bin"]);
    let mut req = h.request(Vendor::OpenCode, "huge");
    req.max_line_bytes = 64 * 1024;
    let (_, outcome) = h.run(req).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::OutputBound { max_line_bytes: 65536 } }), "{outcome:#?}");

    let (events, outcome) = h.run(h.request(Vendor::Codex, "stderr_flood")).await;
    assert_eq!(outcome.status, RunStatus::Completed, "{outcome:#?}");
    assert!(outcome.stderr_tail.len() <= 50);
    let stderr_events = events.iter().filter(|e| matches!(e, AdapterEvent::Stderr { .. })).count();
    assert!(stderr_events >= 500, "stderr is streamed, not dropped: {stderr_events}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_impostor_and_unsupported_binaries_never_run() {
    // Nothing on PATH.
    let h = Harness::new(&["nonexistent-dir"]);
    let (events, outcome) = h.run(h.request(Vendor::Claude, "ok")).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::NotInstalled { vendor: Vendor::Claude } }));
    assert!(!has(&events, |e| matches!(e, AdapterEvent::Started { .. })));

    // A generic `agent` on PATH is not Cursor and must not be executed as a run.
    let h = Harness::new(&["impostor"]);
    let (_, outcome) = h.run(h.request(Vendor::Cursor, "ok")).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::NotInstalled { vendor: Vendor::Cursor } }), "{outcome:#?}");
    let calls = h.state_file("calls.impostor");
    assert!(!calls.contains("-p"), "impostor was invoked as a run: {calls}");

    // A verified binary with a version below the pin fails closed before spawning.
    let mut h = Harness::new(&["bin"]);
    h.supervisor = Supervisor::new(SupervisorConfig {
        support: SupportMatrix {
            codex_min: (99, 0, 0),
            ..SupportMatrix::default()
        },
        ..h.supervisor.config().clone()
    });
    let (_, outcome) = h.run(h.request(Vendor::Codex, "ok")).await;
    assert!(matches!(outcome.status, RunStatus::Failed { reason: FailureReason::UnsupportedVersion { .. } }), "{outcome:#?}");
    assert!(h.run_args(Vendor::Codex).is_empty(), "codex must not have been run");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_never_receive_credentials() {
    // SAFETY: unique names for this test; other tests read env only through `minimal_env`.
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "sk-canary");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-canary-anthropic");
        std::env::set_var("CURSOR_API_KEY", "canary-cursor");
        std::env::set_var("CODEX_API_KEY", "canary-codex");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "canary-oauth");
        std::env::set_var("XAI_API_KEY", "canary-xai");
        std::env::set_var("WORKSHOP_SECRET_THING", "canary-workshop");
    }
    let h = Harness::new(&["bin"]);
    for vendor in Vendor::ALL {
        let (_, outcome) = h.run(h.request(vendor, "ok")).await;
        assert_eq!(outcome.status, RunStatus::Completed);
        let env = h.state_file(&format!("run-env.{}", vendor.id()));
        assert!(!env.contains("canary"), "{vendor:?} child saw a credential:\n{env}");
        assert!(!env.contains("_API_KEY="), "{env}");
        assert!(!env.contains("_TOKEN="), "{env}");
        assert!(env.contains("PATH="));
    }

    // Callers cannot smuggle a credential through extra_env either.
    let mut req = h.request(Vendor::Claude, "ok");
    req.extra_env.push((OsString::from("MY_VENDOR_API_KEY"), OsString::from("x")));
    assert!(matches!(h.supervisor.start(req), Err(workshop_adapters::AdapterError::Env(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_write_profile_maps_to_vendor_controls() {
    let h = Harness::new(&["bin"]);
    for vendor in Vendor::ALL {
        let req = h.request(vendor, "ok").with_permissions(PermissionProfile::WorkspaceWrite);
        let (_, outcome) = h.run(req).await;
        assert_eq!(outcome.status, RunStatus::Completed);
        let args = h.run_args(vendor);
        match vendor {
            Vendor::Claude => assert!(args.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"])),
            Vendor::Codex => assert!(args.windows(2).any(|w| w == ["--sandbox", "workspace-write"])),
            Vendor::Cursor => {
                assert!(args.contains(&"--force".to_string()));
                assert!(args.windows(2).any(|w| w == ["--sandbox", "enabled"]));
            }
            Vendor::OpenCode => {
                let env = h.state_file("run-env.opencode");
                assert!(env.contains(r#""edit":"allow""#) && env.contains(r#""bash":"deny""#), "{env}");
            }
        }
        // Never the dangerous bypass flags.
        assert!(!args.iter().any(|a| a.contains("dangerously") || a == "--yolo" || a == "--full-auto" || a == "--auto"), "{vendor:?}: {args:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_validates_request_before_spawning() {
    let h = Harness::new(&["bin"]);
    let mut req = h.request(Vendor::Claude, "ok");
    req.prompt = "   ".into();
    assert!(matches!(h.supervisor.start(req), Err(workshop_adapters::AdapterError::EmptyPrompt)));

    let mut req = h.request(Vendor::Claude, "ok");
    req.workdir = Workdir::in_place_acknowledged("/definitely/not/here");
    assert!(matches!(h.supervisor.start(req), Err(workshop_adapters::AdapterError::MissingWorkdir(_))));
}
