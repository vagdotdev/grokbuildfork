#!/usr/bin/env bash
# Write upstream-lock.toml for the fetched snapshot and stage it.
#
#   scripts/sync/update-lockfile.sh [--report-dir DIR] [--lock FILE] [--date YYYY-MM-DD]
#
# Values come from sync.env written by fetch-upstream.sh. Keys match
# docs/workshop-production-plan.md section 2: source, git_sha, source_rev,
# version, fetched_at. source_rev is upstream's internal monorepo pointer
# (root SOURCE_REV file); it is recorded for traceability and never fetched.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

FETCHED_AT="$(date -u +%Y-%m-%d)"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    --lock) SYNC_LOCK_FILE="$2"; shift 2 ;;
    --date) FETCHED_AT="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
[[ -n "${UPSTREAM_SHA:-}" ]] || die "UPSTREAM_SHA missing; run fetch-upstream.sh first"

cat > "$SYNC_LOCK_FILE" <<EOF
# Upstream snapshot the upstream-owned tree mirrors. Managed by
# scripts/sync/update-lockfile.sh; edit by running the sync, not by hand.
# source_rev is xAI's internal monorepo pointer (upstream SOURCE_REV); it is
# informational only and is never fetched.
source = "${UPSTREAM_URL:-$SYNC_DEFAULT_UPSTREAM_URL}"
git_sha = "$UPSTREAM_SHA"
source_rev = "${UPSTREAM_SOURCE_REV:-}"
version = "${UPSTREAM_VERSION:-}"
fetched_at = "$FETCHED_AT"
EOF
git add -- "$SYNC_LOCK_FILE"
report_set LOCK_UPDATED_AT "$FETCHED_AT"
log "wrote $SYNC_LOCK_FILE: git_sha=$UPSTREAM_SHA source_rev=${UPSTREAM_SOURCE_REV:-?} version=${UPSTREAM_VERSION:-?} fetched_at=$FETCHED_AT"
