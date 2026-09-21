#!/usr/bin/env bash
# Refresh one patch of the series against the tree at HEAD (quilt refresh).
#
#   scripts/sync/refresh-patch.sh [--patches DIR] <patch-name>
#       Apply the patch with `git apply --3way`. Conflicting files keep their
#       markers in the working tree for you to resolve.
#   scripts/sync/refresh-patch.sh --finish [--patches DIR] <patch-name>
#       After resolving: stage the patch's files, refuse leftover conflict
#       markers, regenerate the patch file from the staged diff (header kept)
#       and commit change + patch file together.
#
# Run this on the sync branch (import commit plus every patch that applied),
# push it, and prove a clean replay of main + the refreshed patch files as
# described in docs/upstream-sync.md, "Fixing a failed patch".
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

FINISH=0 NAME=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --finish) FINISH=1; shift ;;
    --patches) SYNC_PATCHES_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    -*) die "unknown argument: $1" ;;
    *) NAME="$1"; shift ;;
  esac
done
[[ -n "$NAME" ]] || die "usage: refresh-patch.sh [--finish] <patch-name>"

cd "$(repo_root)"
ensure_git_identity
series="$SYNC_PATCHES_DIR/series"
file="$SYNC_PATCHES_DIR/$NAME"
[[ -f "$file" ]] || die "$file not found"
entry="$(series_entry "$series" "$NAME" 2>/dev/null || true)"
strip="${entry%%$'\t'*}"
[[ -n "$strip" ]] || { warn "$NAME is not listed in $series; assuming -p1"; strip=1; }
tags="$(printf '%s' "$entry" | cut -f2)"

if (( ! FINISH )); then
  require_clean_tree
  if git apply --check -p"$strip" "$file" 2>/dev/null; then
    git apply --index -p"$strip" "$file"
    log "$NAME applies cleanly; nothing to refresh. Run with --finish to regenerate it anyway, or 'git reset --hard' to undo."
    exit 0
  fi
  if git apply --3way -p"$strip" "$file"; then
    log "$NAME applied by 3-way merge without conflicts. Review 'git diff --cached', then run: $0 --finish $NAME"
    exit 0
  fi
  log "conflicts left in the working tree:"
  git diff --name-only --diff-filter=U | sed 's/^/  /' >&2
  log "resolve them, then run: $0 --finish $NAME"
  exit 0
fi

# --finish
mapfile -t files < <(patch_files "$strip" "$file")
if git diff --name-only --diff-filter=U | grep -q .; then
  die "unmerged paths remain; resolve and 'git add' them first"
fi
if (( ${#files[@]} )); then git add -A -- "${files[@]}" 2>/dev/null || true; fi
git add -u
if git diff --cached | grep -qE '^\+(<<<<<<<|=======|>>>>>>>)( |$)'; then
  die "conflict markers are still present in the staged changes"
fi
git diff --cached --quiet -- . ":(exclude)$file" && die "nothing staged; the refreshed patch would be empty"
refresh_patch_file "$file"
git commit -q -m "patches: refresh $NAME${tags:+ [$tags]}" \
  -m "Refreshed against the current upstream tree with scripts/sync/refresh-patch.sh."
log "committed refreshed $NAME"
git --no-pager show --stat --oneline HEAD >&2
