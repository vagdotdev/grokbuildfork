//! gate:config-isolation — Workshop reads `$WORKSHOP_HOME` and nothing else. A machine that also
//! runs Grok Build (a populated `~/.grok`, `GROK_HOME` exported) or Claude Code (hooks in
//! `~/.claude/settings.json`) must not have those hooks executed, settings read, or sessions
//! listed by Workshop. The user hit exactly this: a `session_start hook (global/settings)` from
//! another tool's settings failing inside Workshop.
//!
//! The PTY test is opt-in (`WORKSHOP_BIN`, `--include-ignored`); the compiled-default checks run
//! always.

mod pty_common;

use std::path::Path;
use std::time::Duration;

use pty_common::*;

fn session_start_hook(marker: &Path) -> String {
    format!(
        r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"touch {}"}}]}}]}}}}"#,
        marker.display()
    )
}

/// Compiled defaults: another tool's hooks, MCP servers and sessions are opt-in, never default.
#[test]
fn foreign_executing_surfaces_default_off() {
    use xai_grok_tools::types::compat::CompatConfig;
    let c = CompatConfig::default();
    for (vendor, v) in [
        ("claude", c.claude),
        ("cursor", c.cursor),
        ("codex", c.codex),
    ] {
        assert!(!v.hooks, "{vendor}.hooks must default off");
        assert!(!v.mcps, "{vendor}.mcps must default off");
        assert!(!v.sessions, "{vendor}.sessions must default off");
        assert!(
            v.skills && v.rules && v.agents,
            "{vendor}: read-only project context stays on"
        );
    }
}

/// The home resolver never reaches `~/.grok`: with `WORKSHOP_HOME` unset the default is
/// `~/.workshop`, and the product binary strips `GROK_HOME` before resolving (PTY test below).
#[test]
fn default_home_is_workshop_not_grok() {
    let home = xai_dirs::default_grok_home();
    assert!(home.ends_with(".workshop"), "{}", home.display());
    assert!(
        !home.to_string_lossy().contains(".grok"),
        "{}",
        home.display()
    );
}

#[test]
#[ignore = "needs WORKSHOP_BIN (built workshop binary); run with --include-ignored"]
fn populated_grok_home_and_claude_settings_are_ignored_but_workshop_hooks_run() {
    let Some(bin) = bin_from_env() else { return };
    // The engine is irrelevant here; a fast-failing fake keeps the picker step hermetic.
    let fake = fake_opencode("crash");
    let markers = tempfile::tempdir().unwrap();
    let grok_marker = markers.path().join("grok-hook-ran");
    let claude_marker = markers.path().join("claude-hook-ran");
    let workshop_marker = markers.path().join("workshop-hook-ran");

    // Prepare the HOME before Workshop starts: a Grok Build home (also pointed to by
    // `GROK_HOME`), Claude Code settings with a hook, and the positive control under
    // `$WORKSHOP_HOME/hooks`. Hooks fire at session creation, which type-and-go does at once.
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path().to_path_buf();
    let grok_home = home.join(".grok");
    std::fs::create_dir_all(grok_home.join("hooks")).unwrap();
    std::fs::write(
        grok_home.join("hooks").join("startup.json"),
        session_start_hook(&grok_marker),
    )
    .unwrap();
    std::fs::write(
        grok_home.join("config.toml"),
        "[models]\ndefault = \"grok-4.6\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude").join("settings.json"),
        session_start_hook(&claude_marker),
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".workshop").join("hooks")).unwrap();
    std::fs::write(
        home.join(".workshop").join("hooks").join("startup.json"),
        session_start_hook(&workshop_marker),
    )
    .unwrap();
    let grok_home_s = grok_home.to_string_lossy().to_string();
    let mut j = spawn_in(
        "config-isolation",
        &bin,
        &[
            ("GROK_HOME", grok_home_s.as_str()),
            // Even an explicit upstream opt-in env var must not pull Claude hooks in.
            ("GROK_CLAUDE_HOOKS_ENABLED", "1"),
        ],
        Some(fake.path()),
        home_dir,
    );

    connect_big_pickle(&mut j);
    // Session created; hooks have fired by the time the composer is up. Give the positive
    // control a moment, then check.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !workshop_marker.exists() && std::time::Instant::now() < deadline {
        j.h.update(Duration::from_millis(250));
    }
    snapshot(&j.h, &j.dir, "composer");
    assert!(
        workshop_marker.exists(),
        "positive control: a SessionStart hook under $WORKSHOP_HOME/hooks runs\n{}",
        j.h.screen_contents()
    );
    j.h.update(Duration::from_millis(1500));
    assert!(
        !grok_marker.exists(),
        "a hook from ~/.grok (or $GROK_HOME) must never run inside Workshop"
    );
    assert!(
        !claude_marker.exists(),
        "a hook from ~/.claude/settings.json must never run inside Workshop by default"
    );
    let screen = j.h.screen_contents();
    assert!(
        !screen.contains("hook (global/settings)"),
        "no foreign settings hook is even attempted:\n{screen}"
    );
    // Workshop's own config lives under $WORKSHOP_HOME; the Grok Build config.toml was not read.
    assert!(j.workshop_home().join("config.toml").is_file());
    assert!(!screen.contains("grok-4.6"), "{screen}");
}
