#!/usr/bin/env python3
"""Workshop startup bench: launch `workshop` in a PTY and time, from exec, the first byte, the
first frame (the first synchronized-update end marker), the composer ("Ask anything" in the
output), and the engine (state.json `last_phase == ready` under $HOME/.workshop). Standard library
only, Linux and macOS.

    startup_bench.py --bin PATH --home DIR [--runs 5] [--fresh] [--label L] [--out results.jsonl]

`--fresh` empties the home first (the engine installs: expect seconds). Without it the home is
reused as is — run once with `--fresh`, then without, for warm numbers. One JSON line per run.
"""
import argparse
import fcntl
import json
import os
import pty
import select
import shutil
import signal
import struct
import sys
import termios
import time

SYNC_END = b"\x1b[?2026l"
COMPOSER = b"Ask anything"


def now():
    return time.monotonic()


def engine_state(home):
    try:
        with open(os.path.join(home, ".workshop", "engine", "state.json")) as f:
            return json.load(f)
    except Exception:
        return None


def run_once(binary, home, cols, rows, timeout, engine_timeout):
    env = {
        "HOME": home,
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "TERM": "xterm-256color",
        "COLORTERM": "truecolor",
        "LANG": "C.UTF-8",
        "SHELL": "/bin/bash",
        "USER": os.environ.get("USER", "bench"),
        "WORKSHOP_DISABLE_AUTOUPDATER": "1",
    }
    for rel in ("engine/state.json",):
        try:
            os.remove(os.path.join(home, ".workshop", rel))
        except FileNotFoundError:
            pass
    t0 = now()
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(binary, [binary], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    first_output = first_frame = composer = engine_ready = None
    seen_phases = []
    stream = b""
    deadline = t0 + timeout
    engine_deadline = t0 + engine_timeout
    while now() < max(deadline, engine_deadline if composer else deadline):
        r, _, _ = select.select([fd], [], [], 0.005)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            t = now() - t0
            if first_output is None:
                first_output = t
            if first_frame is None and SYNC_END in data:
                first_frame = t
            stream += data
            if composer is None and COMPOSER in stream:
                composer = t
            if len(stream) > 1 << 20:
                stream = stream[-65536:]
        st = engine_state(home)
        if st:
            phase = st.get("last_phase")
            if phase and (not seen_phases or seen_phases[-1][0] != phase):
                seen_phases.append((phase, round(now() - t0, 4)))
            if phase == "ready" and not st.get("last_error") and engine_ready is None:
                engine_ready = now() - t0
            if st.get("last_error") and engine_ready is None:
                engine_ready = -1.0
        if composer is not None and (engine_ready is not None or now() > engine_deadline):
            break
        if composer is None and now() > deadline:
            break
    # quit: /exit, then Ctrl+C twice, then kill
    for keys in (b"/exit\r", b"\x03\x03"):
        try:
            os.write(fd, keys)
        except OSError:
            break
        end = now() + 3
        while now() < end:
            try:
                p, _ = os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                p = pid
            if p:
                break
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    os.read(fd, 65536)
                except OSError:
                    break
        else:
            continue
        break
    try:
        os.kill(pid, signal.SIGKILL)
    except Exception:
        pass
    try:
        os.waitpid(pid, 0)
    except Exception:
        pass
    os.close(fd)
    r = lambda x: None if x is None else round(x, 4)  # noqa: E731
    return dict(first_output=r(first_output), first_frame=r(first_frame), composer=r(composer),
                engine_ready=r(engine_ready) if engine_ready != -1.0 else "error",
                engine_phases=seen_phases)


def median(xs):
    xs = sorted(x for x in xs if isinstance(x, (int, float)))
    if not xs:
        return None
    return xs[len(xs) // 2]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--fresh", action="store_true")
    ap.add_argument("--label", default="")
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--timeout", type=float, default=60)
    ap.add_argument("--engine-timeout", type=float, default=240)
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    if a.fresh:
        shutil.rmtree(a.home, ignore_errors=True)
    os.makedirs(a.home, exist_ok=True)
    results = []
    for i in range(a.runs):
        res = run_once(a.bin, a.home, a.cols, a.rows, a.timeout, a.engine_timeout)
        res.update(label=a.label, bin=a.bin, home_kind="fresh" if a.fresh else "warm",
                   platform=sys.platform, run=i + 1,
                   ts=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
        results.append(res)
        print(json.dumps(res), flush=True)
        if a.out:
            with open(a.out, "a") as f:
                f.write(json.dumps(res) + "\n")
        time.sleep(1)
    meds = {k: median([x[k] for x in results]) for k in ("first_output", "first_frame", "composer", "engine_ready")}
    print(f"median ({a.label or a.bin}, {'fresh' if a.fresh else 'warm'}, n={len(results)}): "
          + ", ".join(f"{k}={v}" for k, v in meds.items()), file=sys.stderr)


if __name__ == "__main__":
    main()
