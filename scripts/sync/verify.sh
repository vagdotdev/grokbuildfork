#!/usr/bin/env bash
# Build and gate checks for a sync branch.
#
#   scripts/sync/verify.sh [--skip-build] [--skip-gates] [--report-dir DIR]
#
# Runs whatever exists on this branch and records pass / fail / absent:
#   build   cargo check -p xai-grok-pager-bin plus every crates/workshop-* member
#   gates   cargo test -p workshop-gates          (no-xAI Gates 1-4, gate:no-theft)
#           scripts/no-xai-scan.sh                (default-path + binary scan)
#           cargo test -p workshop-adapters       (fake-CLI adapter suite)
# Overlay crates add entries to Cargo.lock; a dirty Cargo.lock after the build
# is committed. Results go to sync.env (VERIFY_*); this script exits 0 and
# pr-body.sh turns the results into the verdict.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

BUILD=1 GATES=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) BUILD=0; shift ;;
    --skip-gates) GATES=0; shift ;;
    --report-dir) SYNC_REPORT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
done

cd "$(repo_root)"
init_report_dir
report_load
ensure_git_identity

if [[ -z "${PROTOC:-}" ]] && command -v protoc >/dev/null 2>&1; then
  export PROTOC
  PROTOC="$(command -v protoc)"
fi
export CARGO_TERM_COLOR=never

run_step() { # KEY LABEL CMD...
  local key="$1" label="$2" logf="$SYNC_REPORT_DIR/$1.log"
  shift 2
  log "running: $*"
  if "$@" > "$logf" 2>&1; then
    report_set "VERIFY_$key" pass
    log "$label: pass"
  else
    report_set "VERIFY_$key" fail
    log "$label: FAIL (see $logf)"
    tail -n 30 "$logf" >&2
  fi
  report_set "VERIFY_${key}_CMD" "$*"
}

# --- build -----------------------------------------------------------------
if (( BUILD )); then
  pkgs=(-p xai-grok-pager-bin)
  unregistered=()
  for manifest in crates/workshop-*/Cargo.toml; do
    [[ -f "$manifest" ]] || continue
    crate_dir="${manifest%/Cargo.toml}"
    name="$(sed -n 's/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n1)"
    if grep -q "\"$crate_dir\"" Cargo.toml; then
      pkgs+=(-p "$name")
    else
      unregistered+=("$crate_dir")
    fi
  done
  if (( ${#unregistered[@]} )); then
    warn "overlay crates not listed in workspace members (workspace patch missing?): ${unregistered[*]}"
    report_set VERIFY_UNREGISTERED_CRATES "${unregistered[*]}"
  fi
  # No --locked: upstream's Cargo.lock lacks the overlay crates' entries and
  # cargo adds only those; the refreshed lock is committed below.
  run_step BUILD "cargo check" cargo check "${pkgs[@]}"
  if [[ -n "$(git status --porcelain -- Cargo.lock)" ]]; then
    git add -- Cargo.lock
    git commit -q -m "chore(sync): refresh Cargo.lock for overlay crates"
    report_set VERIFY_CARGO_LOCK_REFRESHED 1
    log "committed refreshed Cargo.lock"
  fi
else
  report_set VERIFY_BUILD skipped
fi

# --- gates -----------------------------------------------------------------
gates_present=0
if (( GATES )); then
  if [[ -f crates/workshop-gates/Cargo.toml ]]; then
    gates_present=1
    run_step GATE_TESTS "cargo test -p workshop-gates" cargo test -p workshop-gates
  else
    report_set VERIFY_GATE_TESTS absent
  fi
  if [[ -x scripts/no-xai-scan.sh ]]; then
    gates_present=1
    run_step NO_XAI_SCAN "scripts/no-xai-scan.sh" scripts/no-xai-scan.sh
  else
    report_set VERIFY_NO_XAI_SCAN absent
  fi
  if [[ -f crates/workshop-adapters/Cargo.toml ]]; then
    gates_present=1
    run_step ADAPTER_TESTS "cargo test -p workshop-adapters" cargo test -p workshop-adapters
  else
    report_set VERIFY_ADAPTER_TESTS absent
  fi
else
  report_set VERIFY_GATE_TESTS skipped
  report_set VERIFY_NO_XAI_SCAN skipped
  report_set VERIFY_ADAPTER_TESTS skipped
fi
report_set VERIFY_GATES_PRESENT "$gates_present"
(( gates_present )) || warn "no gate suite on this branch (crates/workshop-gates, scripts/no-xai-scan.sh); gates cannot be proven"

if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
  warn "verification left tracked files modified:"
  git status --porcelain --untracked-files=no >&2
  report_set VERIFY_DIRTY 1
fi
