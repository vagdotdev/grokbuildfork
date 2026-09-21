#!/usr/bin/env bash
# Interactive runtime proof for milestone A/B, complementing scripts/no-egress-smoke.sh
# (which drives fixed scenarios through a logging HTTP proxy). Run the built `workshop`
# binary in a hermetic sandbox and log every DNS query name, every connect(), and every
# open of a decoy foreign-credential file. Run it *inside a terminal* (tmux pane or a
# terminal emulator): the TUI takes over the PTY, you drive Login / `l` / `/login`, and on
# exit the script writes <outdir>/egress-proof.log with a verdict.
#
#   scripts/hermetic-login-proof.sh [binary] [outdir] [-- extra workshop args]
#
# Sandbox: unshare -r (uid 0 inside), -n (fresh netns: only lo, no route out),
# --mount (private /etc/resolv.conf -> 127.0.0.1 where a logging DNS stub
# answers NXDOMAIN). strace -f records connect/sendto/openat/execve. Nothing can
# leave the namespace; the logs show what the binary *tried*.
set -euo pipefail

bin="${1:-target/release/workshop}"
out="${2:-/tmp/workshop-hermetic-proof}"
shift $(( $# >= 2 ? 2 : $# )) || true
[[ "${1:-}" == "--" ]] && shift
for tool in unshare strace ip python3; do
  command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 2; }
done
bin="$(realpath "$bin")"
mkdir -p "$out"
: > "$out/dns-queries.log"
: > "$out/strace.log"

home="$(mktemp -d "${TMPDIR:-/tmp}/workshop-proof-home.XXXXXX")"
mkdir -p "$home/.codex" "$home/.cursor/sdk" "$home/.local/share/opencode" "$home/.claude" "$home/.workshop" "$home/project"
echo '{"OPENAI_API_KEY":"decoy-codex"}' > "$home/.codex/auth.json"
echo '{"apiKey":"decoy-cursor"}' > "$home/.cursor/sdk/auth.json"
echo '{"openai":{"access":"decoy-opencode"}}' > "$home/.local/share/opencode/auth.json"
echo '{"claudeAiOauth":{"accessToken":"decoy-claude"}}' > "$home/.claude/.credentials.json"
cp /etc/nsswitch.conf "$home/nsswitch.conf" 2>/dev/null || true
printf 'nameserver 127.0.0.1\noptions timeout:1 attempts:1\n' > "$home/resolv.conf"

cat > "$home/dns-stub.py" <<'PY'
import socket, sys, datetime
from datetime import timezone
log = open(sys.argv[1], "a", buffering=1)
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 53))
while True:
    data, addr = s.recvfrom(4096)
    try:
        i, labels = 12, []
        while i < len(data) and data[i]:
            n = data[i]; labels.append(data[i+1:i+1+n].decode("ascii", "replace")); i += n + 1
        qname = ".".join(labels)
        qtype = int.from_bytes(data[i+1:i+3], "big") if i + 3 <= len(data) else 0
        log.write(f"{datetime.datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%S.%fZ')} query type={qtype} name={qname}\n")
        # NXDOMAIN: header with QR|RD, RCODE=3, copy the question.
        flags = ((data[2] & 0x01) | 0x80).to_bytes(1, "big") + b"\x83"
        s.sendto(data[:2] + flags + b"\x00\x01\x00\x00\x00\x00\x00\x00" + data[12:i+5], addr)
    except Exception as e:  # noqa: BLE001
        log.write(f"stub-error {e!r}\n")
PY

export PROOF_BIN="$bin" PROOF_OUT="$out" PROOF_HOME="$home" PROOF_ARGS="$*"
unshare -r -n --mount --propagation private bash -euo pipefail -c '
  ip link set lo up
  mount --bind "$PROOF_HOME/resolv.conf" /etc/resolv.conf
  python3 "$PROOF_HOME/dns-stub.py" "$PROOF_OUT/dns-queries.log" 2>"$PROOF_OUT/dns-stub.stderr" &
  stub=$!
  sleep 0.3
  export HOME="$PROOF_HOME" WORKSHOP_HOME="$PROOF_HOME/.workshop" TERM="${TERM:-xterm-256color}"
  unset XAI_API_KEY GROK_CODE_XAI_API_KEY GROK_OAUTH2_ISSUER GROK_OIDC_ISSUER GROK_HOME
  cd "$PROOF_HOME/project"
  set +e
  # shellcheck disable=SC2086
  strace -f -tt -e trace=connect,sendto,openat,execve -s 200 -o "$PROOF_OUT/strace.log" "$PROOF_BIN" $PROOF_ARGS
  echo "workshop exit=$?" > "$PROOF_OUT/exit-code"
  kill $stub 2>/dev/null
