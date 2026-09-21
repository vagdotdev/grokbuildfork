# ADR 0003 — Spawn official CLIs; never read another app's credentials

Status: accepted (M0). Owner: Workshop. Enforced by `gate:no-theft`.

## Decision

Subscriptions are used by spawning the vendor's official CLI (`claude`, `codex`, `cursor-agent`,
`opencode`) with documented flags. Workshop never holds the vendor's credential.

Forbidden, and failed by the gate if they appear in `crates/`:

- `provider_autodock.rs` (July dock): copying OpenCode `~/.local/share/opencode/auth.json` and
  Codex `~/.codex/auth.json` into a vault. Not ported. Deleted if ever replayed.
- The Blackpen export's probes: `security find-generic-password -s "Claude Code-credentials"`,
  parsing OpenCode / Codex `auth.json`, reading `~/.cursor/sdk/auth.json`.
- Bundling `opencode-with-claude`, pointing Anthropic at `http://127.0.0.1:3456` with a dummy key,
  or capturing `claude.com` / `anthropic.com` OAuth codes in an in-app window.
- Community Claude Pro/Max OAuth plugins. Claude = official `claude` CLI or an Anthropic Console key.
- Treating a bare `agent` binary as Cursor. Only `cursor-agent` counts.

Detection is presence-only: exact binary name on `PATH` and a fixed list of install directories.
Login for an adapter runs the official CLI's login in the user's terminal; Workshop re-probes after
it exits and never claims success from a file's existence.

## Enforcement

`scripts/no-xai-scan.sh --sources` greps `crates/` for the markers in
`workshop_gates::THEFT_MARKERS`; `crates/workshop-auth` is additionally checked for any `auth.json`
/ keychain access. Milestone E adds a filesystem audit of adapter runs.
