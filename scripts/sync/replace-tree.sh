#!/usr/bin/env bash
# Make the upstream-owned part of the tree an exact copy of the fetched
# snapshot (refs/sync/upstream-new) while keeping every overlay path from HEAD.
#
#   scripts/sync/replace-tree.sh [--patches DIR] [--report-dir DIR]
#
# Ownership rule: everything is upstream-owned unless it matches an overlay
# pattern (scripts/overlay-paths.txt or the built-in list in lib.sh). Files
# that HEAD has, upstream lacks, and no overlay pattern claims are dropped:
# upstream deletions go to upstream-deleted-files.txt (routine), files the
# patch series creates go to patch-created-files.txt (the replay re-creates
# them), anything else to dropped-files.txt for a human to confirm.
# Upstream files that collide with an overlay pattern are listed in
# overlay-collisions.txt and make the run red (conflict policy rule 4) —
# except single files Workshop replaces outright (an overlay entry naming
# one file HEAD has, such as README.md): those go to
# overlay-replaced-files.txt, ours is kept and the run stays green.
#
# The result is left staged in the index; run.sh commits it (as a merge with
# the upstream commit as second parent) after update-lockfile.sh.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --patches) SYNC_PATCHES_DIR="$2"; shift 2 ;;
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

# Overlay entries that name one file HEAD has (no glob, a blob rather than a directory) replace
# upstream's same-named file outright — the root README, SECURITY.md, CONTRIBUTING.md. Ours is
# kept and upstream's dropped; that is by declaration, not a rule-4 collision.
: > "$SYNC_REPORT_DIR/overlay-replaced-files.txt"
for pat in "${overlay[@]}"; do
  case "$pat" in *'*'*|*'?'*|*'['*) continue ;; esac
  [[ "$(git cat-file -t "HEAD:$pat" 2>/dev/null)" == blob ]] || continue
  grep -qxF -- "$pat" "$tmp/upstream.txt" && printf '%s\n' "$pat" >> "$SYNC_REPORT_DIR/overlay-replaced-files.txt"
done
sort -o "$SYNC_REPORT_DIR/overlay-replaced-files.txt" "$SYNC_REPORT_DIR/overlay-replaced-files.txt"
filter_paths "${overlay[@]}" < "$tmp/upstream.txt" \
  | comm -23 - "$SYNC_REPORT_DIR/overlay-replaced-files.txt" > "$SYNC_REPORT_DIR/overlay-collisions.txt"
# Overlay files to restore: HEAD files under overlay patterns, minus paths
# upstream now ships itself (upstream wins there; rule 4 says rename ours).
filter_paths "${overlay[@]}" < "$tmp/head.txt" \
  | comm -23 - "$SYNC_REPORT_DIR/overlay-collisions.txt" > "$tmp/overlay-files.txt"
# Not upstream, not overlay: split into upstream deletions (present in the
# locked upstream tree), files the patch series creates (the replay brings
# them back), and genuinely stale files that neither side owns.
: > "$tmp/lock.txt"
if git show-ref --quiet --verify "$SYNC_REF_LOCK"; then
  git ls-tree -r --name-only "$SYNC_REF_LOCK" | sort > "$tmp/lock.txt"
fi
: > "$SYNC_REPORT_DIR/patch-created-files.txt"
if [[ -f "$SYNC_PATCHES_DIR/series" ]]; then
  series_created_paths "$SYNC_PATCHES_DIR/series" "$SYNC_PATCHES_DIR" > "$SYNC_REPORT_DIR/patch-created-files.txt"
fi
comm -23 "$tmp/head.txt" "$tmp/upstream.txt" | comm -23 - "$tmp/overlay-files.txt" > "$tmp/orphans.txt"
comm -12 "$tmp/orphans.txt" "$tmp/lock.txt" > "$SYNC_REPORT_DIR/upstream-deleted-files.txt"
comm -23 "$tmp/orphans.txt" "$tmp/lock.txt" | comm -12 - "$SYNC_REPORT_DIR/patch-created-files.txt" \
  > "$SYNC_REPORT_DIR/patch-recreated-files.txt"
comm -23 "$tmp/orphans.txt" "$tmp/lock.txt" | comm -23 - "$SYNC_REPORT_DIR/patch-created-files.txt" \
  > "$SYNC_REPORT_DIR/dropped-files.txt"

# Snapshot the upstream tree into index + working tree, then bring the overlay
# files back from HEAD. Untracked files (target/, .sync-report) are untouched.
git read-tree -u --reset "$SYNC_REF_NEW"
if [[ -s "$tmp/overlay-files.txt" ]]; then
  tr '\n' '\0' < "$tmp/overlay-files.txt" | xargs -0 git checkout --quiet HEAD --
fi

report_set OVERLAY_FILE_COUNT "$(wc -l < "$tmp/overlay-files.txt")"
report_set OVERLAY_REPLACED_COUNT "$(wc -l < "$SYNC_REPORT_DIR/overlay-replaced-files.txt")"
report_set OVERLAY_COLLISION_COUNT "$(wc -l < "$SYNC_REPORT_DIR/overlay-collisions.txt")"
report_set DROPPED_FILE_COUNT "$(wc -l < "$SYNC_REPORT_DIR/dropped-files.txt")"
report_set UPSTREAM_DELETED_COUNT "$(wc -l < "$SYNC_REPORT_DIR/upstream-deleted-files.txt")"
report_set PATCH_RECREATED_COUNT "$(wc -l < "$SYNC_REPORT_DIR/patch-recreated-files.txt")"

log "restored $(wc -l < "$tmp/overlay-files.txt") overlay file(s) ($(wc -l < "$SYNC_REPORT_DIR/overlay-replaced-files.txt") replacing upstream's); $(wc -l < "$SYNC_REPORT_DIR/upstream-deleted-files.txt") file(s) deleted upstream; $(wc -l < "$SYNC_REPORT_DIR/patch-recreated-files.txt") patch-created file(s) to be replayed; dropped $(wc -l < "$SYNC_REPORT_DIR/dropped-files.txt") stale non-upstream file(s)"
if [[ -s "$SYNC_REPORT_DIR/overlay-collisions.txt" ]]; then
  warn "upstream now ships files under overlay paths (kept upstream's version, needs a human):"
  sed 's/^/  /' "$SYNC_REPORT_DIR/overlay-collisions.txt" >&2
fi
