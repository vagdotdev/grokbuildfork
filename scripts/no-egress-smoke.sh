#!/usr/bin/env bash
# Startup + Login smoke with zero xAI egress.
#
#   scripts/no-egress-smoke.sh target/debug/workshop [OUTDIR]
#
# Runs the binary in a fresh user+network namespace (loopback only, no route out) with
# HTTP(S)_PROXY pointed at a logging proxy that records every hostname and refuses to forward, under
# `strace -f -e trace=network` so DNS query payloads and connect() targets are captured too.
# Scenarios: `--version`, `login` (text picker), headless prompt without a connection (must fail
# closed), and the TUI first run with `l` pressed (picker; never a browser). Fails if any recorded
# hostname matches *.x.ai, *.grok.com, api.mixpanel.com or storage.googleapis.com.
set -euo pipefail
BIN="${1:?path to workshop binary}"
OUT="${2:-target/no-egress}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
command -v strace >/dev/null || { echo "strace required" >&2; exit 2; }
command -v unshare >/dev/null || { echo "unshare required" >&2; exit 2; }
command -v ip >/dev/null || { echo "ip (iproute2) required" >&2; exit 2; }

HOME_DIR="$OUT/home"; CWD_DIR="$OUT/cwd"
rm -rf "$HOME_DIR" "$CWD_DIR"; mkdir -p "$HOME_DIR" "$CWD_DIR"
git -C "$CWD_DIR" init -q .

observed() {
  # observed NAME -- cmd...
  local name="$1"; shift; [ "$1" = "--" ] && shift
  local dir="$OUT/$name"; mkdir -p "$dir"; : > "$dir/proxy.log"
  OUTDIR="$dir" HERE="$HERE" unshare -rn bash -c '
    set -e
    ip link set lo up
    python3 "$HERE/no-egress/logproxy.py" --port 3128 --log "$OUTDIR/proxy.log" & PROXY=$!
    sleep 0.3
    export HTTP_PROXY=http://127.0.0.1:3128 HTTPS_PROXY=http://127.0.0.1:3128 ALL_PROXY=http://127.0.0.1:3128
    export http_proxy=$HTTP_PROXY https_proxy=$HTTPS_PROXY
    set +e
    strace -f -e trace=network -s 512 -o "$OUTDIR/strace.log" -- "$@" > "$OUTDIR/stdout.log" 2> "$OUTDIR/stderr.log"
    echo "exit=$?" > "$OUTDIR/exit.txt"
    kill $PROXY 2>/dev/null || true
    exit 0
  ' bash "$@"
  echo "--- $name ($(cat "$dir/exit.txt")) ---"
  python3 "$HERE/no-egress/summarize.py" "$dir" | tee "$dir/summary.txt"
}

fail=0
COMMON=(env "HOME=$HOME_DIR" "WORKSHOP_HOME=$HOME_DIR/.workshop" TERM=xterm-256color NO_COLOR=1)

observed version -- "${COMMON[@]}" "$BIN" --version || fail=1
observed cli-login -- "${COMMON[@]}" "$BIN" login || fail=1
grep -q "connect a model" "$OUT/cli-login/stdout.log" || { echo "VIOLATION: workshop login did not print the connection picker" >&2; fail=1; }
grep -Eq "auth\.x\.ai|accounts\.x\.ai" "$OUT/cli-login/stderr.log" && { echo "VIOLATION: workshop login mentioned xAI auth on stderr" >&2; fail=1; }

# Headless prompt with no connection: must exit non-zero, never open a browser or contact xAI.
observed headless -- "${COMMON[@]}" "$BIN" -p "say hi" || fail=1
if grep -q "^exit=0" "$OUT/headless/exit.txt"; then echo "VIOLATION: headless prompt without a connection exited 0" >&2; fail=1; fi

# TUI first run: wait for the picker, press l (already open → no-op), Tab to Subscriptions, back, Esc, quit.
observed tui-first-run -- "${COMMON[@]}" python3 "$HERE/no-egress/pty_drive.py" --bin "$BIN" --out "$OUT/tui-first-run/raw.log" --cwd "$CWD_DIR" \
  --script "wait:7000,key:l,wait:1500,key:Tab,wait:800,key:Down,wait:500,key:Tab,wait:800,key:Enter,wait:800,key:Esc,wait:500,key:Esc,wait:800,key:l,wait:1500,key:C-c,wait:800,key:C-c,wait:500" || fail=1
python3 - "$OUT/tui-first-run/raw.log" <<'PY' || fail=1
import re,sys
raw=open(sys.argv[1],'rb').read().decode('utf-8','replace')
txt=re.sub(r'\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07|\x1b[()][A-Z0-9]|\x1b[=>]','',raw)
# ratatui positions the cursor between cells, so compare with all whitespace removed.
flat=''.join(txt.split())
ok=True
for needle in ["connect a model","Models","Subscriptions","xAI (optional)","Claude","Codex","Cursor","[Sign in]"]:
    if ''.join(needle.split()) not in flat:
        print("VIOLATION: TUI first run did not show %r" % needle); ok=False
for bad in ["Login with grok.com","auth.x.ai/.well-known","Login with Grok","accounts.x.ai"]:
    if ''.join(bad.split()) in flat:
        print("VIOLATION: TUI showed %r" % bad); ok=False
print("tui screen check:", "ok" if ok else "FAILED")
sys.exit(0 if ok else 1)
PY

if [ "$fail" = 0 ]; then echo "no-egress-smoke: PASS (evidence in $OUT)"; else echo "no-egress-smoke: FAIL (evidence in $OUT)" >&2; exit 1; fi