'

{
  echo "# Workshop hermetic login proof — $(date -u +%FT%TZ)"
  echo "# binary: $bin ($(sha256sum "$bin" | cut -c1-16)…)"
  echo "# sandbox: unshare -r -n --mount; lo only; /etc/resolv.conf -> 127.0.0.1 logging stub (NXDOMAIN); strace -f connect/sendto/openat/execve"
  echo "# HOME=$home (throwaway, with decoy ~/.codex/auth.json ~/.cursor/sdk/auth.json ~/.local/share/opencode/auth.json ~/.claude/.credentials.json)"
  echo "# $(cat "$out/exit-code" 2>/dev/null)"
  echo
  echo "## DNS queries (every hostname the process tried to resolve)"
  if [[ -s "$out/dns-queries.log" ]]; then sed 's/^/  /' "$out/dns-queries.log"; else echo "  (none)"; fi
  echo
  echo "## connect() to non-loopback / non-UNIX addresses (must be empty: nothing can leave the namespace, this is what was attempted)"
  grep -E 'connect\(' "$out/strace.log" | grep -v 'AF_UNIX' | grep -vE 'sin_addr=inet_addr\("127\.' | sed 's/^/  /' || echo "  (none)"
  echo
  echo "## connect() to loopback (DNS stub on 127.0.0.1:53; 127.0.0.1:1 is Workshop's closed neutral default, not egress)"
  grep -E 'connect\(' "$out/strace.log" | grep -E 'sin_addr=inet_addr\("127\.' | sed 's/^/  /' | head -50 || echo "  (none)"
  echo
  echo "## forbidden host mentions anywhere in the syscall trace (auth.x.ai, accounts.x.ai, *.grok.com, x.ai)"
  grep -E 'auth\.x\.ai|accounts\.x\.ai|grok\.com|[^a-z]x\.ai' "$out/strace.log" "$out/dns-queries.log" | sed 's/^/  /' || echo "  (none)"
  echo
  echo "## foreign credential opens (gate:no-theft; must be empty)"
  grep -E 'openat\(' "$out/strace.log" | grep -E '\.codex/auth\.json|\.cursor/sdk/auth\.json|opencode/auth\.json|\.claude/\.credentials|Claude Code-credentials' | sed 's/^/  /' || echo "  (none)"
  echo
  echo "## execve of vendor CLIs / keychain (must be empty this milestone)"
  grep -E 'execve\(' "$out/strace.log" | grep -E '"[^"]*(security|claude|codex|cursor|opencode)[^"]*"' | sed 's/^/  /' || echo "  (none)"
  echo
  echo "## trace coverage (proves strace saw the process; not an empty log)"
  echo "  syscalls traced: $(wc -l < "$out/strace.log"); openat: $(grep -c 'openat(' "$out/strace.log" || true); execve: $(grep -c 'execve(' "$out/strace.log" || true)"
  echo "  Workshop home touched (identity: ~/.workshop, not ~/.grok):"
  grep -E 'openat\(' "$out/strace.log" | grep -oE '"[^"]*/\.workshop/[^"]*"' | sort | uniq -c | sort -rn | head -8 | sed 's/^/    /' || true
  echo "  advertised auth methods (unified log): $(grep -oE '"methods":\[[^]]*\]' "$home/.workshop/logs/unified.jsonl" 2>/dev/null | sort -u | tr '\n' ' ')"
  echo
  echo "## verdict"
  fail=0
  if grep -qE 'auth\.x\.ai|accounts\.x\.ai|grok\.com|[^a-z]x\.ai' "$out/strace.log" "$out/dns-queries.log"; then echo "  FAIL: an xAI / Grok host was attempted"; fail=1; fi
  if grep -E 'openat\(' "$out/strace.log" | grep -qE '\.codex/auth\.json|\.cursor/sdk/auth\.json|opencode/auth\.json|\.claude/\.credentials'; then echo "  FAIL: a foreign credential file was opened"; fail=1; fi
  if grep -E 'connect\(' "$out/strace.log" | grep -v AF_UNIX | grep -vqE 'sin_addr=inet_addr\("127\.'; then echo "  NOTE: non-loopback connect() attempts present (see above)"; fi
  [[ $fail -eq 0 ]] && echo "  OK: zero requests toward auth.x.ai / accounts.x.ai / *.grok.com / x.ai; zero foreign credential opens"
} > "$out/egress-proof.log"
# Keep Workshop's own logs (unified log names every request the process attempted) for the report.
rm -rf "$out/workshop-home"
cp -r "$home/.workshop" "$out/workshop-home" 2>/dev/null || true
rm -rf "$home"
cat "$out/egress-proof.log"
