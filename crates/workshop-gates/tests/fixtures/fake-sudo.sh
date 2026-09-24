#!/bin/sh
# A stand-in `sudo` for the askpass gate, matching real sudo 1.9.15 run without a terminal: it uses
# the SUDO_ASKPASS helper only when `-A` is given (or DISPLAY is set) — plain `sudo` with no tty
# fails with "a terminal is required". So this proves Workshop's `sudo -A` shim (Grok Build's
# `alias sudo='sudo -A'` equivalent), not a lenient fake that would pass without it. Never prompts
# on a tty itself. A wrong password is retried up to three times ("Sorry, try again.").
use_askpass=0
[ -n "$DISPLAY" ] && use_askpass=1
while [ $# -gt 0 ]; do
  case "$1" in
    -A) use_askpass=1; shift ;;
    -n) echo "sudo: a password is required" >&2; exit 1 ;;
    -S) echo "sudo: this stand-in does not support -S" >&2; exit 1 ;;
    --) shift; break ;;
    -*) shift ;;
    *) break ;;
  esac
done
if [ "$use_askpass" = 0 ] || [ -z "$SUDO_ASKPASS" ]; then
  echo "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper" >&2
  echo "sudo: a password is required" >&2
  exit 1
fi
tries=0
while [ "$tries" -lt 3 ]; do
  pw=$("$SUDO_ASKPASS" "[sudo] password for tester: ") || {
    echo "sudo: no password was provided" >&2
    exit 1
  }
  if [ "$pw" = "hunter2" ]; then
    exec "$@"
  fi
  echo "Sorry, try again." >&2
  tries=$((tries + 1))
done
echo "sudo: 3 incorrect password attempts" >&2
exit 1
