# Workshop

Workshop is a personal coding-agent runtime built as a thin **overlay** on the public
[Grok Build](https://github.com/xai-org/grok-build) tree. A user who has never heard of xAI can
install it, pick a local model, an API key, or an already-installed subscription CLI, and work.
Login never opens `auth.x.ai` unless the user explicitly chooses the optional, labeled xAI card.

The authoritative plan is `docs/workshop-production-plan.md` in the project store; the decisions are
recorded in `docs/workshop/adr/`.

## Layout

| Path | Role |
|---|---|
| `crates/codegen/xai-grok-*`, `crates/common/xai-*` | Upstream crates, **never renamed**; a handful carry small patches |
| `crates/workshop-auth` | Connection picker policy (classes, cards, rails, presence-only CLI detection, text renderer) |
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
| `.github/workflows/ci.yml` | Three jobs: **fmt + check (overlay)**, **no-xai gates** (required; source/binary scans, overlay + touched-upstream tests, full pager lib suite, no-egress + no-theft-fs-audit, PTY picker + rails smokes), **voice-engine** (whisper.cpp helper build + model probe) |

## Build and run

```sh
cargo build -p xai-grok-pager-bin --bin workshop     # protoc 29.3 required (bin/protoc via dotslash)
target/debug/workshop                                # TUI; first run opens the connection picker
target/debug/workshop login                          # the picker as text
target/debug/workshop login --xai                    # optional xAI account login only (opens auth.x.ai)
```

Home is `~/.workshop` (`$WORKSHOP_HOME`; `GROK_HOME` is accepted as a warned alias for now).
Telemetry is off and no Mixpanel token or events URL is baked in. Background auto-update is off
until a Workshop release channel and signature verification exist (`WORKSHOP_ENABLE_AUTOUPDATE=1`
opts in); `workshop update` reads the channel manifest from the release repository baked at build
time (`WORKSHOP_RELEASE_REPO`).

## Gates

```sh
cargo test -p workshop-gates                          # compiled defaults: issuer, auth methods, endpoints, updater, model
scripts/no-xai-scan.sh --sources                      # default-path sources + gate:no-theft markers
scripts/no-xai-scan.sh --binary target/debug/workshop # forbidden strings vs scripts/no-xai-binary-baseline.txt
scripts/no-egress-smoke.sh target/debug/workshop      # zero xAI egress on startup, login, headless, first-run TUI
WORKSHOP_BIN=$PWD/target/debug/workshop cargo test -p workshop-gates --test pty_login_picker -- --include-ignored
```

## Editing an upstream file

Edit the file in place (the tree keeps the patched state), then run `scripts/regenerate-patches.sh`
and commit `patches/` together with the change. Add new upstream paths to the right group in
`patches/groups.txt`. Gate-tagged patches are never commented out of `patches/series`.
