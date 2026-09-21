# ADR 0004 — Telemetry, updater and inherited services stay off

Status: accepted (M0/A). Owner: Workshop.

## Decision

- **Telemetry off.** Workshop builds never set `GROK_TELEMETRY_BUILD_EVENTS_URL`,
  `GROK_TELEMETRY_BUILD_EVENTS_API_KEY` or `GROK_TELEMETRY_BUILD_MIXPANEL_TOKEN`, so
  `TelemetryConfig::default()` has no baked endpoint or token and the inherited
  `resolve_telemetry_mode` stays `Disabled`. CI fails if a workflow sets any of them.
- **Background auto-update off.** `should_check_for_updates` returns false unless
  `WORKSHOP_ENABLE_AUTOUPDATE` is truthy, and stays so while the release repository is private.
  The updater itself is repointed per `internal/updater-repoint-spec.md`: `RELEASE_REPO` is baked
  from `WORKSHOP_RELEASE_REPO` at build time (default `vagdotdev/grokbuildfork`),
  `CHANNEL_BASE_URL` is `https://raw.githubusercontent.com/<RELEASE_REPO>/release-channel`, the
  channel pointer is the JSON manifest `<channel>.json` (`scripts/release/channel-manifest.schema.json`,
  artifacts keyed `<os>-<arch>`), downloads are SHA-256-verified `.tar.gz` assets extracted to
  `~/.workshop/downloads/workshop-<version>-<platform>` behind the atomic `~/.workshop/bin/workshop`
  symlink that `scripts/install.sh` also writes. npm is unsupported; there is no GCS fallback;
  nothing names `x.ai/cli`, `@xai-official/grok` or `xai-org-shared/grok-build`. Re-enabling
  background updates requires milestone F: signature verification, downgrade protection, rollback drill.
- **Production endpoints are loopback.** `PRODUCTION_ENDPOINTS` (proxy, assets, relay, gateway,
  origin) point at `127.0.0.1:1`, so every inherited auxiliary call (model list, managed config,
  feedback, trace upload, announcements, relay) fails fast without resolving a name. Operators may
  override via `GROK_PRODUCTION_*` for their own infrastructure.
- Inherited announcements, feedback upload, remote share, crash upload stay off until Workshop owns
  them.

## Evidence

`scripts/no-xai-scan.sh --binary target/debug/workshop` counts the forbidden strings in the built
binary against `scripts/no-xai-binary-baseline.txt`; the CI `no-egress` smoke runs the binary in a
network namespace with a logging proxy and fails on any `*.x.ai` / `*.grok.com` /
`api.mixpanel.com` / `storage.googleapis.com` request.
