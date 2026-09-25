# Workshop

Workshop is a terminal coding agent: it reads your codebase, runs shell commands, edits files and
tracks tasks, right in your terminal. It starts on a free model with nothing to configure; when you
want more, `/model` switches models and `/auth` connects a coding subscription (Claude Code, Codex,
Cursor) or an API key. Use it interactively as a full-screen TUI, headlessly for scripting and
CI/CD, or from an editor via the Agent Client Protocol (ACP).

## Install

macOS and Linux, one line:

```sh
curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh
```

The installer downloads the release for your platform, verifies its SHA-256 against the channel
manifest, installs `~/.workshop/bin/workshop`, adds that directory to your shell's `PATH` (your
`~/.zshrc`, `~/.bashrc` or fish config, once) and prints the line for the terminal you are in. It
makes no other network requests and sends no telemetry. Then open a new terminal and type:

```sh
workshop
```

You land in the composer with **Big Pickle**, OpenCode's free default model, active: type a
sentence and press Enter. `/model` switches models, `/auth` connects a subscription or an API key,
`/theme` picks a look (Oscura Midnight by default; Night, Day, Tokyo Night, Rose Pine Moon too),
`//` on an empty composer starts dictation and `//` again stops it. Updates install silently in the
background; `workshop update` forces one, `workshop --version` says what you have.

Pin a version with `WORKSHOP_VERSION=0.2.2 curl -fsSL … | sh`. Workshop keeps everything in
`~/.workshop` (`WORKSHOP_HOME`) and never reads another tool's settings, hooks or sessions.

**macOS:** the binary is not Apple-notarized. The installer clears the quarantine attribute; if
macOS still refuses to start it, run `xattr -d com.apple.quarantine ~/.workshop/bin/workshop`.

## What it is

Workshop is a thin **overlay** on the public [Grok Build](https://github.com/xai-org/grok-build)
tree (Apache-2.0): the upstream crates keep their names, Workshop adds `crates/workshop-*` and a
short, numbered patch series for the few upstream files that must change, and a daily sync brings
new upstream releases in. A user who has never heard of xAI can install it and start typing: login
never opens `auth.x.ai` unless the user explicitly chooses the optional, labeled xAI card (last on
`/auth`), no xAI endpoint is contacted by default, and telemetry is off. The decisions are recorded
in [`docs/workshop/adr/`](docs/workshop/adr/); the sync is described in
[`docs/upstream-sync.md`](docs/upstream-sync.md).

## Layout

| Path | Role |
|---|---|
| `crates/codegen/xai-grok-*`, `crates/common/xai-*` | Upstream crates, **never renamed**; a handful carry small patches |
| `crates/workshop-auth` | `/model` + `/auth` picker policy (rows, rails, xAI card last, text renderer); the TUI overlay is `xai-grok-pager/src/views/connection_picker.rs` |
| `crates/workshop-gates` | No-xAI gates 1–4, the theme catalog gate and the picker policy as `cargo test`; PTY smoke of the built binary |
| `crates/workshop-adapters` | Vendor CLI adapters (Claude/Codex/Cursor) + the OpenCode engine (`opencode serve`) |
| `crates/workshop-providers`, `-detect`, `-brand`, `-voice` | Provider catalog + credential broker; presence-only CLI detection; welcome hero art; local voice STT |
| `patches/series`, `patches/*.patch` | Quilt series over upstream files; tags `gate:no-xai` / `gate:no-theft` / `product` / `branding` |
| `patches/groups.txt`, `scripts/regenerate-patches.sh` | Patch file ↔ upstream path groups; regenerates the series from the tree |
| `scripts/no-xai-scan.sh` | Default-path source scan and binary string scan (`--sources`, `--binary BIN`) |
| `scripts/no-egress-smoke.sh` | Startup + Login in a network namespace with hostname logging; fails on any xAI host |
| `scripts/overlay-paths.txt`, `upstream-lock.toml` | What the upstream sync preserves (this README, `SECURITY.md` and `CONTRIBUTING.md` included), and which snapshot the tree is on |
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
cargo test -p workshop-gates                          # compiled defaults: issuer, auth methods, endpoints, updater, model, themes
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

## Security and contributing

Report a vulnerability privately through
[GitHub's security advisories for this repository](https://github.com/vagdotdev/grokbuildfork/security/advisories/new),
never in a public issue (see [`SECURITY.md`](SECURITY.md)). Bug reports and pull requests go
through GitHub as usual (see [`CONTRIBUTING.md`](CONTRIBUTING.md)). Workshop is distributed under
the Apache License 2.0 ([`LICENSE`](LICENSE)); third-party notices are in
[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES).
