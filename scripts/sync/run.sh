#!/usr/bin/env bash
# Upstream sync pipeline: fetch xai-org/grok-build, replace the upstream-owned
# tree, replay patches/series, update upstream-lock.toml, build, run the gates,
# and render / open the sync PR. Used by .github/workflows/sync-upstream.yml
# and runnable locally against a clean checkout.
#
#   scripts/sync/run.sh [--upstream URL] [--ref REF] [--lock FILE] [--patches DIR]
#                       [--branch NAME | --in-place] [--force]
#                       [--no-verify | --skip-build | --skip-gates]
#                       [--pr none|dry-run|create] [--base main] [--remote origin]
#                       [--report-dir DIR]
#
# Exit status: 0 when upstream did not move or the verdict is green/attention,
# 1 when the verdict is red (gate patch failed, series invalid, build or gate
# suite failed) or the PR could not be opened. The PR (draft when not green)
# is opened before exiting. A --force run on an unchanged upstream is a replay
# proof: it verifies and reports but pushes no branch and opens no PR.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
here="$(sync_dir)"

UPSTREAM_ARGS=() BRANCH="" IN_PLACE=0 FORCE=0 VERIFY=1 VERIFY_ARGS=() PR=none BASE=main REMOTE=origin
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream|--ref) UPSTREAM_ARGS+=("$1" "$2"); shift 2 ;;
    --lock) SYNC_LOCK_FILE="$2"; shift 2 ;;
    --patches) SYNC_PATCHES_DIR="$2"; shift 2 ;;
    --branch) BRANCH="$2"; shift 2 ;;
    --in-place) IN_PLACE=1; shift ;;
    --force) FORCE=1; shift ;;
    --no-verify) VERIFY=0; shift ;;
    --skip-build|--skip-gates) VERIFY_ARGS+=("$1"); shift ;;
    --pr) PR="$2"; shift 2 ;;
    --base) BASE="$2"; shift 2 ;;
    --remote) REMOTE="$2"; shift 2 ;;
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done
case "$PR" in none|dry-run|create) ;; *) die "--pr must be none, dry-run or create" ;; esac
export SYNC_LOCK_FILE SYNC_PATCHES_DIR

cd "$(repo_root)"
init_report_dir
rm -f "$SYNC_REPORT_DIR/sync.env"
ensure_git_identity
require_clean_tree
log "report dir: $SYNC_REPORT_DIR"

gh_output() { [[ -n "${GITHUB_OUTPUT:-}" ]] && printf '%s=%s\n' "$1" "$2" >> "$GITHUB_OUTPUT"; return 0; }

# 1. fetch --------------------------------------------------------------------
"$here/fetch-upstream.sh" "${UPSTREAM_ARGS[@]}" --lock "$SYNC_LOCK_FILE"
report_load
gh_output upstream_sha "$UPSTREAM_SHA"
gh_output upstream_version "${UPSTREAM_VERSION:-}"

if (( ! UPSTREAM_MOVED )) && (( ! FORCE )); then
  log "upstream did not move ($(short_sha "$UPSTREAM_SHA")); nothing to do"
  report_set OUTCOME unchanged
  gh_output outcome unchanged
  exit 0
fi

# 2. branch -------------------------------------------------------------------
if (( ! IN_PLACE )); then
  [[ -n "$BRANCH" ]] || BRANCH="sync/grok-build-$(short_sha "$UPSTREAM_SHA")"
  if git show-ref --quiet --verify "refs/heads/$BRANCH"; then
    (( FORCE )) || die "branch $BRANCH already exists; delete it or pass --force to recreate it"
    git checkout -q -B "$BRANCH"
  else
    git checkout -q -b "$BRANCH"
  fi
else
  BRANCH="$(git branch --show-current)"
  [[ -n "$BRANCH" ]] || die "--in-place needs a checked-out branch"
fi
report_set SYNC_BRANCH "$BRANCH"
gh_output branch "$BRANCH"
log "sync branch: $BRANCH (from $(short_sha "$BASE_SHA"))"

# 3. replace tree + lockfile, committed as a merge with the upstream commit -----
"$here/replace-tree.sh"
"$here/update-lockfile.sh" --lock "$SYNC_LOCK_FILE"
report_load
tree="$(git write-tree)"
import_msg="chore(sync): import grok-build $(short_sha "$UPSTREAM_SHA") (${UPSTREAM_VERSION:-unknown version})

Upstream-owned paths now mirror xai-org/grok-build $UPSTREAM_SHA
(SOURCE_REV ${UPSTREAM_SOURCE_REV:-?}, committed ${UPSTREAM_COMMIT_DATE:-?}).
Overlay paths restored from $(short_sha "$BASE_SHA"); patch series replayed
in the following commits. Lockfile: $SYNC_LOCK_FILE."
import_commit="$(git commit-tree "$tree" -p HEAD -p "$UPSTREAM_SHA" -m "$import_msg")"
git reset -q --soft "$import_commit"
report_set IMPORT_COMMIT "$import_commit"
log "import commit $(short_sha "$import_commit") (parents: $(short_sha "$BASE_SHA"), $(short_sha "$UPSTREAM_SHA"))"

# 4. replay -------------------------------------------------------------------
"$here/replay-patches.sh" --patches "$SYNC_PATCHES_DIR"
report_load

# 5. verify -------------------------------------------------------------------
if (( ! VERIFY )); then
  report_set VERIFY_SKIPPED 1
  report_set VERIFY_SKIPPED_REASON "--no-verify"
elif (( REPLAY_RED )) || (( ${OVERLAY_COLLISION_COUNT:-0} )); then
  report_set VERIFY_SKIPPED 1
  report_set VERIFY_SKIPPED_REASON "replay is red, build and gates were not attempted"
  log "skipping build/gates: replay is red"
else
  "$here/verify.sh" "${VERIFY_ARGS[@]}"
fi

# 6. PR body + PR -------------------------------------------------------------
"$here/pr-body.sh" > /dev/null
report_load
gh_output verdict "$VERDICT"
gh_output pr_title "$PR_TITLE"
pr_failed=0
case "$PR" in
  dry-run)
    (( UPSTREAM_MOVED )) || log "upstream did not move: a real run would stop here with the verdict (no branch, no PR); dry-run output follows"
    "$here/open-pr.sh" --dry-run --base "$BASE" --remote "$REMOTE"
    ;;
  create)
    if (( ! UPSTREAM_MOVED )); then
      # Forced replay of the locked snapshot: the branch would differ from the
      # base only by the lockfile date. The verdict is the proof; nothing to sync.
      log "upstream did not move: replay proof only, not pushing a branch or opening a PR"
      report_set PR_ACTION skipped-unchanged
    else
      pr_args=(--base "$BASE" --remote "$REMOTE")
      (( FORCE )) && pr_args+=(--force)
      # A failed PR creation (token cannot open PRs) must not hide the verdict:
      # outputs are still written, the job fails at the end.
      "$here/open-pr.sh" "${pr_args[@]}" || pr_failed=1
      report_load
      gh_output pr_url "${PR_URL:-}"
      gh_output pr_error "${PR_ERROR:-}"
      gh_output pr_branch_url "${PR_BRANCH_URL:-}"
    fi
    ;;
esac

report_set OUTCOME "$VERDICT"
gh_output outcome "$VERDICT"
log "done: verdict=$VERDICT branch=$BRANCH report=$SYNC_REPORT_DIR"
(( pr_failed )) && die "sync PR could not be opened (see above); verdict was $VERDICT"
[[ "$VERDICT" != red ]]
