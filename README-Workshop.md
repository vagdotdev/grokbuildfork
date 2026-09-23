# Workshop

Workshop is a personal coding-agent runtime built as a thin **overlay** on the public
[Grok Build](https://github.com/xai-org/grok-build) tree. A user who has never heard of xAI can
install it and start typing: the first run lands in the composer with the OpenCode engine's free
default model active; `/model` switches models (Kilo pool, local servers, connected providers) and
`/auth` connects an installed subscription CLI or an API key. Login never opens `auth.x.ai` unless
the user explicitly chooses the optional, labeled xAI card (last on `/auth`).

The authoritative plan is `docs/workshop-production-plan.md` in the project store; the decisions are
recorded in `docs/workshop/adr/`.

## Layout

| Path | Role |
|---|---|
| `crates/codegen/xai-grok-*`, `crates/common/xai-*` | Upstream crates, **never renamed**; a handful carry small patches |
| `crates/workshop-auth` | `/model` + `/auth` picker policy (rows, rails, xAI card last, text renderer); the TUI overlay is `xai-grok-pager/src/views/connection_picker.rs` |
| `crates/workshop-gates` | No-xAI gates 1–4 and the picker policy as `cargo test`; PTY smoke of the built binary |
| `crates/workshop-adapters` | Vendor CLI adapters (Claude/Codex/Cursor) + the OpenCode engine (`opencode serve`) |
| `crates/workshop-providers`, `-detect`, `-brand`, `-voice` | Provider catalog + credential broker; presence-only CLI detection; welcome hero art; local voice STT |
| `patches/series`, `patches/*.patch` | Quilt series over upstream files; tags `gate:no-xai` / `gate:no-theft` / `product` / `branding` |
| `patches/groups.txt`, `scripts/regenerate-patches.sh` | Patch file ↔ upstream path groups; regenerates the series from the tree |
| `scripts/no-xai-scan.sh` | Default-path source scan and binary string scan (`--sources`, `--binary BIN`) |
| `scripts/no-egress-smoke.sh` | Startup + Login in a network namespace with hostname logging; fails on any xAI host |
| `scripts/overlay-paths.txt`, `upstream-lock.toml` | What the upstream sync preserves, and which snapshot the tree is on |
| `scripts/sync/`, `.github/workflows/sync-upstream.yml` | Upstream auto-sync |
| `scripts/install.sh`, `scripts/release/`, `.github/workflows/release.yml` | Release pipeline (CLI installer + voice-engine helper and pinned Whisper model) |
| `scripts/smoke/first-run.py`, `.github/workflows/first-run-smoke.yml` | After each release: public one-liner → `workshop` → one sentence → reply → `/exit` in a PTY on macOS arm64, macOS Intel and Linux |
| `.github/workflows/ci.yml` | Three jobs: **fmt + check (overlay)**, **no-xai gates** (required; source/binary scans, overlay + touched-upstream tests, full pager lib suite, no-egress + no-theft-fs-audit, PTY picker + rails smokes), **voice-engine** (whisper.cpp helper build + model probe) |

## Build and run

```sh
cargo build -p xai-grok-pager-bin --bin workshop     # protoc 29.3 required (bin/protoc via dotslash)
target/debug/workshop                                # TUI; first run lands in the composer (OpenCode · Big Pickle)
target/debug/workshop login                          # the /model + /auth lists as text
target/debug/workshop login --xai                    # optional xAI account login only (opens auth.x.ai)
```

Home is `~/.workshop` (`$WORKSHOP_HOME`). `GROK_HOME` and `~/.grok` are never read, so a machine that also runs Grok Build keeps its settings, hooks, sessions and memory separate.
The model lists on `/model` are live: the keyless catalogs (Kilo, OpenRouter, NVIDIA) are fetched
when `/model` opens (lists younger than 5 min are reused; `r` forces) and on a launch that already
has an active connection; the OpenCode rows are what the running `opencode serve` reports, fetched
on every engine start. Results are cached under `~/.workshop/catalog-cache/` so the next launch is
instant, and every row says `fetched <age>` or, offline, `cached list from <date>` (the compiled
seed). Nothing is fetched on a first run, by `/auth`, `/login` or `workshop login`.
Telemetry is off and no Mixpanel token or events URL is baked in. Updates install silently: at
launch Workshop reads the channel manifest (`stable.json` on the `release-channel` branch of the
release repository baked at build time, `WORKSHOP_RELEASE_REPO`) and, when it names a newer version,
a detached `workshop update` downloads the archive, checks its SHA-256 against the manifest,
smoke-runs the binary and swaps `~/.workshop/bin/workshop` atomically; the running session is left
alone, the welcome screen offers the restart, and the next launch says "Updated to <version>".
Offline or on any failure it only logs. `--no-auto-update`, `WORKSHOP_DISABLE_AUTOUPDATER=1` or
`[cli] auto_update = false` in `~/.workshop/config.toml` turn it off.

## Gates

```sh
cargo test -p workshop-gates                          # compiled defaults: issuer, auth methods, endpoints, updater, model
scripts/no-xai-scan.sh --sources                      # default-path sources + gate:no-theft markers
scripts/no-xai-scan.sh --binary target/debug/workshop # forbidden-host strings must be reviewed contexts (scripts/no-xai-binary-baseline.txt)
scripts/no-egress-smoke.sh target/debug/workshop      # zero xAI egress on startup, login, headless, first-run TUI (+ /auth); /model reaches only the catalog hosts
WORKSHOP_BIN=$PWD/target/debug/workshop cargo test -p workshop-gates --test pty_login_picker -- --include-ignored
WORKSHOP_BIN=$PWD/target/debug/workshop cargo test -p workshop-gates --test pty_live_catalogs -- --include-ignored  # no fetch before the user acts; /model lists the engine's live rows
```

## Editing an upstream file

Edit the file in place (the tree keeps the patched state), then run `scripts/regenerate-patches.sh`
and commit `patches/` together with the change. Add new upstream paths to the right group in
`patches/groups.txt`. Gate-tagged patches are never commented out of `patches/series`.
