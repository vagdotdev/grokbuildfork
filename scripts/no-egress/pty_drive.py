#!/usr/bin/env python3
"""Minimal PTY driver: spawn BIN in a pseudo-terminal, send scripted keys, dump raw output.

usage: pty_drive.py --bin PATH --out RAW.log [--env K=V ...] [--cwd DIR] --script "wait:5000,key:l,wait:8000,key:C-c,wait:1000"
Keys: literal single chars, C-c (ctrl-c), Esc, Enter, Tab, Up, Down, Left, Right, or text:<string>
"""
import argparse, os, pty, select, sys, time, fcntl, termios, struct, signal

KEYS = {"Esc": b"\x1b", "Enter": b"\r", "Tab": b"\t", "Up": b"\x1b[A", "Down": b"\x1b[B",
        "Left": b"\x1b[D", "Right": b"\x1b[C", "C-c": b"\x03", "C-d": b"\x04", "C-q": b"\x11", "Space": b" "}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--env", action="append", default=[])
    ap.add_argument("--cwd")
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--script", required=True)
    ap.add_argument("--args", default="")
    a = ap.parse_args()
    env = dict(os.environ)
    for kv in a.env:
        k, _, v = kv.partition("=")
        if v == "" and not kv.endswith("="):
            env.pop(k, None)
        else:
            env[k] = v
    pid, fd = pty.fork()
    if pid == 0:
        if a.cwd:
            os.chdir(a.cwd)
        argv = [a.bin] + ([x for x in a.args.split(" ") if x] if a.args else [])
        os.execve(a.bin, argv, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", a.rows, a.cols, 0, 0))
    out = open(a.out, "wb")
    def pump(ms):
        end = time.time() + ms / 1000.0
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if fd in r:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    return False
                if not data:
                    return False
                out.write(data); out.flush()
                # answer cursor-position / device-attribute queries so the TUI does not stall
                if b"\x1b[6n" in data:
                    os.write(fd, b"\x1b[1;1R")
                if b"\x1b[c" in data or b"\x1b[>c" in data or b"\x1b[>0c" in data:
                    os.write(fd, b"\x1b[?62;c")
        return True
    alive = True
    for step in a.script.split(","):
        step = step.strip()
        if not step:
            continue
        kind, _, val = step.partition(":")
        if kind == "wait":
            alive = pump(int(val))
        elif kind == "key":
            os.write(fd, KEYS.get(val, val.encode()))
        elif kind == "text":
            os.write(fd, val.encode())
        if not alive:
            break
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(0.5)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
    out.close()

if __name__ == "__main__":
    main()
