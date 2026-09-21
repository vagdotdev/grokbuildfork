# ADR 0002 — Three connection classes, one picker, no default provider

Status: accepted (M0/A).

## Context

Upstream has one auth surface: an xAI OAuth2 browser flow advertised as the
`grok.com` ACP method, reached from cold start, welcome `l`, `/login`, and
`grok login`. A user who has never heard of xAI cannot get past it.

## Decision

Every connection is one of three classes and is always labeled as such:

| Class | Workshop owns | Credential |
|---|---|---|
| Direct API | prompt, tools, loop, HTTP | a key issued for API use (OpenAI, Anthropic, OpenRouter, custom, optional xAI) |
| Local | the same, on loopback | none / local token (Ollama, LM Studio, llama.cpp, vLLM) |
| Agent adapter | process supervision + normalized events | the vendor CLI's own login; Workshop never holds it (`claude`, `codex`, `cursor-agent`, `opencode`) |

Login is a connection picker, never a provider. It follows the Blackpen
picker UX (`Models` / `Subscriptions` tabs; rails Claude, Codex, Cursor with
Detecting / Ready / Sign in pills; per-rail model radios; XML empty copy).
The optional xAI card is last on the Models tab, labeled
"Uses xAI accounts and auth.x.ai. Not required.", never preselected, and the
only way to reach the inherited OIDC flow. Choosing it records an explicit
opt-in marker (`$WORKSHOP_HOME/workshop-connections.toml`); only that marker
lets `GrokComConfig::default()` construct the xAI issuer.

`build_auth_methods` advertises `workshop.connect` where upstream advertised
`grok.com`. The agent fails closed for it ("No connection configured");
the pager opens the picker for it. Enterprise IdPs (`GROK_OIDC_*`) and external
auth-provider commands are explicit deployment configuration and keep working.

## Consequences

- No cold-start path calls `dispatch_login → "grok.com"`.
- Headless / ACP clients without a configured connection get an error, not a
  browser.
- Direct/Local rows currently hand the user the `[model.*]` TOML that upstream's
  BYOK path already honours; provider manifests, keyring storage, and adapter
  spawning arrive in milestones C–E.
