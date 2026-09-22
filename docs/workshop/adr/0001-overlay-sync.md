# ADR 0001 — Workshop is a thin overlay on an exact Grok Build tree

Status: accepted (M0). Owner: Workshop.

## Decision

Workshop keeps the upstream `xai-org/grok-build` crate graph unchanged (`crates/codegen/xai-grok-*`,
`crates/common/xai-*`, proto `xai.grok.tools.v1`, Rust identifiers, `authors = ["xAI"]`). Product
behaviour lives in new `crates/workshop-*` crates plus a short, numbered patch series
(`patches/series`) on the few upstream files whose compile-time defaults first run hits.

The base snapshot is recorded in `upstream-lock.toml` (`source`, `git_sha`, `source_rev`,
`version`, `fetched_at`). The sync tooling (`scripts/sync/*`, `.github/workflows/sync-upstream.yml`,
`docs/upstream-sync.md`; separate PR) fetches a new snapshot, replaces the tree while preserving
the paths in `scripts/overlay-paths.txt`, replays `patches/series` with `git apply --3way`, runs
`cargo check`, `cargo test -p workshop-gates` and `scripts/no-xai-scan.sh`, and opens a draft PR.

The tree keeps upstream files in their patched state (quilt "pushed"). `patches/groups.txt` maps
each patch to its upstream paths; `scripts/regenerate-patches.sh` rewrites `patches/*.patch` and
`patches/series` from the diff against `git_sha`, and CI fails when they disagree with the tree.
`Cargo.lock` is committed but never patched: a sync regenerates it. Workshop crates join the
workspace through the `Cargo.toml` patch (`0009-workspace-members.patch`).

## Why not rename the crates

A mechanical `xai-grok-*` → `workshop-*` rename touches ~72 crates and every import; each upstream
"Synced from monorepo" drop would then be unmergeable. The July attempt (`main` @ `1231e0c7`)
showed the other failure mode: renaming the binary and the home directory while leaving
`GrokComConfig::default()` on `auth.x.ai`. Neither is progress. The overlay keeps drops mergeable
and the gates keep the login honest.

## Patch vs overlay crate

| Kind | Lives in |
|---|---|
| Compile-time default first run hits (`GrokComConfig::default`, `push_interactive_login`, `PRODUCTION_ENDPOINTS`, updater constants, `default_grok_home`, `[[bin]] name`, `default_models.json`) | patch |
| New product behaviour (connection picker policy, CLI detection, adapters, providers) | `crates/workshop-*` |
| Hook from upstream into the overlay (welcome Login → picker) | small patch calling the crate |

## Series format

One patch per line, tag as a trailing comment: `0001-no-default-xai-oauth.patch  # gate:no-xai`.
Tags: `gate:no-xai`, `gate:no-theft`, `product`, `branding`. A commented-out gate line is a red
sync, not a skip.

## Conflict policy

1. `gate:no-xai` / `gate:no-theft` patches never skip. A failed gate patch is a red sync; refresh it
   with `scripts/sync/refresh-patch.sh`.
2. `product` / `branding` patches may be deferred only with a `TODO(sync)` naming the user-visible gap,
   and never if deferral would re-enable xAI login, endpoints or updater.
3. Every sync PR is human-reviewed. Changes to the Login / OIDC / env / updater files upstream are a
   security review, not a routine refresh.
4. No silent tree replace: overlay paths are restored after every fetch.
