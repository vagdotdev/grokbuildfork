# ADR 0004 — Telemetry, updater, and other inherited services stay off

Status: accepted (M0/B).

## Context

Upstream bakes product-event and Mixpanel sinks through
`GROK_TELEMETRY_BUILD_*` at compile time, auto-updates from `https://x.ai/cli`
/ a Grok GCS bucket / `@xai-official/grok`, and points announcements,
feedback, changelog, remote share, and crash upload at xAI infrastructure.

## Decision

- Telemetry: no `GROK_TELEMETRY_BUILD_EVENTS_URL`, `_EVENTS_API_KEY`, or
  `_MIXPANEL_TOKEN` is honoured at build time (patch 0007 neutralises the
  `option_env!` layer); runtime default remains disabled; the user may still
  point `GROK_TELEMETRY_*` / `[features] telemetry` at their own collector.
- Updater: `WORKSHOP_AUTO_UPDATE_ENABLED = false`; every update entry point
  reports "auto-update is disabled in this Workshop build" and makes no
  request. Channel constants point at `workshop.invalid` placeholders until a
  signed Workshop channel exists (milestone F).
- Production endpoints: `PRODUCTION_ENDPOINTS`, the shell's auxiliary API
  default, and the computer-hub URL point at reserved `.invalid` hosts so an
  unconfigured code path fails at DNS instead of reaching x.ai / grok.com.
- Inherited announcements, feedback upload, remote share, and crash upload stay
  off until Workshop owns them.

## Consequences

- `cargo test -p workshop-gates` (gates 3, 4, 6) pins each of these.
- Offline start must make no network call once the remaining startup fetches
  are audited (milestone B exit); the hermetic egress log in the PR shows what
  is still attempted against the placeholders.
