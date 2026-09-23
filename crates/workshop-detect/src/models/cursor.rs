//! Cursor Agent: `cursor-agent models`, "List available models for this account"
//! (<https://cursor.com/docs/cli/reference/parameters>). Text, not JSON; the format is pinned from
//! Cursor Agent 2026.09.18-9a7762b (`src/commands/models.ts` in the installed CLI bundle):
//!
//! ```text
//! Available models
//!
//! <id> - <name> (current, default)
//! …
//!
//! Tip: use --model <id> (or /model <id> in interactive mode) to switch. …
//! ```
//!
//! The name and the `(current, default)` flags are each optional. An account without models
//! prints `No models available for this account.`; signed out it exits 1 with
//! `Error: Authentication required. …` (captured on the real CLI), an expired login with
//! `Authentication failed: …`, anything else with `Failed to load models: …`.

use std::ffi::OsString;
use std::path::Path;

use super::{Answer, ModelsError, SubscriptionModel};
use crate::locate::DetectConfig;
use crate::model::Vendor;
use crate::process::{self, ChildOutput};

pub(crate) const ARGS: &[&str] = &["models"];

pub(super) fn fetch(
    bin: &Path,
    cwd: Option<&Path>,
    env: &[(OsString, OsString)],
    cfg: &DetectConfig,
) -> Result<Answer, ModelsError> {
    let out = process::run_vendor(
        Vendor::Cursor,
        bin,
        ARGS,
        cwd,
        env,
        cfg.models_timeout,
        cfg.kill_grace(Vendor::Cursor),
    )?;
    interpret(&out)
}

pub(crate) fn interpret(out: &ChildOutput) -> Result<Answer, ModelsError> {
    if out.timed_out {
        return Err(ModelsError::TimedOut);
    }
    let text = process::strip_ansi(&format!("{}\n{}", out.stdout, out.stderr));
    let first = || {
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_owned()
    };
    if text.contains("Authentication required") || text.contains("Authentication failed") {
        return Err(ModelsError::NotLoggedIn(first()));
    }
    if out.code != Some(0) {
        let line = first();
        return Err(ModelsError::Failed(if line.is_empty() {
            format!("cursor-agent models exited {:?}", out.code)
        } else {
            line
        }));
    }
    let answer = |models| Answer {
        models,
        account: None,
        documented_aliases: false,
    };
    if text.contains("No models available for this account") {
        return Ok(answer(Vec::new()));
    }
    let mut lines = text.lines().map(str::trim);
    if !lines.by_ref().any(|l| l == "Available models") {
        return Err(ModelsError::Failed(
            "cursor-agent models printed no model list".into(),
        ));
    }
    Ok(answer(
        lines
            .take_while(|l| !l.starts_with("Tip:"))
            .filter(|l| !l.is_empty())
            .map(parse_line)
            .collect(),
    ))
}

/// `<id> - <name> (current, default)`; ids never contain whitespace.
fn parse_line(line: &str) -> SubscriptionModel {
    let (id, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(id, rest)| (id, rest.trim()));
    let mut flags: Vec<&str> = Vec::new();
    let mut name = rest;
    if let Some(open) = rest.rfind('(')
        && rest.ends_with(')')
    {
        let inner: Vec<&str> = rest[open + 1..rest.len() - 1]
            .split(',')
            .map(str::trim)
            .collect();
        if inner.iter().all(|f| *f == "current" || *f == "default") {
            flags = inner;
            name = rest[..open].trim_end();
        }
    }
    let name = name.strip_prefix('-').unwrap_or(name).trim();
    SubscriptionModel {
        id: id.to_owned(),
        label: if name.is_empty() { id } else { name }.to_owned(),
        is_default: flags.contains(&"default"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, stdout: &str, stderr: &str) -> ChildOutput {
        ChildOutput {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            timed_out: false,
        }
    }

    #[test]
    fn listing_in_the_bundle_format() {
        let listing = "\u{1b}[2mAvailable models\u{1b}[22m\n\n\u{1b}[32mauto\u{1b}[39m \u{1b}[2m- Auto\u{1b}[22m\u{1b}[2m (current)\u{1b}[22m\ncomposer-2.5 - Composer 2.5 (default)\ngpt-6-sol - GPT-6 Sol\nclaude-opus-5-5-thinking - Claude Opus 5.5 (Thinking)\nbare-id\n\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\n";
        let a = interpret(&out(0, listing, "")).unwrap();
        let rows: Vec<(&str, &str, bool)> = a
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.label.as_str(), m.is_default))
            .collect();
        assert_eq!(
            rows,
            [
                ("auto", "Auto", false),
                ("composer-2.5", "Composer 2.5", true),
                ("gpt-6-sol", "GPT-6 Sol", false),
                (
                    "claude-opus-5-5-thinking",
                    "Claude Opus 5.5 (Thinking)",
                    false
                ),
                ("bare-id", "bare-id", false),
            ]
        );
    }

    #[test]
    fn real_signed_out_output_is_not_logged_in() {
        let err = interpret(&out(
            1,
            "",
            "Error: Authentication required. Run 'agent login', pass --api-key/--auth-token, or set CURSOR_API_KEY/CURSOR_AUTH_TOKEN.\n",
        ))
        .unwrap_err();
        assert!(
            matches!(err, ModelsError::NotLoggedIn(ref m) if m.starts_with("Error: Authentication required.")),
            "{err:?}"
        );
        assert!(matches!(
            interpret(&out(
                1,
                "",
                "Authentication failed: your Cursor credentials or API key are invalid or expired."
            )),
            Err(ModelsError::NotLoggedIn(_))
        ));
    }

    #[test]
    fn empty_failed_and_garbled_outputs() {
        let empty = interpret(&out(0, "No models available for this account.\n", "")).unwrap();
        assert!(empty.models.is_empty());
        assert_eq!(
            interpret(&out(1, "", "Failed to load models: network down")).unwrap_err(),
            ModelsError::Failed("Failed to load models: network down".into())
        );
        assert!(matches!(
            interpret(&out(0, "something else", "")),
            Err(ModelsError::Failed(_))
        ));
        let mut timed_out = out(0, "", "");
        timed_out.timed_out = true;
        assert_eq!(interpret(&timed_out).unwrap_err(), ModelsError::TimedOut);
    }
}
