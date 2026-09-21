#!/usr/bin/env bash
# Runtime half of gate:no-theft: run the built binary under strace inside a
# network-less namespace and prove it never opens another app's credentials.
#
#   scripts/no-theft-fs-audit.sh [path/to/workshop-binary] [out.log]
#
# Plants decoy files at every forbidden location under a throwaway HOME so a
# read would show up as an openat() on those paths, then drives the TUI far
# enough to open the connection picker (cold start does it automatically).
set -euo pipefail

bin="${1:-target/release/workshop}"
out="${2:-/tmp/workshop-no-theft-audit.log}"
command -v strace >/dev/null || { echo "strace is required" >&2; exit 2; }
command -v unshare >/dev/null || { echo "unshare is required" >&2; exit 2; }

home="$(mktemp -d)"
mkdir -p "$home/.codex" "$home/.cursor/sdk" "$home/.local/share/opencode" "$home/.claude" "$home/.workshop"
echo '{"OPENAI_API_KEY":"decoy"}' > "$home/.codex/auth.json"
echo '{"apiKey":"decoy"}' > "$home/.cursor/sdk/auth.json"
echo '{"openai":{"access":"decoy"}}' > "$home/.local/share/opencode/auth.json"
echo '{"claudeAiOauth":{"accessToken":"decoy"}}' > "$home/.claude/.credentials.json"

trace="$(mktemp)"
# 60x24 PTY via script(1); the TUI exits on the second Ctrl-C.
unshare -rn --mount sh -c "
  export HOME='$home' WORKSHOP_HOME='$home/.workshop' TERM=xterm-256color
  strace -f -e trace=openat,open,connect,execve -o '$trace' \
    script -qfc 'timeout -s INT 6 $bin 2>/dev/null; sleep 0.2; printf \"\\003\\003\"' /dev/null >/dev/null 2>&1 || true
"
{
  echo "# no-theft filesystem audit — $(date -u +%FT%TZ)"
  echo "# binary: $bin"
  echo "# decoy HOME: $home"
  echo "# forbidden path opens (must be empty):"
  grep -E 'openat|open\(' "$trace" | grep -E '\.codex/auth\.json|\.cursor/sdk/auth\.json|opencode/auth\.json|\.claude/\.credentials|Claude Code-credentials|security' || echo "(none)"
  echo "# execve of vendor tools / keychain (must be empty):"
  grep -E 'execve\(' "$trace" | grep -E 'security|claude|codex|cursor|opencode' || echo "(none)"
  echo "# connect() calls (network namespace has no interfaces; anything here is an attempt, not egress):"
  grep -E 'connect\(' "$trace" | grep -vE 'AF_UNIX' || echo "(none)"
} | tee "$out"
rm -rf "$home" "$trace"
if grep -qE '\.codex/auth\.json|\.cursor/sdk/auth\.json|opencode/auth\.json|\.claude/\.credentials' "$out"; then
  echo "no-theft audit: FAIL"; exit 1
fi
echo "no-theft audit: OK -> $out"
