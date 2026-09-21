#!/usr/bin/env bash
# Fetch the public xai-org/grok-build snapshot into this repo's object store and
# compare it with upstream-lock.toml.
#
#   scripts/sync/fetch-upstream.sh [--upstream URL] [--ref REF] [--lock FILE]
#
# Results land in $SYNC_REPORT_DIR:
#   sync.env            UPSTREAM_SHA, UPSTREAM_SOURCE_REV, UPSTREAM_VERSION,
#                       UPSTREAM_COMMIT_DATE, LOCK_*, UPSTREAM_MOVED=0|1
#   upstream-commits.txt  one line per upstream commit since the lock
#   changed-files.txt     `git diff --name-status <lock> <new>`
#   security-review.txt   changed files matching security-review-paths.txt
# The fetched commit is kept at refs/sync/upstream-new, the locked one at
# refs/sync/upstream-lock, so later steps can 3-way merge and blame.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

UPSTREAM_URL="" UPSTREAM_REF="$SYNC_DEFAULT_UPSTREAM_REF"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --upstream) UPSTREAM_URL="$2"; shift 2 ;;
    --ref) UPSTREAM_REF="$2"; shift 2 ;;
    --lock) SYNC_LOCK_FILE="$2"; shift 2 ;;
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir

LOCK_SHA="$(lock_get "$SYNC_LOCK_FILE" git_sha)"
LOCK_SOURCE_REV="$(lock_get "$SYNC_LOCK_FILE" source_rev)"
LOCK_VERSION="$(lock_get "$SYNC_LOCK_FILE" version)"
LOCK_FETCHED_AT="$(lock_get "$SYNC_LOCK_FILE" fetched_at)"
[[ -n "$UPSTREAM_URL" ]] || UPSTREAM_URL="$(lock_get "$SYNC_LOCK_FILE" source)"
[[ -n "$UPSTREAM_URL" ]] || UPSTREAM_URL="$SYNC_DEFAULT_UPSTREAM_URL"
[[ -f "$SYNC_LOCK_FILE" ]] || warn "$SYNC_LOCK_FILE not found; treating every upstream commit as new"

log "fetching $UPSTREAM_URL ($UPSTREAM_REF)"
git fetch --no-tags --quiet "$UPSTREAM_URL" "$UPSTREAM_REF"
git update-ref "$SYNC_REF_NEW" FETCH_HEAD
UPSTREAM_SHA="$(git rev-parse "$SYNC_REF_NEW")"

if [[ -n "$LOCK_SHA" ]]; then
  if ! git cat-file -e "$LOCK_SHA^{commit}" 2>/dev/null; then
    log "locked commit $LOCK_SHA not present locally; fetching it"
    git fetch --no-tags --quiet "$UPSTREAM_URL" "$LOCK_SHA" \
      || warn "could not fetch locked commit $LOCK_SHA (rewritten upstream history?)"
  fi
  if git cat-file -e "$LOCK_SHA^{commit}" 2>/dev/null; then
    git update-ref "$SYNC_REF_LOCK" "$LOCK_SHA"
  else
    git update-ref -d "$SYNC_REF_LOCK" 2>/dev/null || true
  fi
fi

UPSTREAM_SOURCE_REV="$(git show "$SYNC_REF_NEW:SOURCE_REV" 2>/dev/null | tr -d '[:space:]' || true)"
UPSTREAM_VERSION="$(git show "$SYNC_REF_NEW:$SYNC_VERSION_FILE" 2>/dev/null \
  | sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1 || true)"
UPSTREAM_COMMIT_DATE="$(git log -1 --format=%cs "$SYNC_REF_NEW")"
UPSTREAM_SUBJECT="$(git log -1 --format=%s "$SYNC_REF_NEW")"
[[ -n "$UPSTREAM_SOURCE_REV" ]] || warn "upstream tree has no SOURCE_REV file"
[[ -n "$UPSTREAM_VERSION" ]] || warn "could not read version from $SYNC_VERSION_FILE"

UPSTREAM_MOVED=1
[[ "$UPSTREAM_SHA" == "$LOCK_SHA" ]] && UPSTREAM_MOVED=0

: > "$SYNC_REPORT_DIR/upstream-commits.txt"
: > "$SYNC_REPORT_DIR/changed-files.txt"
: > "$SYNC_REPORT_DIR/security-review.txt"
if git show-ref --quiet --verify "$SYNC_REF_LOCK"; then
  git log --format='%h %cs %s' "$SYNC_REF_LOCK..$SYNC_REF_NEW" > "$SYNC_REPORT_DIR/upstream-commits.txt"
  git diff --name-status "$SYNC_REF_LOCK" "$SYNC_REF_NEW" > "$SYNC_REPORT_DIR/changed-files.txt"
  mapfile -t watch < <(sed -e 's/#.*//' -e 's/[[:space:]]*$//' -e '/^$/d' "$(sync_dir)/security-review-paths.txt")
  if (( ${#watch[@]} )); then
    cut -f2- "$SYNC_REPORT_DIR/changed-files.txt" | sed 's/.*\t//' \
      | filter_paths "${watch[@]}" > "$SYNC_REPORT_DIR/security-review.txt"
  fi
fi

report_set UPSTREAM_URL "$UPSTREAM_URL"
report_set UPSTREAM_REF "$UPSTREAM_REF"
report_set UPSTREAM_SHA "$UPSTREAM_SHA"
report_set UPSTREAM_SOURCE_REV "$UPSTREAM_SOURCE_REV"
report_set UPSTREAM_VERSION "$UPSTREAM_VERSION"
report_set UPSTREAM_COMMIT_DATE "$UPSTREAM_COMMIT_DATE"
report_set UPSTREAM_SUBJECT "$UPSTREAM_SUBJECT"
report_set UPSTREAM_MOVED "$UPSTREAM_MOVED"
report_set LOCK_FILE "$SYNC_LOCK_FILE"
report_set LOCK_SHA "$LOCK_SHA"
report_set LOCK_SOURCE_REV "$LOCK_SOURCE_REV"
report_set LOCK_VERSION "$LOCK_VERSION"
report_set LOCK_FETCHED_AT "$LOCK_FETCHED_AT"
report_set BASE_SHA "$(git rev-parse HEAD)"
report_set BASE_BRANCH "$(git branch --show-current || true)"

log "lock:     ${LOCK_SHA:-<none>} (${LOCK_VERSION:-?}, SOURCE_REV ${LOCK_SOURCE_REV:-?}, fetched ${LOCK_FETCHED_AT:-?})"
log "upstream: $UPSTREAM_SHA (${UPSTREAM_VERSION:-?}, SOURCE_REV ${UPSTREAM_SOURCE_REV:-?}, committed $UPSTREAM_COMMIT_DATE)"
if (( UPSTREAM_MOVED )); then
  log "upstream moved: $(wc -l < "$SYNC_REPORT_DIR/upstream-commits.txt") commit(s), $(wc -l < "$SYNC_REPORT_DIR/changed-files.txt") file(s) changed, $(wc -l < "$SYNC_REPORT_DIR/security-review.txt") on the security-review list"
else
  log "upstream did not move"
fi
