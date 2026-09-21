# ADR 0001 — Workshop is an overlay on the exact Grok Build tree

Status: accepted (M0). Supersedes the rename-the-world draft.

## Context

Upstream lands as periodic "Synced from monorepo" snapshot drops of
`xai-org/grok-build`; `SOURCE_REV` is an internal pointer, not a public parent.
The tree has ~72 `xai-*` / `xai-grok-*` crates. A mechanical crate rename made
every upstream drop unmergeable, and a surface rename (binary name, home dir)
shipped twice without changing the code path that opens `auth.x.ai`.

## Decision

- Keep the upstream crate graph, proto packages, Rust identifiers, and
  `GROK_*` operator env vars exactly as upstream ships them.
- Add Workshop behaviour in new crates (`crates/workshop-*`) that upstream does
  not have.
- Change the few upstream files that hold first-run defaults through a numbered
  patch series (`patches/series`, `scripts/patch-files.txt`). Each upstream file
  belongs to exactly one patch, so patches are order-independent and a refresh
  is `git diff <base_commit> -- <files>`.
- Record the upstream snapshot in `upstream-lock.toml`; `scripts/sync-upstream.sh`
  replays overlay paths plus the series onto it and, in `--dry-run`, must
  reproduce `HEAD` byte-for-byte.
- Tag every patch. `gate:no-xai` / `gate:no-theft` patches never skip: if one
  fails to apply the sync is red. `product` / `branding` patches may be
  deferred in a sync PR only with a `TODO(sync)` naming the user-visible gap,
  never if deferral re-enables xAI login, endpoints, or the updater.
- Gates are cargo tests (`crates/workshop-gates`) that link the real upstream
  crates and assert on compiled defaults. They must fail on a pristine
  snapshot; a grep count is not a gate.

## Consequences

- One shipped binary (`workshop`, from `xai-grok-pager-bin`); no companion.
- Upstream changes to login / OIDC / env / updater files are a security review,
  not a routine refresh.
- Milestone F adds the auto-PR to `sync-upstream.yml`; until then the workflow
  proves replay and uploads `.rej` files.
