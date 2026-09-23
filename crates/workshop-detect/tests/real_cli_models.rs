//! The model probes against the real official CLIs on this machine (installed with the vendors'
//! own installers). Ignored by default; `cargo test -p workshop-detect --test real_cli_models --
//! --ignored --nocapture`. Holds signed in or signed out: a signed-in CLI must list models, a
//! signed-out Codex / Cursor must say so, and Claude answers either way.

use workshop_detect::{
    DetectConfig, LoginState, ModelsError, Rail, probe_vendor, subscription_models,
};

#[test]
#[ignore = "needs the real Claude Code / Codex / Cursor Agent CLIs on PATH; run with --ignored"]
fn real_clis_list_models_or_say_signed_out() {
    let cfg = DetectConfig::default();
    for rail in Rail::ALL {
        let vp = probe_vendor(rail.vendor(), &cfg);
        let Some(id) = &vp.binary else {
            eprintln!("{rail:?}: not installed; skipped");
            continue;
        };
        let started = std::time::Instant::now();
        let answer = subscription_models(rail, &id.path, &cfg);
        eprintln!(
            "{rail:?} {} login={:?} in {:?}: {answer:?}",
            id.version,
            vp.login,
            started.elapsed()
        );
        match (rail, &vp.login, answer) {
            (_, _, Ok(list)) => {
                assert!(!list.models.is_empty(), "{rail:?}");
                assert!(list.models.iter().filter(|m| m.is_default).count() <= 1);
                assert!(!list.documented_aliases, "{rail:?}: handshake unsupported");
            }
            (
                Rail::Codex | Rail::Cursor,
                Some(LoginState::LoggedOut),
                Err(ModelsError::NotLoggedIn(_)),
            ) => {}
            (_, login, Err(e)) => panic!("{rail:?} ({login:?}): {e}"),
        }
    }
}
