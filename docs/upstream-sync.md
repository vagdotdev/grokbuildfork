# Upstream sync (xai-org/grok-build → Workshop overlay)

Workshop is a thin overlay on an exact [xai-org/grok-build](https://github.com/xai-org/grok-build)
tree: upstream crates keep their names, Workshop adds `crates/workshop-*` and a
numbered patch series for the few upstream files that must change. This
document covers the tooling that keeps the upstream part current and the
conflict policy for the patch series. Milestone F, proof 4 of the production
plan: a fresh upstream snapshot plus the patch series plus the overlay crates
builds, passes the no-xAI gates, and opens a sync PR.

## Pieces

| Path | Role |
|---|---|
| `upstream-lock.toml` | The snapshot the upstream-owned tree mirrors: `source`, `git_sha` (public commit), `source_rev` (upstream's `SOURCE_REV`, an internal monorepo pointer — informational, never fetched), `version` (`xai-grok-pager-bin`), `fetched_at`. |
| `patches/series` | Ordered patch list with tags (below). |
| `patches/NNNN-*.patch` | `git format-patch` / `git diff` output against the locked upstream tree. |
| `scripts/overlay-paths.txt` | Optional. Paths Workshop owns; everything else is upstream-owned. Default when absent: `crates/workshop-*`, `patches/`, `scripts/`, `.github/`, `docs/`, `upstream-lock.toml`. An entry naming a single file Workshop has (`README.md`, `SECURITY.md`, `CONTRIBUTING.md`) replaces upstream's same-named file: ours is kept, upstream's dropped, no collision. |
| `scripts/sync/` | The pipeline (this document). |
| `scripts/sync/security-review-paths.txt` | Upstream paths whose change makes a sync a security review (rule 7). |
| `.github/workflows/sync-upstream.yml` | Daily schedule + manual dispatch; opens the sync PR, never merges. |

`scripts/sync/`:

| Script | Does |
|---|---|
| `run.sh` | Orchestrates everything below. `run.sh --pr dry-run` is the local end-to-end. |
| `fetch-upstream.sh` | Fetches upstream into `refs/sync/upstream-new`, the locked commit into `refs/sync/upstream-lock`; records SHA, `SOURCE_REV`, version, commit date; lists upstream commits and changed files; flags security-review paths. Upstream unchanged → `UPSTREAM_MOVED=0`. |
| `replace-tree.sh` | `git read-tree -u --reset` the upstream tree, then restore every overlay path from the base commit. Reports upstream deletions, files the patch series creates (re-created by the replay), stale files dropped, and upstream files colliding with overlay paths. |
| `update-lockfile.sh` | Rewrites `upstream-lock.toml` and stages it. |
| `replay-patches.sh` | Applies the series in order, one commit per patch (details below). |
| `verify.sh` | `cargo check -p xai-grok-pager-bin` + every `crates/workshop-*` workspace member; `cargo test -p workshop-gates`, `scripts/no-xai-scan.sh`, `cargo test -p workshop-adapters` when present. Commits a refreshed `Cargo.lock`. |
| `pr-body.sh` | Verdict (`green` / `attention` / `red`), title, labels, Markdown body. |
| `open-pr.sh` | Pushes `sync/grok-build-<sha12>` and opens or updates the PR with `gh`. Draft unless green. `--dry-run` prints instead. |
| `refresh-patch.sh` | Human helper to fix a patch that no longer applies (quilt refresh). |

The import commit is a merge: first parent the base branch, second parent the
upstream commit, tree = upstream tree + overlay paths. `git log` and `git blame`
on upstream files therefore point at upstream's "Synced from monorepo" drops,
and `git apply --3way` always has the pre-image blobs.

## `patches/series` format

Quilt-compatible; one patch per line, tags in the trailing comment:

```text
# <patch> [-pN]   # <tag> [<tag>...] [free text: user-visible gap if deferred]
0001-no-default-xai-oauth.patch            # gate:no-xai
0002-neutral-production-endpoints.patch    # gate:no-xai
0003-disable-xai-updater.patch             # gate:no-xai
0004-workshop-home-and-bin.patch           # branding
0006-welcome-opens-picker.patch            # product Login still opens the Grok welcome screen
0008-no-provider-autodock.patch            # gate:no-theft
```

Tags: `gate:no-xai`, `gate:no-theft` (never skippable), `product`, `branding`
(deferrable with an explicit annotation). Bare tags after the file name are
accepted too. An untagged patch is treated as non-deferrable and warned about.
Free text after the tags is carried into the PR as the "user-visible gap" of a
deferred patch. Patches must be `git diff` / `git format-patch` output (the
`index` lines are what make the 3-way fallback possible).

## Replay semantics

For each series entry, in order, on the imported upstream tree:

1. `git apply --check` clean → apply, commit `patches: <name> [<tags>]`. Status `applied`.
2. Otherwise `git apply --3way`. Success → status `applied-3way`; the patch
   file is refreshed (header kept, diff body regenerated with `--full-index`
   against the new upstream) and committed together with the change so the next
   drop applies cleanly. The PR body lists the upstream commits that touched the
   patched files (both sides of the merge).
3. Otherwise the hunks are harvested with `git apply --reject` into the report
   (`rejects/<patch>/…`), the tree is reset, and:
   - `gate:*` or untagged → status `failed`, the run is **red**;
   - `product` / `branding` → status `deferred`, the replay continues, the PR
     body gets a `TODO(sync)` line.
4. A patch whose 3-way result is empty is `noop`: upstream already contains the
   change; drop it from the series.

Series lint (red): a listed patch file is missing; a gate-tagged patch is
commented out of `series`.

No fuzz, no `patch -p1`: the same inputs always give the same tree.

## Verdict, PR, labels

| Verdict | Cause | PR |
|---|---|---|
| **red** | gate patch failed / series invalid / upstream file collides with an overlay path / `cargo check` failed / a gate suite failed | draft, label `sync-red`, workflow run fails |
| **attention** | non-gate patch deferred / noop patch / security-review path changed upstream / stale files dropped / gates not run or not present yet | draft, label `sync-needs-attention` |
| **green** | everything applied, built, gates passed | ready for review, label `upstream-sync` |

`security-review` is added whenever a path in
`scripts/sync/security-review-paths.txt` changed upstream. Title:
`chore(sync): grok-build <sha12> (<version>)`. The body always lists: locked
vs new SHA / `SOURCE_REV` / version / dates, upstream commits, the replay table
(patch, tags, result, upstream-changed files), failed-patch details with reject
hunks and upstream blame, `TODO(sync)` lines for deferrals, verification
results, tree-replacement stats (overlay files restored, files replacing
upstream's, collisions, deletions, drops) and the full upstream file list.

Auto-PR is not auto-merge: every sync PR gets human review and the required
`no-xai` check. If upstream did not move, the workflow exits 0 and does nothing.
A **forced** run on an unchanged upstream (dispatch `force=true`, or a
`sync/<stamp>@<locked sha>` tag) is a replay proof: it imports, replays, builds
and runs the gates, and the job summary carries the verdict and verification
table, but it pushes no branch and opens no PR because there is nothing to sync
(the tree would differ from `main` only by the lockfile date). It exits 0 on
green/attention and 1 on red. If a sync branch for the same upstream SHA already
has an open PR, the run is a no-op (dispatch with `force` to rebuild it).

## Conflict policy

1. **Gate patches never skip.** If a `gate:no-xai` / `gate:no-theft` patch fails
   to apply, the sync is red. Refresh the patch on the sync branch. Do not
   comment it out of `series`, do not retag it, do not "fix" it by deleting the
   hunk that no longer applies.
2. **Product / branding patches** may be deferred in a sync PR only with the
   explicit `TODO(sync)` line the PR body generates, naming the patch and the
   user-visible gap (the free text in `series`). Deferral is not allowed if it
   would re-enable xAI login, endpoints, or the updater — those hunks belong in
   gate patches in the first place.
3. **Hunk refresh:** prefer three-way (`replay-patches.sh` does it
   automatically; `refresh-patch.sh` for manual conflicts). The PR body attaches
   both sides' blame: the upstream commits that changed the patched files and
   the patch's own header.
4. **Overlay crates** are new files and should not conflict. If upstream adds a
   same-named path under an overlay prefix or glob, `replace-tree.sh` keeps
   upstream's file, lists the collision and the run is red: rename the Workshop
   path, never overwrite upstream. The exception is declared: an overlay entry
   that names one file Workshop has (the root `README.md`, `SECURITY.md`,
   `CONTRIBUTING.md`) is a replacement of upstream's file — ours is kept,
   upstream's dropped, the PR body lists it under "replacing upstream's".
5. **No silent tree replace.** Overlay paths are enumerated
   (`scripts/overlay-paths.txt` or the built-in default) and restored after
   every fetch. Files upstream deleted since the lock are counted as routine
   deletions, and files the patch series creates (new files, rename targets —
   read from the patches with `git apply --summary`) are expected to vanish
   and come back with the replay; anything else that was in the base branch
   but is neither upstream, overlay, nor patch-created is listed under "stale
   files dropped" in the PR for a human to confirm.
6. **Human review** on every sync PR. Auto-PR ≠ auto-merge.
7. If upstream changes Login / OIDC / env / updater / paths / telemetry files
   (`security-review-paths.txt`), the sync is a **security review**, not a
   routine refresh: read the upstream diff of those files before approving even
   when every patch applied cleanly.

## Fixing a failed patch

```sh
git fetch origin sync/grok-build-<sha12> && git checkout sync/grok-build-<sha12>
# the branch is at the import commit plus every patch that applied
scripts/sync/refresh-patch.sh 0001-no-default-xai-oauth.patch      # 3-way apply, conflict markers left
# resolve the markers
scripts/sync/refresh-patch.sh --finish 0001-no-default-xai-oauth.patch   # regenerate + commit
git push origin sync/grok-build-<sha12>                              # plain push; ci.yml runs the gates
```

Deferred non-gate patches were skipped, so later patches are already on the
branch; refresh them the same way. Before merging, prove that `main` plus the
refreshed patch files replays cleanly from a fresh import (this is what the
next scheduled run will do after the merge):

```sh
git worktree add /tmp/replay-check main && cd /tmp/replay-check
git checkout sync/grok-build-<sha12> -- patches && git commit -qm "wip: refreshed patches"
scripts/sync/run.sh --force --pr dry-run --branch tmp/replay-check   # expect: verdict green/attention, no failed patch
```

The refreshed patch files reach `main` only through the sync PR, so a
`workflow_dispatch` with `force=true` rebuilds the branch from `main` and would
hit the same conflict again; use it after the PR merged, not instead of it.

## Running locally

```sh
# needs protoc 29.3 on PATH (or PROTOC=...), the pinned Rust toolchain, git ≥ 2.32
scripts/sync/run.sh --pr dry-run                    # full pipeline, prints the PR body
scripts/sync/run.sh --pr dry-run --skip-build       # replay + PR body only
scripts/sync/run.sh --pr create --base main         # what the workflow does (needs GH_TOKEN)
scripts/sync/fetch-upstream.sh                      # just compare lock vs upstream
scripts/sync/run.sh --in-place --force --ref "$(sed -n 's/^git_sha = "\(.*\)"/\1/p' upstream-lock.toml)" --pr dry-run
                                                    # forced replay of the locked snapshot; expect verdict green
scripts/sync/tests/replace-tree-patch-created.sh    # shell test: patch-created files are not "stale"
```

`run.sh` needs a clean working tree and creates `sync/grok-build-<sha12>` from
the current branch (`--in-place` to stay on the current branch, `--force` to
proceed when upstream did not move or the branch exists). The report directory
(default `$RUNNER_TEMP`/`$TMPDIR`/`/tmp` + `/workshop-sync-report`) holds
`sync.env`, `patches.tsv`, `changed-files.txt`, `rejects/`, the cargo logs and
`pr-body.md`; the workflow uploads it as the `sync-report-<run id>` artifact.

## Layout assumptions (reconcile with the M0 overlay)

The scripts were written against the production plan before the M0 overlay
landed. They assume:

- `patches/series` carries tags as a trailing `# gate:no-xai` style comment
  (bare tags after the file name also work); patches are `git diff` /
  `git format-patch` output with `index` lines, `-p1` unless the line says
  otherwise.
- `upstream-lock.toml` is flat TOML with string values `source`, `git_sha`,
  `source_rev`, `version`, `fetched_at`; comments are allowed.
- The base branch keeps the patched upstream files in its tree (quilt "pushed"
  state); the sync resets upstream-owned paths and replays, so the base tree
  and `patches/` must agree.
- Overlay paths default to `crates/workshop-*`, `patches/`, `scripts/`,
  `.github/`, `docs/`, `upstream-lock.toml`; `scripts/overlay-paths.txt`
  (one prefix or glob per line) overrides the list. `scripts/upstream-paths.txt`
  from the plan is not needed: everything not overlay is upstream.
- `crates/workshop-*` become workspace members through a patch on the root
  `Cargo.toml`; `Cargo.lock` is not patched (cargo adds the overlay entries
  during `verify.sh` and the sync commits the result).
- Gate suites are discovered by path: `crates/workshop-gates/Cargo.toml`,
  executable `scripts/no-xai-scan.sh`, `crates/workshop-adapters/Cargo.toml`.
- The plan's `scripts/sync-upstream.sh` is `scripts/sync/run.sh` here; keep a
  one-line wrapper at the old name if M0 adds one.

## Workflow setup

- Secret `SYNC_PR_TOKEN` (optional but recommended): fine-grained PAT or GitHub
  App token limited to this repository with **Contents: write** and
  **Pull requests: write**. Without it the workflow uses `GITHUB_TOKEN`; the PR
  still opens, but pushes made with `GITHUB_TOKEN` do not trigger `ci.yml`, so
  the required `no-xai` status would never appear on the sync PR.
- Branch protection on `main`: require the `no-xai` check; do not allow the sync
  token to bypass reviews.
- Labels `upstream-sync`, `sync-red`, `sync-needs-attention`, `security-review`
  are created on first use.
- Schedule: daily 06:23 UTC, plus `workflow_dispatch` with `upstream_ref`,
  `force`, `skip_verify`.
- On-demand without `actions: write` (App installation tokens cannot dispatch):
  push a `sync/**` tag — `git tag sync/$(date +%Y%m%d-%H%M) && git push origin --tags`
  — which runs like `workflow_dispatch` with `upstream_ref=main`, `force=true`
  against the default branch (never the tag); `sync/<stamp>@<ref>` syncs that
  upstream ref instead (e.g. the locked SHA for a forced replay). Delete the tag
  afterwards (`git push origin :refs/tags/sync/<stamp>`).
- With the default `GITHUB_TOKEN` the PR is only created if the repository
  setting *Actions → General → Allow GitHub Actions to create and approve pull
  requests* is on; otherwise the run pushes the sync branch, reports the reason
  in the job summary and fails, and a human opens the PR from the branch.
