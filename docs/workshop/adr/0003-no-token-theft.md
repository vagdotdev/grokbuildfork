# ADR 0003 — Spawn official CLIs; never read another app's credentials

Status: accepted (M0). Enforced by `gate:no-theft`.

## Context

The July dock (`provider_autodock.rs` on `main`) copied OpenCode
`~/.local/share/opencode/auth.json` and Codex `~/.codex/auth.json` (API key and
ChatGPT OAuth) into a local vault. The Blackpen picker export that defines our
UX checks the macOS keychain item `Claude Code-credentials`, parses OpenCode and
Codex `auth.json`, reads `~/.cursor/sdk/auth.json`, and routes Claude through a
bundled `opencode-with-claude` proxy with an in-app OAuth capture window.
Anthropic prohibits Claude Pro/Max OAuth reuse; the others are foreign tokens.

## Decision

- Subscriptions are agent adapters: Workshop detects the official binary
  (`claude`, `codex`, `cursor-agent`, `opencode`; a bare `agent` is not
  Cursor), attaches the vendor's own login command to the user's terminal, and
  later spawns the documented non-interactive run. Login status comes from the
  vendor CLI's status command, never from a file's existence.
- Forbidden, checked by `crates/workshop-gates/tests/gate5_no_theft.rs` and
  `scripts/no-theft-fs-audit.sh`: opening `~/.codex/auth.json`, OpenCode
  `auth.json`, the Claude Code keychain item, `~/.cursor/sdk/auth.json`, Cursor
  app databases, or browser cookies; bundling `opencode-with-claude`,
  `http://127.0.0.1:3456` with a dummy key, or a Claude/Anthropic OAuth
  capture; replaying another app's refresh token; porting `provider_autodock.rs`.
- Detection is presence-only (`PATH`, known install dirs, env-var presence).
  Key values are never printed or copied until the user chooses to use them.
- `workshop auth logout <connection>` calls the vendor CLI's logout or deletes
  Workshop's own secret; it never removes another app's files.

## Consequences

- A pasted Cursor key is a Direct API connection, not a reason to read Cursor's
  auth file.
- Claude means the official `claude` CLI or an Anthropic Console API key.
