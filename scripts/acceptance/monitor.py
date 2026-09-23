#!/usr/bin/env python3
"""Live side of the acceptance runner.

usage: monitor.py CAST OUTDIR TMUX_TARGET

Tails the asciinema cast as it is written and keeps a pyte screen. Every 0.2 s it writes
OUTDIR/live-screen.txt (a "# cast_t=… wall=…" header, then the screen text) and appends (wall, cast_t)
pairs to OUTDIR/cast-wall-sync.txt so cast time maps onto the desktop video afterwards.

It also plays the user where a real user would have to act, and logs each case to OUTDIR/prompts.log:
- An approval prompt ("Allow …" with a numbered option footer) should never show in always-approve.
  If one does, it is logged with a screen dump and answered `1` after 3 s so the run can go on.
- A password prompt drawn on the terminal (`[sudo] password for …:`, `Password:`) is answered with
  $ACC_SUDO_PASSWORD and Enter after 3 s, the way the user would type it. Without that variable the
  prompt is only logged.
"""
import json
import os
import re
import subprocess
import sys
import time

import pyte

cast, out, target = sys.argv[1], sys.argv[2], sys.argv[3]
password = os.environ.get("ACC_SUDO_PASSWORD")
live = os.path.join(out, "live-screen.txt")
dumps = os.path.join(out, "prompts")
os.makedirs(dumps, exist_ok=True)
plog = open(os.path.join(out, "prompts.log"), "a", buffering=1)
sync = open(os.path.join(out, "cast-wall-sync.txt"), "a", buffering=1)

APPROVAL_Q = re.compile(r"Allow [^\n]*")
APPROVAL_OPTS = re.compile(r"Yes, proceed|1 \(.\) Yes|\d/\d:select")
PASSWORD = re.compile(r"\[sudo\] password for [^\n:]*:|^\s*Password:\s*$", re.M)


def keys(*args):
    subprocess.run(["tmux", "-L", "acc", "send-keys", "-t", target, *args], capture_output=True)


def log(kind, n, text, t):
    stamp = time.strftime("%H:%M:%S", time.gmtime())
    plog.write(json.dumps({"wall": round(time.time(), 3), "utc": stamp, "cast_t": round(t, 3),
                           "kind": kind, "n": n, "text": text}) + "\n")


while not os.path.exists(cast) or os.path.getsize(cast) == 0:
    time.sleep(0.05)
f = open(cast, encoding="utf-8", errors="replace")
screen = stream = None
buf = ""
last_t = 0.0
counts = {"approval": 0, "password": 0}
pending = {"approval": None, "password": None}
cooldown = {"approval": 0.0, "password": 0.0}

while True:
    chunk = f.read()
    if chunk:
        buf += chunk
        lines = buf.split("\n")
        buf = lines.pop()
        got = False
        for line in lines:
            if not line.strip():
                continue
            if screen is None:
                hdr = json.loads(line)
                screen = pyte.Screen(hdr["width"], hdr["height"])
                stream = pyte.Stream(screen)
                continue
            try:
                t, k, d = json.loads(line)
            except Exception:
                continue
            last_t = t
            got = True
            if k == "o":
                stream.feed(d)
        if got:
            sync.write(f"{time.time():.3f} {last_t:.3f}\n")
    if screen is None:
        time.sleep(0.05)
        continue
    txt = "\n".join(line.rstrip() for line in screen.display)
    now = time.time()
    with open(live + ".tmp", "w") as g:
        g.write(f"# cast_t={last_t:.3f} wall={now:.3f}\n{txt}\n")
    os.replace(live + ".tmp", live)

    seen = {
        "approval": bool(APPROVAL_Q.search(txt) and APPROVAL_OPTS.search(txt)),
        "password": bool(PASSWORD.search(txt)),
    }
    for kind, on_screen in seen.items():
        if not on_screen:
            pending[kind] = None
            continue
        if now < cooldown[kind]:
            continue
        if pending[kind] is None:
            counts[kind] += 1
            pending[kind] = now
            n = counts[kind]
            m = (APPROVAL_Q if kind == "approval" else PASSWORD).search(txt)
            with open(os.path.join(dumps, f"{kind}-{n:02d}.txt"), "w") as g:
                g.write(txt + "\n")
            log(f"{kind}_shown", n, m.group(0).strip(), last_t)
        elif now - pending[kind] >= 3.0:
            n = counts[kind]
            if kind == "approval":
                keys("1")
                log("approval_answered", n, "pressed 1", last_t)
            elif password:
                keys("-l", password)
                keys("Enter")
                log("password_typed", n, "typed the password + Enter", last_t)
            else:
                log("password_unanswered", n, "no ACC_SUDO_PASSWORD", last_t)
            pending[kind] = None
            cooldown[kind] = now + 8.0
    time.sleep(0.2)
