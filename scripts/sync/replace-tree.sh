#!/usr/bin/env bash
# Make the upstream-owned part of the tree an exact copy of the fetched
# snapshot (refs/sync/upstream-new) while keeping every overlay path from HEAD.
#
#   scripts/sync/replace-tree.sh [--report-dir DIR]
#
# Ownership rule: everything is upstream-owned unless it matches an overlay
# pattern (scripts/overlay-paths.txt or the built-in list in lib.sh). Files
# that HEAD has, upstream lacks, and no overlay pattern claims are dropped and
# listed in dropped-files.txt (patch outputs are re-created by the replay).
# Upstream files that collide with an overlay pattern are listed in
# overlay-collisions.txt and make the run red (conflict policy rule 4).
#
# The result is left staged in the index; run.sh commits it (as a merge with
# the upstream commit as second parent) after update-lockfile.sh.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
require_clean_tree
git show-ref --quiet --verify "$SYNC_REF_NEW" || die "$SYNC_REF_NEW missing; run fetch-upstream.sh first"

mapfile -t overlay < <(load_overlay_patterns)
printf '%s\n' "${overlay[@]}" > "$SYNC_REPORT_DIR/overlay-patterns.txt"
log "overlay patterns: ${overlay[*]}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

git ls-tree -r --name-only HEAD | sort > "$tmp/head.txt"
git ls-tree -r --name-only "$SYNC_REF_NEW" | sort > "$tmp/upstream.txt"

filter_paths "${overlay[@]}" < "$tmp/upstream.txt" > "$SYNC_REPORT_DIR/overlay-collisions.txt"
# Overlay files to restore: HEAD files under overlay patterns, minus paths
# upstream now ships itself (upstream wins there; rule 4 says rename ours).
filter_paths "${overlay[@]}" < "$tmp/head.txt" \
  | comm -23 - "$SYNC_REPORT_DIR/overlay-collisions.txt" > "$tmp/overlay-files.txt"
# Dropped: in HEAD, not overlay, not in upstream.
comm -23 "$tmp/head.txt" "$tmp/upstream.txt" | comm -23 - "$tmp/overlay-files.txt" \
  > "$SYNC_REPORT_DIR/dropped-files.txt"

# Snapshot the upstream tree into index + working tree, then bring the overlay
# files back from HEAD. Untracked files (target/, .sync-report) are untouched.
git read-tree -u --reset "$SYNC_REF_NEW"
if [[ -s "$tmp/overlay-files.txt" ]]; then
  tr '\n' '\0' < "$tmp/overlay-files.txt" | xargs -0 git checkout --quiet HEAD --
fi

report_set OVERLAY_FILE_COUNT "$(wc -l < "$tmp/overlay-files.txt")"
report_set OVERLAY_COLLISION_COUNT "$(wc -l < "$SYNC_REPORT_DIR/overlay-collisions.txt")"
report_set DROPPED_FILE_COUNT "$(wc -l < "$SYNC_REPORT_DIR/dropped-files.txt")"

log "restored $(wc -l < "$tmp/overlay-files.txt") overlay file(s); dropped $(wc -l < "$SYNC_REPORT_DIR/dropped-files.txt") stale non-upstream file(s)"
if [[ -s "$SYNC_REPORT_DIR/overlay-collisions.txt" ]]; then
  warn "upstream now ships files under overlay paths (kept upstream's version, needs a human):"
  sed 's/^/  /' "$SYNC_REPORT_DIR/overlay-collisions.txt" >&2
fi
