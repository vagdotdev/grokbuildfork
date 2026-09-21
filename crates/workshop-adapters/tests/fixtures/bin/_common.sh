# Shared helpers for the fake vendor CLIs (POSIX sh). Sourced, not executed.
# State and behaviour are controlled only through env vars the tests pass explicitly:
#   FAKE_CLI_STATE_DIR  where calls, args, env, and session turn counters are recorded
#   FAKE_CLI_MODE       ok | slow | idle | drift | no_terminal | fail | huge | exit_nonzero | stderr_flood
#   FAKE_LOGIN_<VENDOR> in | out | garbage   (status commands)
record() { # record <vendor> "$@"
  v="$1"; shift
  [ -n "$FAKE_CLI_STATE_DIR" ] || return 0
  echo "$v $*" >> "$FAKE_CLI_STATE_DIR/calls.$v"
  env > "$FAKE_CLI_STATE_DIR/env.$v"
}
record_run() { # record_run <vendor> "$@"  -- one arg per line
  v="$1"; shift
  [ -n "$FAKE_CLI_STATE_DIR" ] || return 0
  : > "$FAKE_CLI_STATE_DIR/run-args.$v"
  for a in "$@"; do printf '%s\n' "$a" >> "$FAKE_CLI_STATE_DIR/run-args.$v"; done
  env > "$FAKE_CLI_STATE_DIR/run-env.$v"
  pwd > "$FAKE_CLI_STATE_DIR/run-cwd.$v"
}
next_turn() { # next_turn <session-id>  -> prints the turn number, persists it
  f="${FAKE_CLI_STATE_DIR:-/tmp}/session.$1"
  n=0; [ -f "$f" ] && n=$(cat "$f")
  n=$((n + 1)); echo "$n" > "$f"; echo "$n"
}
new_session() { echo "$1-$$-$(date +%s)"; }
mode() { echo "${FAKE_CLI_MODE:-ok}"; }
# Behaviours shared by every vendor's run mode. Each vendor calls this after its init event.
# Returns 0 when the caller should continue with the happy path, 1 when it must exit as-is.
apply_mode() { # apply_mode <vendor>
  case "$(mode)" in
    slow)
      # A grandchild that must die with the process group when Workshop cancels.
      sleep 60 &
      echo $! > "${FAKE_CLI_STATE_DIR:-/tmp}/grandchild.pid"
      sleep 60
      exit 0 ;;
    idle) sleep 60; exit 0 ;;
    drift) echo "Reading additional input from stdin..."; exit 0 ;;
    no_terminal) return 1 ;;
    huge) head -c 300000 /dev/zero | tr '\0' 'x'; echo; exit 0 ;;
    stderr_flood) i=0; while [ $i -lt 500 ]; do echo "stderr noise line $i" >&2; i=$((i + 1)); done ;;
  esac
  return 0
}
