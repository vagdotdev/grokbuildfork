//! Real-binary checks. Each test skips (with a note) when the vendor CLI is
//! not installed, so the suite stays green on a bare machine and becomes
//! stronger where the CLIs exist. Login cannot be completed here; the point is
//! that identity, version, and the vendor's *own* logged-out answer are read
//! correctly — never a credential file.
//!
//! Run with `--nocapture` to see what was found.

#![cfg(unix)]

use std::time::Duration;

use workshop_adapters::vendors;
use workshop_adapters::{
    AdapterId, DetectConfig, Detection, PinStatus, RunOutcome, RunRequest, SupervisorOptions,
    detect, spawn,
};
use workshop_detect::{LoginState, probe_vendor};

async fn real(id: AdapterId) -> Option<(workshop_adapters::InstalledCli, LoginState)> {
    let adapter = vendors::by_id(id);
    let cfg = DetectConfig {
        timeout: Duration::from_secs(60),
        ..DetectConfig::default()
    };
    match detect(adapter.as_ref(), &cfg).await {
        Detection::NotInstalled => {
            eprintln!("[real_cli] {id}: not installed, skipping");
            None
        }
        Detection::Unverified { path, reason } => {
            eprintln!(
                "[real_cli] {id}: `{}` present but unverified ({reason}); skipping",
                path.display()
            );
            None
        }
        Detection::Installed(cli) => {
            let state = probe_vendor(id, &cfg)
                .login
                .expect("an installed CLI has a login state");
            let pin = adapter.version_pin().classify(&cli.version);
            eprintln!(
                "[real_cli] {id}: {} version {} ({pin:?}) login={state:?}",
                cli.path.display(),
                cli.version,
            );
            assert!(!cli.version.is_empty());
            assert!(
                !matches!(state, LoginState::Unknown { .. }),
                "{id}: vendor status output not understood: {state:?}"
            );
            if pin != PinStatus::Tested {
                eprintln!(
                    "[real_cli] {id}: version {} is outside the tested pin ({pin:?})",
                    cli.version
                );
            }
            Some((cli, state))
        }
    }
}

#[tokio::test]
async fn claude_real_detection_and_login_state() {
    let Some((cli, state)) = real(AdapterId::Claude).await else {
        return;
    };
    if state != LoginState::LoggedOut {
        eprintln!("[real_cli] claude is logged in here; skipping the logged-out spawn check");
        return;
    }
    // Logged out, the real CLI still accepts the pinned flags and reports the
    // failure through its own stream; the run must fail closed.
    let tmp = tempfile::tempdir().unwrap();
    let run = spawn(
        vendors::by_id(AdapterId::Claude).as_ref(),
        &cli,
        RunRequest::new("say hi", tmp.path()),
        &SupervisorOptions::default(),
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(90), run.wait())
        .await
        .unwrap();
    eprintln!("[real_cli] claude logged-out run: {outcome:?}");
    match outcome {
        RunOutcome::Failed {
            reason,
            stderr_tail,
        } => {
            let all = format!("{reason}\n{stderr_tail}").to_lowercase();
            assert!(
                !all.contains("unknown option") && !all.contains("unknown argument"),
                "flag drift: {all}"
            );
            assert!(
                all.contains("log"),
                "expected a login-related failure, got: {all}"
            );
        }
        other => panic!("logged-out claude must not succeed: {other:?}"),
    }
}

#[tokio::test]
async fn codex_real_detection_and_login_state() {
    // `codex exec` logged out retries the API for ~15s before `turn.failed`;
    // detection + status is enough here.
    let _ = real(AdapterId::Codex).await;
}

#[tokio::test]
async fn cursor_real_detection_and_login_state() {
    let Some((cli, state)) = real(AdapterId::Cursor).await else {
        return;
    };
    if state != LoginState::LoggedOut {
        eprintln!("[real_cli] cursor-agent is logged in here; skipping the logged-out spawn check");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let run = spawn(
        vendors::by_id(AdapterId::Cursor).as_ref(),
        &cli,
        RunRequest::new("say hi", tmp.path()),
        &SupervisorOptions::default(),
    )
    .await
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(90), run.wait())
        .await
        .unwrap();
    eprintln!("[real_cli] cursor-agent logged-out run: {outcome:?}");
    match outcome {
        RunOutcome::Failed {
            reason,
            stderr_tail,
        } => {
            let all = format!("{reason}\n{stderr_tail}").to_lowercase();
            assert!(
                !all.contains("unknown option") && !all.contains("too many arguments"),
                "flag drift: {all}"
            );
            assert!(
                all.contains("authentication required") || all.contains("login"),
                "{all}"
            );
        }
        other => panic!("logged-out cursor-agent must not succeed: {other:?}"),
    }
}

#[tokio::test]
async fn opencode_real_detection_and_login_state() {
    // `opencode run` without credentials may still reach free models over the
    // network; detection + status only.
    let _ = real(AdapterId::OpenCode).await;
}
