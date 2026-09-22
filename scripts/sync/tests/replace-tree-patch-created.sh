#!/usr/bin/env bash
# Shell test: files the patch series creates are not "stale files dropped".
#
#   scripts/sync/tests/replace-tree-patch-created.sh
#
# Builds a throwaway git repo: an "upstream" snapshot, a base branch in quilt
# pushed state (overlay paths, patches/series, one patch that edits an upstream
# file and creates a new one, one renaming patch) plus a genuinely stale file.
# Then runs replace-tree.sh and replay-patches.sh against it and asserts:
#   - patch_created_files / series_created_paths see the new file and the
#     rename target;
#   - replace-tree.sh lists the patch-created file under
#     patch-recreated-files.txt, only the stale file under dropped-files.txt,
#     and resets the upstream-owned file;
#   - the replay re-creates the file and the tree ends clean.
set -euo pipefail

sync_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=lib.sh
source "$sync_dir/lib.sh"

fail=0
check() { # check DESCRIPTION CONDITION...
  local desc="$1"; shift
  if "$@"; then printf 'ok   %s\n' "$desc"; else printf 'FAIL %s\n' "$desc"; fail=1; fi
}
file_is() { [[ -f "$1" && "$(cat "$1")" == "$2" ]]; }
lines_equal() { [[ "$(cat "$1")" == "$2" ]]; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid
export SYNC_REPORT_DIR="$work/report"

# --- fixture ----------------------------------------------------------------
git init -q -b main "$work/repo"
cd "$work/repo"
mkdir -p dir
printf 'upstream a\n' > a.txt
printf 'upstream b\n' > dir/b.txt
printf 'keep\n' > dir/keep.txt
git add -A && git commit -q -m "Synced from monorepo"
upstream="$(git rev-parse HEAD)"

# Patch 0001: edit a.txt and create new.txt. Patch 0002: rename dir/b.txt.
printf 'patched a\n' > a.txt
printf 'created by patch\n' > new.txt
git add -A
patch1="$(git diff --cached --full-index)"
git commit -q -m "wip 0001"
git mv dir/b.txt dir/c.txt
patch2="$(git diff --cached --full-index -M)"
git commit -q -m "wip 0002"

# Base branch = upstream + both patches applied (quilt pushed) + overlay files
# + a stale leftover no patch creates.
mkdir -p patches scripts
printf '%s\n' "$patch1" > patches/0001-edit-and-create.patch
printf '%s\n' "$patch2" > patches/0002-rename-b.patch
printf '%s\n' \
  '0001-edit-and-create.patch  # gate:no-xai' \
  '0002-rename-b.patch         # product' > patches/series
printf 'patches\nscripts\n' > scripts/overlay-paths.txt
printf 'left behind\n' > stale.txt
git add -A && git commit -q -m "base: overlay + patches pushed + stale file"
git update-ref "$SYNC_REF_NEW" "$upstream"
git update-ref "$SYNC_REF_LOCK" "$upstream"

# --- lib helpers -----------------------------------------------------------
check "patch_created_files sees the new file" \
  lines_equal <(patch_created_files 1 patches/0001-edit-and-create.patch) "new.txt"
check "patch_created_files sees the rename target" \
  lines_equal <(patch_created_files 1 patches/0002-rename-b.patch) "dir/c.txt"
check "series_created_paths merges the series" \
  lines_equal <(series_created_paths patches/series patches) $'dir/c.txt\nnew.txt'

# --- replace-tree ----------------------------------------------------------
"$sync_dir/replace-tree.sh" --report-dir "$SYNC_REPORT_DIR" >/dev/null 2>&1
# shellcheck disable=SC1091
source "$SYNC_REPORT_DIR/sync.env"
check "patch-created files are listed, not dropped" \
  lines_equal "$SYNC_REPORT_DIR/patch-recreated-files.txt" $'dir/c.txt\nnew.txt'
check "only the stale file is dropped" \
  lines_equal "$SYNC_REPORT_DIR/dropped-files.txt" "stale.txt"
check "DROPPED_FILE_COUNT=1" [ "${DROPPED_FILE_COUNT:-}" = 1 ]
check "PATCH_RECREATED_COUNT=2" [ "${PATCH_RECREATED_COUNT:-}" = 2 ]
check "upstream-owned a.txt reset to upstream" file_is a.txt "upstream a"
check "upstream-owned dir/b.txt restored" file_is dir/b.txt "upstream b"
check "patch-created new.txt gone until replay" [ ! -e new.txt ]
check "stale.txt gone" [ ! -e stale.txt ]
check "overlay patches/series kept" [ -f patches/series ]
git commit -q -m "import" >/dev/null

# --- replay ------------------------------------------------------------------
"$sync_dir/replay-patches.sh" --report-dir "$SYNC_REPORT_DIR" >/dev/null 2>&1
# shellcheck disable=SC1091
source "$SYNC_REPORT_DIR/sync.env"
check "both patches applied clean" [ "${PATCH_APPLIED:-}" = 2 ]
check "replay not red" [ "${REPLAY_RED:-1}" = 0 ]
check "replay re-created new.txt" file_is new.txt "created by patch"
check "replay re-created dir/c.txt" file_is dir/c.txt "upstream b"
check "a.txt patched again" file_is a.txt "patched a"
check "tree clean after replay" [ -z "$(git status --porcelain)" ]

if (( fail )); then echo "FAILED"; exit 1; fi
echo "PASS"
