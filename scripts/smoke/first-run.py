#!/usr/bin/env python3
"""Drive a first run of the installed `workshop` in a real PTY, exactly as a new user would.

  first-run.py --workshop BIN --project DIR --out DIR [--message TEXT] [--timeout SECONDS]

Launches BIN in DIR (the caller sets HOME/PATH/SHELL for a fresh user), waits for the composer,
types the message at human speed, waits for the reply to stream and settle, then quits with
/exit. Writes OUT/first-run.cast (asciinema v2, playable with `asciinema play`), OUT/NN-*.txt
screens at each step and OUT/timings.json. Exit status 0 only when a reply arrived and the app
quit cleanly. Needs `pyte` (pip install pyte).
"""
import argparse
import json
import os
import pty
import re
import select
import signal
import sys
import time

import pyte

STATUS = re.compile(
    r"Waiting for|Installing|Starting|Thinking|Connecting|connecting|Ctrl\+C to cancel|"
    r"Esc to interrupt|\[stop\]|Repairing|downloaded"
)
SPINNER = re.compile(r"[\u2800-\u28ff]")
CLOCK = re.compile(r"\b\d{1,2}:\d{2}( [AP]M)?\b|\b\d+s\b")


class Session:
    def __init__(self, argv, cwd, cols, rows, cast_path):
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.t0 = time.monotonic()
        self.cast = open(cast_path, "w", encoding="utf-8")
        self.cast.write(json.dumps({"version": 2, "width": cols, "height": rows,
                                    "timestamp": int(time.time())}) + "\n")
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execvp(argv[0], argv)
        import fcntl, struct, termios
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.exit_status = None
        self.last_change = self.now()

    def now(self):
        return time.monotonic() - self.t0

    def pump(self, wait=0.1):
        if self.exit_status is not None:
            time.sleep(wait)
            return
        r, _, _ = select.select([self.fd], [], [], wait)
        if not r:
            return
        try:
            data = os.read(self.fd, 65536)
        except OSError:
            data = b""
        if not data:
            self.reap(block=True)
            return
        self.cast.write(json.dumps([round(self.now(), 3), "o", data.decode("utf-8", "replace")]) + "\n")
        self.stream.feed(data)
        self.last_change = self.now()

    def reap(self, block=False):
        if self.exit_status is not None:
            return self.exit_status
        pid, status = os.waitpid(self.pid, 0 if block else os.WNOHANG)
        if pid:
            self.exit_status = os.waitstatus_to_exitcode(status)
        return self.exit_status

    def text(self):
        return "\n".join(line.rstrip() for line in self.screen.display)

    def send(self, data):
        self.cast.write(json.dumps([round(self.now(), 3), "i", data]) + "\n")
        os.write(self.fd, data.encode())

    def type(self, text, delay=0.04):
        for ch in text:
            self.send(ch)
            self.settle(delay)

    def settle(self, seconds):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self.pump(min(0.05, max(0.0, end - time.monotonic())))

    def wait_for(self, rx, timeout):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump()
            if re.search(rx, self.text(), re.M):
                return self.now()
            if self.reap() is not None:
                return None
        return None


def normalized(text):
    return CLOCK.sub("", SPINNER.sub("", text))


def reply_lines(text, prompt, base):
    """New text between the echoed prompt and the composer box below it."""
    lines = text.split("\n")
    idx = max((i for i, l in enumerate(lines) if prompt[:20] in l), default=None)
    if idx is None:
        return []
    box = next((i for i in range(idx + 1, len(lines)) if lines[i].strip().startswith("\u256d")), len(lines))
    return [l.strip() for l in lines[idx + 1:box]
            if l.strip() and l.strip() not in base and re.search(r"[A-Za-z]{3,}", l)
            and not STATUS.search(l)]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--workshop", required=True)
    ap.add_argument("--project", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--message", default="hello, what can you do?")
    ap.add_argument("--timeout", type=float, default=180)
    ap.add_argument("--cols", type=int, default=132)
    ap.add_argument("--rows", type=int, default=42)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    t = {}
    s = Session([a.workshop], a.project, a.cols, a.rows, os.path.join(a.out, "first-run.cast"))

    def save(name):
        with open(os.path.join(a.out, f"{name}.txt"), "w", encoding="utf-8") as f:
            f.write(s.text() + "\n")

    t["first_frame"] = s.wait_for(r"\S", 30)
    t["composer"] = s.wait_for(r"Big Pickle|/model|Ask anything", 60)
    s.settle(2)
    save("01-composer")
    if t["composer"] is None:
        print("FAIL: composer never appeared", file=sys.stderr)
        return finish(s, a, t, 1)

    s.type(a.message)
    save("02-typed")
    lines = s.text().split("\n")
    base = {l.strip() for l in lines if l.strip()}
    t["enter"] = s.now()
    s.send("\r")

    end = time.monotonic() + a.timeout
    status_seen = False
    while time.monotonic() < end and s.reap() is None:
        s.pump()
        screen = s.text()
        if t.get("first_status") is None and STATUS.search(screen):
            t["first_status"] = s.now()
            status_seen = True
            save("03-waiting")
        new = reply_lines(screen, a.message, base)
        if new and t.get("first_token") is None:
            t["first_token"] = s.now()
            t["first_token_line"] = new[0][:160]
            save("04-first-token")
        if t.get("first_token") is not None:
            break
    if t.get("first_token") is None:
        save("04-no-reply")
        print(f"FAIL: no reply within {a.timeout:.0f}s (status line seen: {status_seen})", file=sys.stderr)
        return finish(s, a, t, 1)

    stable_since, prev = s.now(), normalized(s.text())
    while time.monotonic() < end and s.reap() is None:
        s.pump(0.2)
        cur = normalized(s.text())
        if cur != prev:
            prev, stable_since = cur, s.now()
        elif s.now() - stable_since >= 4 and not STATUS.search(cur):
            break
    t["reply_done"] = stable_since
    save("05-reply")
    return finish(s, a, t, 0)


def finish(s, a, t, rc):
    if s.reap() is None:
        s.type("/exit")
        s.send("\r")
        end = time.monotonic() + 15
        while time.monotonic() < end and s.reap() is None:
            s.pump()
        if s.reap() is None:
            s.send("\x03")
            s.pump(0.3)
            s.send("\x03")
            end = time.monotonic() + 10
            while time.monotonic() < end and s.reap() is None:
                s.pump()
        if s.reap() is None:
            os.kill(s.pid, signal.SIGKILL)
            s.reap(block=True)
            rc = rc or 1
    for _ in range(5):
        s.pump(0.05)
    with open(os.path.join(a.out, "99-after-exit.txt"), "w", encoding="utf-8") as f:
        f.write(s.text() + "\n")
    t["exit_status"] = s.exit_status
    if s.exit_status not in (0, None) and rc == 0:
        rc = 1
    with open(os.path.join(a.out, "timings.json"), "w") as f:
        json.dump(t, f, indent=2)
    print(json.dumps(t, indent=2))
    s.cast.close()
    return rc


if __name__ == "__main__":
    sys.exit(main())
