#!/usr/bin/env bash
# Run acceptance tasks N times each, one after another, then print the pass-rate table.
#
#   scripts/acceptance/suite.sh WORKSHOP_BIN RESULTS_DIR RUNS TASK...     e.g. suite.sh ./workshop /tmp/acc 3 T1 T2 T3
#
# T1 and T2 record the desktop too (render.py cuts them into an mp4 per run). Runs of different
# tasks can go in parallel from separate invocations, except the ones that install system packages
# (T1, T5, T11), which share the apt lock.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$1"; ROOT="$2"; RUNS="$3"; shift 3
mkdir -p "$ROOT"
for task in "$@"; do
  for r in $(seq 1 "$RUNS"); do
    out="$ROOT/$task-r$r"
    desk=""; case "$task" in T1|T2) desk="--desktop" ;; esac
    echo "== $task run $r ($(date -u +%T) UTC)"
    "$HERE/run.sh" "$task" "$out" "$BIN" $desk
    if [ -n "$desk" ] && [ -s "$out/raw-screen.mp4" ]; then
      python3 "$HERE/render.py" "$out" "$out/$task-r$r.mp4" > "$out/render.log" 2>&1 || echo "render failed: $out/render.log"
    fi
  done
done
python3 "$HERE/summarize.py" "$ROOT"
