#!/usr/bin/env bash
# A stand-in `opencode` for the engine-failure gates. It identifies itself like the real CLI
# (`--version` → 1.18.31, `--help` → "opencode run …" on stderr) and then fails `serve` the way a
# broken install fails on a machine we cannot see. The mode comes from the file `mode` next to
# this script:
#   nobind   serve never prints "listening on" (hangs)            → startup timeout
#   crash    serve exits 1 at once with a loader-style error       → early exit reported
#   silent   serve is healthy but the model never emits an event   → first-event timeout
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mode="$(cat "$here/mode" 2>/dev/null || echo crash)"
case "${1:-}" in
  --version) echo "1.18.31"; exit 0 ;;
  --help) echo "opencode run [message..]  run opencode with a message" >&2; exit 0 ;;
  serve)
    case "$mode" in
      nobind) exec sleep 3600 ;;
      crash) echo "dyld: Library not loaded: @rpath/libfake.dylib (fault injected)" >&2; exit 1 ;;
      silent) exec python3 "$here/fake-opencode-serve.py" "$@" ;;
      *) echo "unknown fake mode $mode" >&2; exit 2 ;;
    esac ;;
  *) echo "fake opencode: unexpected args $*" >&2; exit 2 ;;
esac
