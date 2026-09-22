#!/usr/bin/env bash
# Push the sync branch and open (or update) the sync PR with `gh`.
#
#   scripts/sync/open-pr.sh [--remote origin] [--base main] [--dry-run] [--force]
#                           [--report-dir DIR]
#
# Reads verdict, pr-title.txt, labels.txt and pr-body.md from pr-body.sh.
# green -> ready-for-review PR; attention/red -> draft PR. Never merges.
# If the branch already exists on the remote with an open PR the run is a
# no-op unless --force (then force-with-lease push + PR body update).
# --dry-run prints title, labels, draft flag and body without touching the
# remote. Needs GH_TOKEN (or gh auth) with contents+pull-requests write.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

REMOTE=origin BASE=main DRY_RUN=0 FORCE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --remote) REMOTE="$2"; shift 2 ;;
    --base) BASE="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --force) FORCE=1; shift ;;
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
R="$SYNC_REPORT_DIR"
[[ -f "$R/verdict" && -f "$R/pr-body.md" ]] || die "run pr-body.sh first"

verdict="$(cat "$R/verdict")"
title="$(cat "$R/pr-title.txt")"
mapfile -t labels < "$R/labels.txt"
branch="$(git branch --show-current)"
[[ -n "$branch" ]] || die "detached HEAD; check out the sync branch first"
draft_flag=()
[[ "$verdict" == green ]] || draft_flag=(--draft)

if (( DRY_RUN )); then
  {
    printf 'DRY RUN — would push %s to %s and open a PR against %s\n' "$branch" "$REMOTE" "$BASE"
    printf 'title:  %s\n' "$title"
    printf 'labels: %s\n' "${labels[*]}"
    printf 'draft:  %s (verdict %s)\n' "$([[ ${#draft_flag[@]} -gt 0 ]] && echo yes || echo no)" "$verdict"
    printf 'commits on branch since %s:\n' "${BASE_SHA:-base}"
    git log --oneline --first-parent "${BASE_SHA:-HEAD~1}..HEAD" | sed 's/^/  /'
    printf -- '----- pr-body.md -----\n'
    cat "$R/pr-body.md"
  } >&2
  exit 0
fi

command -v gh >/dev/null || die "gh CLI not found"

label_color() {
  case "$1" in
    upstream-sync) echo 0e8a16 ;;
    sync-red) echo b60205 ;;
    sync-needs-attention) echo fbca04 ;;
    security-review) echo d93f0b ;;
    *) echo ededed ;;
  esac
}
for l in "${labels[@]}"; do
  gh label create "$l" --color "$(label_color "$l")" --force \
    --description "workshop upstream sync ($l)" >/dev/null 2>&1 || warn "could not ensure label $l"
done

existing_pr="$(gh pr list --head "$branch" --base "$BASE" --state open --json url --jq '.[0].url // empty' 2>/dev/null || true)"
remote_has_branch=0
git ls-remote --exit-code --heads "$REMOTE" "$branch" >/dev/null 2>&1 && remote_has_branch=1

if (( remote_has_branch )) && ! (( FORCE )); then
  if [[ -n "$existing_pr" ]]; then
    log "branch $branch already has an open PR: $existing_pr (use --force to update)"
    report_set PR_URL "$existing_pr"
    report_set PR_ACTION unchanged
    exit 0
  fi
  die "remote branch $branch exists without an open PR; delete it or use --force"
fi

if (( remote_has_branch )); then
  git push --force-with-lease -u "$REMOTE" "HEAD:refs/heads/$branch"
else
  git push -u "$REMOTE" "HEAD:refs/heads/$branch"
fi

label_args=()
for l in "${labels[@]}"; do label_args+=(--label "$l"); done

if [[ -n "$existing_pr" ]]; then
  gh pr edit "$existing_pr" --title "$title" --body-file "$R/pr-body.md" "${label_args[@]}" >/dev/null
  if (( ${#draft_flag[@]} )); then gh pr ready --undo "$existing_pr" >/dev/null 2>&1 || true; fi
  url="$existing_pr"
  action=updated
else
  url="$(gh pr create --base "$BASE" --head "$branch" --title "$title" --body-file "$R/pr-body.md" \
    "${label_args[@]}" "${draft_flag[@]}")"
  action=created
fi
report_set PR_URL "$url"
report_set PR_ACTION "$action"
log "PR $action: $url (${draft_flag[*]:-ready for review})"
