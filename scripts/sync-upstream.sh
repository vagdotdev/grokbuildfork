#!/usr/bin/env bash
# Replay the Workshop overlay onto an xai-org/grok-build snapshot.
#
#   scripts/sync-upstream.sh --dry-run            # replay onto the locked base_commit, diff against HEAD
#   scripts/sync-upstream.sh --dry-run --gates    # ... and run cargo test -p workshop-gates in the worktree
#   scripts/sync-upstream.sh --upstream-ref <sha> # replay onto a new upstream commit (opens no PR by itself)
#
# Steps (plan §2 "Quilt / patch replay"):
#   1. resolve the upstream tree (locked base_commit, or a fetched upstream ref)
#   2. materialise it in a throwaway git worktree
#   3. restore overlay-only paths from the current branch (scripts/overlay-paths.txt)
#   4. reset upstream-owned paths to the fetched tree (scripts/upstream-paths.txt)
#   5. apply patches/series in order; a failing gate:* patch is fatal, a failing
#      product/branding patch is reported and leaves .rej files for the sync PR
#   6. refresh Cargo.lock offline, then (optional) run the gates
#   7. dry-run: the replayed tree must be byte-identical to HEAD
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

dry_run=0
run_gates=0
upstream_ref=""
keep=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) dry_run=1 ;;
    --gates) run_gates=1 ;;
    --upstream-ref) upstream_ref="$2"; shift ;;
    --keep) keep=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

lock_get() { sed -n "s/^$1 = \"\(.*\)\"/\1/p" upstream-lock.toml; }
base_commit="$(lock_get base_commit)"
upstream_source="$(lock_get source)"

if [[ -n "$upstream_ref" ]]; then
  echo "==> fetching $upstream_source $upstream_ref"
  git fetch --no-tags "$upstream_source" "$upstream_ref"
  target_tree="$(git rev-parse FETCH_HEAD)"
else
  target_tree="$base_commit"
fi
echo "==> replaying overlay onto $target_tree"

worktree="$(mktemp -d "${TMPDIR:-/tmp}/workshop-sync.XXXXXX")"
cleanup() {
  if [[ $keep -eq 0 ]]; then
    git worktree remove --force "$worktree" 2>/dev/null || rm -rf "$worktree"
  else
    echo "==> worktree kept at $worktree"
  fi
}
trap cleanup EXIT
git worktree add --detach --quiet "$worktree" "$target_tree"

echo "==> restoring overlay paths from HEAD"
grep -v '^\s*#' scripts/overlay-paths.txt | grep -v '^\s*$' | while read -r path; do
  if git cat-file -e "HEAD:$path" 2>/dev/null; then
    git archive HEAD -- "$path" | tar -x -C "$worktree"
  else
    echo "    (skipping $path: not in HEAD)"
  fi
done

echo "==> upstream-owned paths reset to $target_tree (fresh worktree, nothing to do)"

echo "==> applying patches/series"
status=0
while IFS= read -r line; do
  entry="${line%%#*}"
  entry="$(echo "$entry" | xargs)"
  [[ -z "$entry" ]] && continue
  tag="$(echo "$line" | sed -n 's/.*#\s*\(gate:[a-z-]*\|product\|branding\).*/\1/p')"
  patch_file="$repo_root/patches/$entry"
  if [[ ! -f "$patch_file" ]]; then
    echo "    MISSING $entry"; status=1; continue
  fi
  if git -C "$worktree" apply --check "$patch_file" 2>/dev/null; then
    git -C "$worktree" apply "$patch_file"
    echo "    ok      $entry  [$tag]"
  else
    if [[ "$tag" == gate:* ]]; then
      echo "    FAILED  $entry  [$tag]  <- gate patch; the sync is red. Refresh it, never skip it." >&2
      status=1
    else
      echo "    FAILED  $entry  [$tag]  <- defer only with a TODO(sync) in the PR" >&2
      git -C "$worktree" apply --reject "$patch_file" >/dev/null 2>&1 || true
      status=1
    fi
  fi
done < patches/series

if [[ $status -ne 0 ]]; then
  echo "==> patch replay failed" >&2
  keep=1
  exit 1
fi

echo "==> refreshing Cargo.lock"
# Adding workspace members only adds entries for path crates; no new registry
# dependencies. Offline first (hermetic CI cache), then online, then fall back
# to HEAD's lock so a sandbox without the full registry cache can still verify
# the patch replay.
if ! (cd "$worktree" && cargo metadata --format-version 1 --offline >/dev/null 2>&1); then
  if ! (cd "$worktree" && cargo metadata --format-version 1 >/dev/null 2>&1); then
    echo "    (cargo metadata unavailable here; reusing HEAD's Cargo.lock)"
    git show HEAD:Cargo.lock > "$worktree/Cargo.lock"
  fi
fi

if [[ $dry_run -eq 1 ]]; then
  echo "==> dry-run: comparing replayed tree with HEAD"
  if diff -r --exclude=.git --exclude=target "$worktree" "$repo_root" >/tmp/workshop-sync-diff.txt; then
    echo "    replay reproduces HEAD byte-for-byte"
  else
    echo "    replay differs from HEAD:" >&2
    head -50 /tmp/workshop-sync-diff.txt >&2
    exit 1
  fi
fi

if [[ $run_gates -eq 1 ]]; then
  echo "==> running gates in the replayed tree"
  (cd "$worktree" && cargo test -p workshop-gates)
fi

echo "==> sync replay OK onto $target_tree"
