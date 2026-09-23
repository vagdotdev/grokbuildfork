#!/usr/bin/env bash
# Run one acceptance task once, recorded, on a fresh HOME. The tasks and their pass criteria are in
# scripts/acceptance/tasks/*.steps and verify.py.
#
#   scripts/acceptance/run.sh TASK OUTDIR WORKSHOP_BIN [--desktop]
#
# Workshop runs in a real 120x36 PTY (a tmux pane on a private socket, `-L acc`) under
# `asciinema rec --stdin`, so the cast holds every byte drawn and every key typed. With --desktop the
# pane is also shown in an xfce4-terminal on $DISPLAY and the whole desktop is recorded to
# OUTDIR/raw-screen.mp4 (render.py cuts the waits afterwards).
#
# Needs: tmux, asciinema, python3 with pyte and PIL, and a static ffmpeg outside the user's PATH
# ($ACC_FFMPEG, default /opt/rec/bin/ffmpeg; T5 removes the system one). sudo for system resets.
#
# Steps file (one per line; '#' comments; `@key value` directives before the first step):
#   @user NAME / @password PW / @timeout S / @stall S      run as NAME (T11), per-turn limits
#   launch CMD         type CMD at the shell prompt, then wait for the composer
#   type TEXT          type a prompt exactly, then Enter (starts a turn)
#   waitturn [S]       wait until that turn has ended (engine session record + still screen)
#   line TEXT          type any other line + Enter (slash commands, shell commands)
#   key KEYS           tmux key names (C-c, Escape, Enter)
#   waitre REGEX [S]   wait until the screen matches
#   waitshell [S]      wait for the shell prompt after Workshop exits
#   snap LABEL         disk snapshot now (verify.py snap)
#   probe NAME         in-session check now (verify.py probe, or a key probe below)
#   mark TEXT          caption/marker in events.jsonl (protect-start / protect-end keep video)
#   sleep S
set -uo pipefail
export LC_ALL=C.UTF-8
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TASK="$1"; mkdir -p "$2"; OUT="$(cd "$2" && pwd)"; BIN="$3"; DESKTOP="${4:-}"
STEPS="$HERE/tasks/$TASK.steps"
[ -f "$STEPS" ] || { echo "no steps file for $TASK" >&2; exit 2; }
[ -x "$BIN" ] || { echo "no workshop binary at $BIN" >&2; exit 2; }
RUN_ID="$(basename "$OUT")"
SESSION="acc-$RUN_ID"
FFMPEG="${ACC_FFMPEG:-/opt/rec/bin/ffmpeg}"
export DISPLAY="${DISPLAY:-:1}"
XAUTH="${XAUTHORITY:-$HOME/.Xauthority}"
T=(tmux -L acc)

directive() { sed -n "s/^@$1[[:space:]]\+//p" "$STEPS" | head -1; }
RUN_USER="$(directive user)"; RUN_USER="${RUN_USER:-$(id -un)}"
PASSWORD="$(directive password)"
TURN_TIMEOUT="$(directive timeout)"; TURN_TIMEOUT="${TURN_TIMEOUT:-900}"
STALL="$(directive stall)"; STALL="${STALL:-120}"
if [ "$RUN_USER" = "$(id -un)" ]; then UHOME="$OUT/home"; AS=(); else UHOME="/home/acc-$RUN_USER/$RUN_ID"; AS=(sudo -n -u "$RUN_USER"); fi

rm -rf "$OUT"/{live-screen.txt,events.jsonl,driver.log,cast-wall-sync.txt,prompts.log,prompts,snaps,probes,verify.*}
mkdir -p "$OUT/snaps" "$OUT/probes"
CAST="$OUT/session.cast"; rm -f "$CAST"
LIVE="$OUT/live-screen.txt"
EVENTS="$OUT/events.jsonl"; LOG="$OUT/driver.log"

