#!/usr/bin/env bash
# Runtime half of gate:no-theft: run the built binary under strace inside a
# network-less namespace and prove it never opens another app's credentials.
#
#   scripts/no-theft-fs-audit.sh [path/to/workshop-binary] [out.log]
#
# Plants decoy files at every forbidden location under a throwaway HOME so a
# read would show up as an openat() on those paths, then drives the TUI far
# enough to open the connection picker (cold start does it automatically).
#
# Phase 2 runs the subscription model-list probes (workshop-detect's `probe`
# example, next to the binary: `cargo build -p workshop-detect --example probe`,
# or PROBE_BIN=...) against signed-in fake vendor CLIs that never read a
# credential themselves, with a fake `security` keychain tool on PATH: any
# decoy open or keychain call there comes from Workshop's probe code.
set -euo pipefail

bin="${1:-target/release/workshop}"
out="${2:-/tmp/workshop-no-theft-audit.log}"
probe="${PROBE_BIN:-$(dirname "$bin")/examples/probe}"
fakes_src="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/crates/workshop-detect/tests/fixtures/vendors"
command -v strace >/dev/null || { echo "strace is required" >&2; exit 2; }
command -v unshare >/dev/null || { echo "unshare is required" >&2; exit 2; }
[ -x "$probe" ] || { echo "probe example missing at $probe (cargo build -p workshop-detect --example probe)" >&2; exit 2; }

forbidden_re='\.codex/auth\.json|\.cursor/sdk/auth\.json|opencode/auth\.json|\.claude/\.credentials|Claude Code-credentials'

home="$(mktemp -d)"
mkdir -p "$home/.codex" "$home/.cursor/sdk" "$home/.local/share/opencode" "$home/.claude" "$home/.workshop"
echo '{"OPENAI_API_KEY":"decoy"}' > "$home/.codex/auth.json"
echo '{"apiKey":"decoy"}' > "$home/.cursor/sdk/auth.json"
echo '{"openai":{"access":"decoy"}}' > "$home/.local/share/opencode/auth.json"
echo '{"claudeAiOauth":{"accessToken":"decoy"}}' > "$home/.claude/.credentials.json"

trace="$(mktemp)"
trace2="$(mktemp)"
fakes="$(mktemp -d)"
fake_state="$(mktemp -d)"
cleanup() { rm -rf "$home" "$trace" "$trace2" "$fakes" "$fake_state"; }
trap cleanup EXIT

# PTY via script(1) so the TUI starts and opens the picker; SIGINT after 6 s, SIGKILL 3 s later if
# the second Ctrl-C the TUI normally wants never arrives.
unshare -rn --mount sh -c "
  export HOME='$home' WORKSHOP_HOME='$home/.workshop' TERM=xterm-256color
  strace -f -e trace=openat,open,connect,execve -o '$trace' \
    script -qfc 'timeout -s INT -k 3 6 $bin 2>/dev/null' /dev/null >/dev/null 2>&1 || true
"
{
  echo "# no-theft filesystem audit — $(date -u +%FT%TZ)"
  echo "# binary: $bin"
  echo "# decoy HOME: $home"
  echo "# forbidden path opens (must be empty):"
  grep -E 'openat|open\(' "$trace" | grep -E "$forbidden_re|security" || echo "(none)"
  echo "# execve of vendor tools / keychain (must be empty; the launch-time engine install is a bash+curl pipeline, not a vendor tool):"
  grep -E 'execve\("[^"]*(security|claude|codex|cursor|opencode)[^"]*"' "$trace" || echo "(none)"
  echo "# connect() calls (network namespace has no interfaces; anything here is an attempt, not egress):"
  grep -E 'connect\(' "$trace" | grep -vE 'AF_UNIX' || echo "(none)"
  echo "# trace coverage (must not be empty, or the binary never ran under strace):"
  echo "syscalls=$(wc -l < "$trace") openat=$(grep -c 'openat(' "$trace" || true) execve=$(grep -c 'execve(' "$trace" || true)"
  echo "# Workshop home paths opened (identity: ~/.workshop):"
  grep -E 'openat\(' "$trace" | grep -oE '"[^"]*/\.workshop/[^"]*"' | sed "s#$home#~#" | sort | uniq -c | sort -rn | head -6 || true
} | tee "$out"
if [ "$(grep -c 'openat(' "$trace" || true)" -lt 50 ]; then
  echo "no-theft audit: INCONCLUSIVE (trace too small; did the TUI start?)" >&2; exit 2
fi

# Phase 2: the model-list probes, signed in, through every vendor's documented protocol.
cp "$fakes_src"/* "$fakes/"
printf '#!/bin/sh\necho "security $*" >> "%s/security.calls"\nexit 44\n' "$fake_state" > "$fakes/security"
chmod +x "$fakes"/*
unshare -rn sh -c "
  export HOME='$home' WORKSHOP_HOME='$home/.workshop' PATH='$fakes:/usr/bin:/bin' TMPDIR='$fake_state'
  strace -f -e trace=openat,open,execve -o '$trace2' '$probe' --models \
    --env FAKE_CLI_STATE_DIR='$fake_state' --env FAKE_LOGIN_CLAUDE=in \
    --env FAKE_LOGIN_CODEX=in --env FAKE_LOGIN_CURSOR=in > '$fake_state/probe.out' 2>&1 || true
"
{
  echo "# phase 2: subscription model-list probes (fake signed-in CLIs from $fakes_src)"
  echo "# model-list commands run (must show --input-format, app-server, models):"
  grep -E 'execve\(' "$trace2" | grep -oE '"(--input-format|app-server|models)"' | sort | uniq -c || true
  echo "# forbidden path opens by the probes (must be empty):"
  grep -E 'openat|open\(' "$trace2" | grep -E "$forbidden_re" || echo "(none)"
  echo "# keychain tool calls (must be empty):"
  { grep -E 'execve\(' "$trace2" | grep -E '/security"'; cat "$fake_state/security.calls" 2>/dev/null; } || echo "(none)"
  echo "# what the probes listed:"
  grep -E '^\[(Claude|Codex |Cursor)\]' "$fake_state/probe.out" || cat "$fake_state/probe.out"
} | tee -a "$out"
for cmd in '"--input-format"' '"app-server"' '"models"'; do
  if ! grep -E 'execve\(' "$trace2" | grep -qF "$cmd"; then
    echo "no-theft audit: INCONCLUSIVE (model probe $cmd never ran)" >&2; exit 2
  fi
done
if [ "$(grep -c 'pill=Ready' "$fake_state/probe.out" || true)" -ne 3 ]; then
  echo "no-theft audit: INCONCLUSIVE (the fake subscriptions were not all Ready)" >&2; exit 2
fi

if grep -qE "$forbidden_re" <(grep -E 'openat|open\(' "$trace" "$trace2") \
  || grep -qE '/security"' <(grep -E 'execve\(' "$trace2") || [ -s "$fake_state/security.calls" ]; then
  echo "no-theft audit: FAIL"; exit 1
fi
echo "no-theft audit: OK -> $out"