cast_now() { head -1 "$LIVE" 2>/dev/null | sed -E 's/# cast_t=([0-9.]+).*/\1/'; }
ev() { # ev NAME [TEXT]
  local c; c="$(cast_now)"
  python3 -c 'import json,sys,time; print(json.dumps({"wall": round(time.time(),3), "cast_t": float(sys.argv[1] or 0), "ev": sys.argv[2], "text": sys.argv[3]}))' \
    "${c:-0}" "$1" "${2:-}" >> "$EVENTS"
  echo "[$(date -u +%T.%3N) cast=${c:-0}] $1 ${2:-}" >> "$LOG"
}
screen() { tail -n +2 "$LIVE" 2>/dev/null; }
screen_has() { screen | grep -qE -- "$1"; }
# Screen text without what changes on its own: braille spinner frames, clock times, elapsed counters.
norm() { screen | LC_ALL=C sed -E $'s/\xe2[\xa0-\xa3][\x80-\xbf]//g; s/[0-9]{1,2}:[0-9]{2}( [AP]M)?//g; s/[0-9]+(\\.[0-9]+)?\\s?(ms|s|m|min)\\b//g'; }
# A tool call is running: the run's `opencode serve` has a child process.
tool_running() {
  local p
  for p in $(pgrep -f "$UHOME/.workshop/tools/opencode" 2>/dev/null); do pgrep -P "$p" >/dev/null && return 0; done
  return 1
}
# The waiting line (`⠏ Thinking… · 4s · Ctrl+C to cancel`); braille alone is not busy (the hero logo uses it).
busy() { screen | grep -qE 'Thinking…|Ctrl\+C to cancel|Esc to interrupt'; }
uread() { "${AS[@]}" cat "$@" 2>/dev/null; }
turns_total() {
  local f n=0 k
  for f in $("${AS[@]}" sh -c "ls '$UHOME'/.workshop/engine/sessions/*.json" 2>/dev/null); do
    k="$(uread "$f" | python3 -c 'import json,sys; print(len(json.load(sys.stdin).get("turns", [])))' 2>/dev/null)"
    n=$((n + ${k:-0}))
  done
  echo "$n"
}
type_text() {
  local s="$1" i ch
  for ((i = 0; i < ${#s}; i++)); do
    ch="${s:i:1}"
    [ "$ch" = ";" ] && ch='\;'
    "${T[@]}" send-keys -t "$SESSION" -l -- "$ch"
    sleep 0.045
  done
}
type_line() { type_text "$1"; "${T[@]}" send-keys -t "$SESSION" Enter; }
waitre() {
  local rx="$1" t="${2:-60}" end=$((SECONDS + ${2:-60}))
  while [ $SECONDS -lt $end ]; do screen_has "$rx" && { ev waitre_ok "$rx"; return 0; }; sleep 0.3; done
  ev waitre_timeout "$rx (${t}s)"; return 1
}
waitshell() { # the shell prompt is back as the last non-empty line
  local end=$((SECONDS + ${1:-30}))
  while [ $SECONDS -lt $end ]; do
    screen | sed '/^[[:space:]]*$/d' | tail -1 | grep -qE '^\$ ?$' && { ev shell_back; return 0; }
    sleep 0.3
  done
  ev shell_timeout; return 1
}
TURN_BASE=0
waitturn() {
  local limit="${1:-$TURN_TIMEOUT}" start=$SECONDS prev="" cur still=0 stalled=0 n
  while [ $((SECONDS - start)) -lt "$limit" ]; do
    cur="$(norm)"
    if [ "$cur" = "$prev" ]; then still=$((still + 1)); else still=0; prev="$cur"; fi
    n="$(turns_total)"
    if [ "$n" -gt "$TURN_BASE" ] && [ $still -ge 6 ] && ! busy; then
      ev turn_end "record $n after $((SECONDS - start))s"; TURN_BASE="$n"; return 0
    fi
    # No engine record (a non-engine connection answered): 120 s still, nothing busy on screen and no
    # tool process running ends the turn.
    if [ $still -ge 240 ] && ! busy && [ "$n" -le "$TURN_BASE" ] && ! tool_running; then
      ev turn_end "idle 120s without a record after $((SECONDS - start))s"; return 0
    fi
    if [ $still -ge $((STALL * 2)) ] && [ $stalled -eq 0 ]; then ev stall "screen unchanged ${STALL}s"; stalled=1; fi
    [ $still -eq 0 ] && stalled=0
    sleep 0.5
  done
  ev turn_timeout "${limit}s"; return 1
}
probe_keys() { # probes that need the keyboard
  case "$1" in
    composer-echo)
      local before; before="$(screen | grep -c 'xq7')"
      type_text "xq7"; sleep 1.5
      if [ "$(screen | grep -c 'xq7')" -gt "$before" ]; then ev probe_ok "composer echoes typed text"; else ev probe_fail "typed text not echoed"; fi
      screen > "$OUT/probes/composer-echo.txt"
      "${T[@]}" send-keys -t "$SESSION" BSpace BSpace BSpace ;;
    *) return 1 ;;
  esac
}

# --- fresh machine state for this task --------------------------------------------------------
ev setup_start "$TASK as $RUN_USER, HOME=$UHOME"
python3 "$HERE/setup.py" "$TASK" "$UHOME" "$OUT" "$RUN_USER" >> "$LOG" 2>&1 || { ev setup_failed; exit 3; }
if [ ${#AS[@]} -gt 0 ]; then
  sudo install -D -m 755 -o "$RUN_USER" -g "$RUN_USER" "$BIN" "$UHOME/.workshop/bin/workshop"
else
  install -D -m 755 "$BIN" "$UHOME/.workshop/bin/workshop"
fi
# The owner's decision: Workshop starts in always-approve. Seeded until that default ships.
printf '[ui]\npermission_mode = "always-approve"\n' | "${AS[@]}" tee "$UHOME/.workshop/config.toml" >/dev/null
RC="$UHOME/.acc-rc"
printf 'PS1="\\$ "\nexport LANG=C.UTF-8\n' | "${AS[@]}" tee "$RC" >/dev/null
UPATH="$UHOME/.local/bin:$UHOME/.workshop/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
XENV="DISPLAY=$DISPLAY XAUTHORITY=$XAUTH"; [ ${#AS[@]} -gt 0 ] && XENV=""
INNER="${AS[*]} env -i HOME=$UHOME USER=$RUN_USER LOGNAME=$RUN_USER PATH=$UPATH TERM=xterm-256color LANG=C.UTF-8 SHELL=/bin/bash $XENV bash --noprofile --rcfile $RC -i"
python3 - "$OUT/run.json" <<EOF
import json, sys, time
json.dump({"task": "$TASK", "run_id": "$RUN_ID", "user": "$RUN_USER", "home": "$UHOME", "bin": "$BIN",
           "path": "$UPATH", "desktop": "$DESKTOP" == "--desktop", "started": time.time(),
           "turn_timeout": $TURN_TIMEOUT, "stall": $STALL}, open(sys.argv[1], "w"), indent=1)
EOF
python3 "$HERE/verify.py" snap "$TASK" "$OUT" before >> "$LOG" 2>&1

# --- recorded session -------------------------------------------------------------------------
"${T[@]}" kill-session -t "$SESSION" 2>/dev/null
"${T[@]}" -f /dev/null new-session -d -s "$SESSION" -x 120 -y 36 \
  "asciinema rec --stdin --overwrite -q -c '$INNER' '$CAST'"
"${T[@]}" set -g status off >/dev/null; "${T[@]}" set -g window-size manual >/dev/null
"${T[@]}" set -g escape-time 0 >/dev/null
ACC_SUDO_PASSWORD="$PASSWORD" python3 "$HERE/monitor.py" "$CAST" "$OUT" "$SESSION" > "$OUT/monitor.log" 2>&1 &
MON=$!
FF=""; TERMW=""
if [ "$DESKTOP" = "--desktop" ]; then
  echo "ffmpeg_start_wall=$(date +%s.%3N)" > "$OUT/video-sync.txt"
  "$FFMPEG" -loglevel error -y -f x11grab -video_size 1920x1200 -framerate 15 -i "$DISPLAY" \
    -codec:v libx264 -preset veryfast -pix_fmt yuv420p "$OUT/raw-screen.mp4" > "$OUT/ffmpeg.log" 2>&1 &
  FF=$!
  xfce4-terminal --disable-server --geometry=120x36 --font="JetBrains Mono 14" --hide-menubar \
    --title Terminal --command "tmux -L acc attach -t $SESSION" > /dev/null 2>&1 &
  TERMW=$!
  for _ in $(seq 1 60); do WID="$(xdotool search --onlyvisible --pid "$TERMW" 2>/dev/null | head -1)"; [ -n "$WID" ] && break; sleep 0.5; done
  [ -n "${WID:-}" ] && { xdotool windowmove "$WID" 40 40; xdotool mousemove 1900 1180; }
fi
for _ in $(seq 1 100); do [ -s "$LIVE" ] && break; sleep 0.1; done
waitre '^\$' 30 >/dev/null
ev recording_started "$(date -u +%FT%TZ)"

# --- steps ------------------------------------------------------------------------------------
while IFS= read -r raw || [ -n "$raw" ]; do
  line="${raw%%$'\r'}"
  case "$line" in ''|'#'*|'@'*) continue ;; esac
  cmd="${line%% *}"; arg="${line#* }"; [ "$arg" = "$line" ] && arg=""
  case "$cmd" in
    launch)
      type_line "$arg"; ev launch "$arg"
      waitre 'always-approve|Big Pickle|Ask anything' 90
      sleep 2
      if screen_has 'always-approve'; then ev mode_on_screen always-approve; else ev mode_on_screen "not shown"; fi
      screen > "$OUT/probes/composer.txt"
      TURN_BASE="$(turns_total)" ;;
    type) TURN_BASE="$(turns_total)"; type_text "$arg"; "${T[@]}" send-keys -t "$SESSION" Enter; ev prompt_sent "$arg" ;;
    waitturn) waitturn ${arg:+"$arg"}; screen > "$OUT/probes/turn-end-$(grep -c '"turn_end"\|"turn_timeout"' "$EVENTS").txt" ;;
    line) type_line "$arg"; ev line "$arg" ;;
    key) # shellcheck disable=SC2086
      "${T[@]}" send-keys -t "$SESSION" $arg; ev key "$arg" ;;
    waitre) waitre "${arg% *}" "${arg##* }" ;;
    waitshell) waitshell ${arg:+"$arg"} ;;
    snap) python3 "$HERE/verify.py" snap "$TASK" "$OUT" "$arg" >> "$LOG" 2>&1; ev snap "$arg" ;;
    probe) probe_keys "$arg" || { python3 "$HERE/verify.py" probe "$TASK" "$OUT" "$arg" >> "$LOG" 2>&1; ev probe "$arg"; } ;;
    mark) ev mark "$arg" ;;
    sleep) sleep "$arg" ;;
    *) ev unknown_step "$line" ;;
  esac
done < "$STEPS"

# --- teardown ---------------------------------------------------------------------------------
screen > "$OUT/probes/final-screen.txt"
"${T[@]}" send-keys -t "$SESSION" -l -- "exit"; "${T[@]}" send-keys -t "$SESSION" Enter
for _ in $(seq 1 20); do "${T[@]}" has-session -t "$SESSION" 2>/dev/null || break; sleep 0.5; done
"${T[@]}" kill-session -t "$SESSION" 2>/dev/null
sleep 1
[ -n "$FF" ] && { kill -INT "$FF" 2>/dev/null; wait "$FF" 2>/dev/null; echo "ffmpeg_stop_wall=$(date +%s.%3N)" >> "$OUT/video-sync.txt"; }
[ -n "$TERMW" ] && kill "$TERMW" 2>/dev/null
kill "$MON" 2>/dev/null
ev recording_stopped
python3 "$HERE/verify.py" verify "$TASK" "$OUT" >> "$LOG" 2>&1
python3 "$HERE/verify.py" cleanup "$TASK" "$OUT" >> "$LOG" 2>&1
tail -1 "$OUT/verify.txt" 2>/dev/null
