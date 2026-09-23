#!/usr/bin/env python3
"""Acceptance verifiers: every check is made on disk, by running things, or on the recorded screen —
never on what the model says it did. The criteria are documented per task in the checks below.

  verify.py snap    TASK OUTDIR LABEL   disk snapshot mid-run -> OUTDIR/snaps/LABEL.json
  verify.py probe   TASK OUTDIR NAME    in-session check      -> OUTDIR/probes/NAME.json
  verify.py verify  TASK OUTDIR         all checks            -> OUTDIR/verify.json, verify.txt
  verify.py cleanup TASK OUTDIR         stop what the run left running, collect its logs
  verify.py sheet   OUTDIR [SNAP]       contact sheet of the photos for the by-eye species review

OUTDIR/run.json (written by run.sh) says who ran, with which HOME and PATH.
"""
import csv
import glob
import hashlib
import io
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
import zipfile
from pathlib import Path

MODE, TASK, OUT = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "", Path(sys.argv[3] if len(sys.argv) > 3 else sys.argv[2])
if MODE == "sheet":
    OUT = Path(sys.argv[2])
RUN = json.loads((OUT / "run.json").read_text())
HOME = Path(RUN["home"])
ME = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
OTHER = RUN["user"] != ME
if OTHER and not HOME.exists() and (OUT / "home").exists():  # archived after the run: re-checking files
    HOME, OTHER = OUT / "home", False
IMAGE_EXT = {".jpg", ".jpeg", ".png", ".webp", ".gif", ".heic", ".avif", ".bmp", ".tif", ".tiff"}


# --- running things as the run's user --------------------------------------------------------
def user_env(extra=None):
    env = {"HOME": str(HOME), "USER": RUN["user"], "LOGNAME": RUN["user"], "PATH": RUN["path"],
           "LANG": "C.UTF-8", "SHELL": "/bin/bash", "TERM": "xterm-256color"}
    env.update(DISPLAY=os.environ.get("DISPLAY", ":1"),
               XAUTHORITY=str(HOME / ".Xauthority") if OTHER else os.environ.get("XAUTHORITY", str(Path.home() / ".Xauthority")))
    env.update(extra or {})
    return env


def as_user(cmd, cwd=None, timeout=60, stdin=None, extra=None):
    """Run a bash command line as the run's user in a fresh login-like env. Returns (rc, output)."""
    env = user_env(extra)
    argv = ["bash", "-c", cmd]
    if OTHER:
        argv = ["sudo", "-n", "-u", RUN["user"], "env", "-i"] + [f"{k}={v}" for k, v in env.items()] + argv
        env = None
    try:
        r = subprocess.run(argv, cwd=cwd, env=env, input=stdin, capture_output=True, text=True,
                           timeout=timeout, errors="replace")
        return r.returncode, (r.stdout + r.stderr)
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or b"") + (e.stderr or b"")
        return 124, (out.decode(errors="replace") if isinstance(out, bytes) else out) + f"\n[timed out after {timeout}s]"


def read_bytes(p):
    try:
        return Path(p).read_bytes()
    except PermissionError:
        return subprocess.run(["sudo", "-n", "cat", str(p)], capture_output=True).stdout


def sha(p):
    return hashlib.sha256(read_bytes(p)).hexdigest()


def walk(root, max_depth=6, skip=()):
    """Regular files under root, at most max_depth levels down, never descending into `skip` names."""
    root = Path(root)
    if OTHER:
        r = subprocess.run(["sudo", "-n", "find", str(root), "-maxdepth", str(max_depth), "-type", "f"],
                           capture_output=True, text=True)
        return sorted(p for p in map(Path, r.stdout.splitlines())
                      if not any(part in skip for part in p.relative_to(root).parts))
    files = []
    for d, dirs, names in os.walk(root):
        depth = len(Path(d).relative_to(root).parts)
        dirs[:] = [x for x in dirs if x not in skip and depth + 1 < max_depth]
        files += [Path(d) / n for n in names if (Path(d) / n).is_file()]
    return sorted(files)


# --- what Workshop recorded --------------------------------------------------------------------
def sessions():
    out = []
    for f in walk(HOME / ".workshop/engine/sessions", 1):
        if f.suffix == ".json":
            try:
                out.append(json.loads(read_bytes(f)))
            except ValueError:
                pass
    return sorted(out, key=lambda s: s.get("created_unix", 0))


def turns():
    return [t for s in sessions() for t in s.get("turns", [])]


def turn_text(t, final=False):
    items = t.get("items", [])
    if final:
        last_tool = max((i for i, x in enumerate(items) if x.get("kind") == "tool"), default=-1)
        items = items[last_tool + 1:]
    return "".join(x.get("text", "") for x in items if x.get("kind") == "text").strip()


def events():
    p = OUT / "events.jsonl"
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []


def prompts_log():
    p = OUT / "prompts.log"
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []


NOISE = [re.compile(r"[\u2800-\u28ff]"), re.compile(r"\b\d{1,2}:\d{2}( [AP]M)?\b"),
         re.compile(r"\b\d+(\.\d+)?\s?(ms|s|m|h|sec|min)\b"), re.compile(r"\.{1,3}(?=\s|$)")]


def cast_analysis():
    """Every distinct screen line ever shown (with the cast time it first showed), and the cast times at
    which the screen really changed (spinner frames, clocks and elapsed counters ignored)."""
    import pyte
    p = OUT / "session.cast"
    with open(p, encoding="utf-8", errors="replace") as f:
        hdr = json.loads(f.readline())
        evs = []
        for line in f:
            try:
                evs.append(json.loads(line))
            except ValueError:
                pass
    sc = pyte.Screen(hdr["width"], hdr["height"])
    st = pyte.Stream(sc)
    seen, activity, prev, bucket, last_t = {}, [], None, None, 0.0

    def sample():
        nonlocal prev
        txt = "\n".join(l.rstrip() for l in sc.display)
        for l in txt.split("\n"):
            if l.strip():
                seen.setdefault(l.strip(), last_t)
        for rx in NOISE:
            txt = rx.sub("", txt)
        if txt != prev:
            activity.append(last_t)
            prev = txt

    for t, k, d in evs:
        if k != "o":
            continue
        b = int(t * 5)
        if bucket is not None and b != bucket:
            sample()
        st.feed(d)
        bucket, last_t = b, t
    sample()
    return {"seen": seen, "activity": activity, "end": last_t}


def cast_offset():
    pairs = [tuple(map(float, l.split())) for l in (OUT / "cast-wall-sync.txt").read_text().splitlines() if l.strip()]
    return min(w - t for w, t in pairs) if pairs else 0.0


def turn_windows():
    """(prompt text, cast start, cast end, how it ended) for every prompt typed."""
    off, wins, cur = cast_offset(), [], None
    for e in events():
        if e["ev"] == "prompt_sent":
            cur = (e["text"], e["wall"] - off)
        elif e["ev"] in ("turn_end", "turn_timeout") and cur:
            wins.append((cur[0], cur[1], e["wall"] - off, e["ev"] + ": " + e["text"]))
            cur = None
    if cur:
        wins.append((cur[0], cur[1], None, "no end recorded"))
    return wins


def workshop_spans():
    """Cast-time spans while Workshop was on screen: from each launch to the shell prompt coming back."""
    off, spans, start = cast_offset(), [], None
    for e in events():
        if e["ev"] == "launch" or (e["ev"] == "line" and e["text"].startswith("workshop")):
            start = e["wall"] - off
        elif e["ev"] == "shell_back" and start is not None:
            spans.append((start, e["wall"] - off))
            start = None
    if start is not None:
        spans.append((start, float("inf")))
    return spans


def max_gap(activity, a, b):
    pts = [a] + [t for t in activity if a < t < b] + [b]
    gaps = [(y - x, x) for x, y in zip(pts, pts[1:])]
    return max(gaps) if gaps else (0.0, a)


# --- file checks -------------------------------------------------------------------------------
def image_info(p):
    from PIL import Image, ImageStat
    info = {"path": str(p), "bytes": len(read_bytes(p)), "sha256": sha(p)}
    head = read_bytes(p)[:512].lower()
    if b"<html" in head or b"<!doctype" in head or b"<svg" in head or b"<?xml" in head:
        return {**info, "valid": False, "why": "markup, not an image"}
    try:
        im = Image.open(io.BytesIO(read_bytes(p)))
        fmt = im.format
        im.load()
        w, h = im.size
        sd = ImageStat.Stat(im.convert("L")).stddev[0]
    except Exception as e:  # noqa: BLE001 — any decode failure means "not a usable photo"
        return {**info, "valid": False, "why": f"does not decode: {e}"}
    g = im.convert("L").resize((9, 8))
    px = list(g.getdata())
    info["dhash"] = f"{sum(1 << i for i in range(64) if px[i // 8 * 9 + i % 8] > px[i // 8 * 9 + i % 8 + 1]):016x}"
    info.update(format=fmt, size=[w, h], stddev=round(sd, 1))
    if fmt not in ("JPEG", "PNG", "WEBP", "MPO"):
        return {**info, "valid": False, "why": f"format {fmt}"}
    if min(w, h) < 200:
        return {**info, "valid": False, "why": f"too small {w}x{h}"}
    if sd <= 10:
        return {**info, "valid": False, "why": "flat placeholder"}
    return {**info, "valid": True}


def near_duplicates(imgs, limit=10):
    """Pairs of valid images whose difference hashes are within `limit` bits (the same photo re-encoded,
    resized or lightly cropped)."""
    v = [i for i in imgs if i.get("dhash")]
    return [(Path(a["path"]).name, Path(b["path"]).name) for n, a in enumerate(v) for b in v[n + 1:]
            if bin(int(a["dhash"], 16) ^ int(b["dhash"], 16)).count("1") <= limit]


def image_candidates(root, max_depth=4):
    out = []
    for p in walk(root, max_depth):
        if p.name.startswith("."):
            continue
        head = read_bytes(p)[:16]
        if p.suffix.lower() in IMAGE_EXT or head[:3] == b"\xff\xd8\xff" or head[:8] == b"\x89PNG\r\n\x1a\n" or head[8:12] == b"WEBP":
            out.append(p)
    return out


SPECIES = {
    "lion": {"lion", "leo"}, "tiger": {"tiger", "tigris"}, "leopard": {"leopard", "pardus"},
    "jaguar": {"jaguar", "onca"}, "snow leopard": {"snow leopard", "snowleopard", "uncia"},
}


def species_of(folder):
    n = re.sub(r"[_\-]+", " ", folder.lower()).strip()
    n = re.sub(r"^\d+[\s.)]*", "", n)
    n = re.sub(r"^panthera\s+", "", n)
    n = re.sub(r"\s*\(.*\)$", "", n).strip()
    n = n[:-1] if n.endswith("s") and n[:-1] in {a for v in SPECIES.values() for a in v} else n
    for sp, names in SPECIES.items():
        if n in names or n.replace(" ", "") in names:
            return sp
    return None


def book_info(p):
    data = read_bytes(p)
    info = {"path": str(p), "bytes": len(data)}
    low = data[:4096].lower()
    if len(data) < 20_000:
        return {**info, "valid": False, "why": "under 20 KB"}
    if b"<html" in low or b"<!doctype html" in low:
        return {**info, "valid": False, "why": "an HTML page"}
    if data[:4] == b"PK\x03\x04":
        try:
            z = zipfile.ZipFile(io.BytesIO(data))
            bad = z.testzip()
            mt = z.read("mimetype").decode(errors="replace").strip() if "mimetype" in z.namelist() else ""
            opf = next((n for n in z.namelist() if n.endswith(".opf")), None)
            meta = z.read(opf).decode(errors="replace") if opf else ""
        except Exception as e:  # noqa: BLE001
            return {**info, "valid": False, "why": f"broken zip: {e}"}
        dc = {k: re.findall(rf"<dc:{k}[^>]*>([^<]*)", meta) for k in ("title", "creator", "source", "rights", "publisher")}
        info.update(kind="epub", mimetype=mt, **{k: v[:2] for k, v in dc.items()})
        ok = bad is None and mt == "application/epub+zip" and dc["title"] and dc["creator"]
        info["pd_marker"] = bool(re.search(r"gutenberg|standard ?ebooks|public domain", meta, re.I))
        return {**info, "valid": bool(ok), "why": "" if ok else "not a complete EPUB"}
    if data[:5] == b"%PDF-":
        tmp = Path(tempfile.mkstemp(suffix=".pdf")[1])
        tmp.write_bytes(data)
        r = subprocess.run(["pdfinfo", str(tmp)], capture_output=True, text=True)
        txt = subprocess.run(["pdftotext", "-l", "3", str(tmp), "-"], capture_output=True, text=True).stdout
        tmp.unlink()
        pages = int(m.group(1)) if (m := re.search(r"^Pages:\s+(\d+)", r.stdout, re.M)) else 0
        title = re.search(r"^Title:\s+(.*)$", r.stdout, re.M)
        info.update(kind="pdf", pages=pages, title=[title.group(1)] if title else [],
                    pd_marker=bool(re.search(r"gutenberg|public domain|standard ebooks", r.stdout + txt, re.I)))
        return {**info, "valid": pages >= 10, "why": "" if pages >= 10 else f"{pages} pages"}
    if data[60:68] == b"BOOKMOBI":
        info.update(kind="mobi", pd_marker=b"gutenberg" in data.lower())
        return {**info, "valid": True}
    text = data[:200_000].decode("utf-8", errors="replace")
    if len(data) >= 50_000 and re.search(r"project gutenberg", text[:20000], re.I):
        title = re.search(r"^Title:\s*(.+)$", text, re.M)
        author = re.search(r"^Author:\s*(.+)$", text, re.M)
        info.update(kind="txt", title=[title.group(1).strip()] if title else [],
                    creator=[author.group(1).strip()] if author else [], pd_marker=True)
        return {**info, "valid": True}
    return {**info, "valid": False, "why": "not an EPUB, PDF, MOBI or Gutenberg text"}


def fib_ok(text, n):
    a, b, seq0 = 0, 1, []
    for _ in range(n + 1):
        seq0.append(a)
        a, b = b, a + b
    starts = (seq0[:n], seq0[1:n + 1])
    lines = [l for l in text.splitlines() if l.strip()]
    cands = [[int(x) for x in re.findall(r"-?\d+", text)],
             [int(re.findall(r"-?\d+", l)[-1]) for l in lines if re.findall(r"-?\d+", l)]]
    return any(c == s for c in cands for s in starts)


# --- snapshots and probes ----------------------------------------------------------------------
def listeners():
    """TCP ports listened on by this run's own processes (HOME is the run's), not Workshop's engine."""
    r = subprocess.run(["sudo", "-n", "ss", "-ltnpH"], capture_output=True, text=True)
    ports = set()
    for l in r.stdout.splitlines():
        port = re.search(r":(\d+)\s", l.split(None, 4)[3] + " ")
        pids = re.findall(r'\("([^"]+)",pid=(\d+)', l)
        for name, pid in pids:
            try:
                env = subprocess.run(["sudo", "-n", "cat", f"/proc/{pid}/environ"], capture_output=True).stdout
            except OSError:
                continue
            if port and f"HOME={HOME}\0".encode() in env and name not in ("opencode", "workshop"):
                ports.add(int(port.group(1)))
    return sorted(ports)


def snap(label):
    s = {"label": label, "t": time.time()}
    if label == "before":
        s["listeners"] = listeners()
    elif TASK in ("T2", "T2v"):
        s["images"] = [image_info(p) | {"rel": str(p.relative_to(HOME / "Desktop"))} for p in image_candidates(HOME / "Desktop")]
        s["files"] = [str(p.relative_to(HOME)) for p in walk(HOME / "Desktop", 4)]
        from PIL import Image
        thumbs = OUT / "snaps" / f"{label}-thumbs"
        thumbs.mkdir(parents=True, exist_ok=True)
        for i in s["images"]:
            if i["valid"]:
                im = Image.open(io.BytesIO(read_bytes(i["path"]))).convert("RGB")
                im.thumbnail((480, 480))
                im.save(thumbs / f"{i['sha256'][:16]}.jpg", quality=80)
    elif TASK == "T8":
        s["scripts"] = {}
        keep = OUT / "snaps" / f"{label}-files"
        for p in walk(HOME / "myproject", 3, skip={"venv", ".venv", "node_modules", "__pycache__"}):
            if p.suffix == ".py":
                rc, out = as_user(f"python3 '{p}'", cwd=str(p.parent), timeout=15, stdin="")
                s["scripts"][str(p)] = {"sha256": sha(p), "rc": rc, "out": out[-3000:], "fib20": fib_ok(out, 20)}
                (keep / p.relative_to(HOME)).parent.mkdir(parents=True, exist_ok=True)
                (keep / p.relative_to(HOME)).write_bytes(read_bytes(p))
    elif TASK == "T1":
        s["ibooks"] = [str(p.relative_to(HOME)) for p in walk(HOME / "Desktop", 3)]
        s["ghostty"] = as_user("command -v ghostty; ghostty --version 2>&1 | head -2")[1]
    else:
        s["files"] = [str(p.relative_to(HOME)) for p in walk(HOME, 3, skip={".workshop", ".cache", ".npm", ".local", "node_modules"})]
    (OUT / "snaps" / f"{label}.json").write_text(json.dumps(s, indent=1))


def http_get(url, timeout=6):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return r.status, r.headers.get("content-type", ""), r.read(400_000).decode(errors="replace")
    except Exception as e:  # noqa: BLE001
        return None, "", str(e)


def probe(name):
    p = {"name": name, "t": time.time()}
    if TASK == "T3":
        before = set(json.loads((OUT / "snaps/before.json").read_text())["listeners"])
        p["new_listeners"] = [x for x in listeners() if x not in before]
        ts = turns()
        final = turn_text(ts[-1]) if ts else ""
        urls = sorted(set(re.findall(r"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0)(?::\d+)?[^\s)\]'\"`<>*]*", final)))
        p["answer_urls"] = urls
        targets = urls or [f"http://127.0.0.1:{x}/" for x in p["new_listeners"]]
        p["fetch"] = []
        for u in targets:
            status, ctype, body = http_get(u.rstrip(".,"))
            p["fetch"].append({"url": u, "status": status, "type": ctype, "bytes": len(body), "head": body[:300],
                               "input": "<input" in body.lower(), "button": bool(re.search(r"<button|<form|type=.submit", body, re.I)),
                               "script": "<script" in body.lower(), "todo": bool(re.search(r"to-?do", body, re.I))})
        if name == "t3-server" and targets:
            (OUT / "probes" / f"{name}.json").write_text(json.dumps(p, indent=1))
            shot = OUT / "probes" / "t3-page.png"
            # headless Chrome writes the shot and then may not exit: give it 30 s, then kill it
            chrome = subprocess.Popen(["google-chrome", "--headless=new", "--no-sandbox", "--disable-gpu", "--hide-scrollbars",
                                       f"--user-data-dir={tempfile.mkdtemp(prefix='acc-chrome-')}", f"--screenshot={shot}",
                                       "--window-size=1200,900", targets[0]], stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL, start_new_session=True)
            try:
                chrome.wait(30)
            except subprocess.TimeoutExpired:
                os.killpg(chrome.pid, signal.SIGKILL)
            p["screenshot"] = str(shot) if shot.exists() else None
    elif TASK == "T1" and name == "ghostty-launch":
        rc, where = as_user("command -v ghostty")
        p["which"] = where.strip()
        if rc == 0:
            env = user_env()
            argv = ["bash", "-c", "exec ghostty"]
            if OTHER:
                argv = ["sudo", "-n", "-u", RUN["user"], "env", "-i"] + [f"{k}={v}" for k, v in env.items()] + argv
                env = None
            before = set(subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "ghostty"],
                                        capture_output=True, text=True).stdout.split())
            proc = subprocess.Popen(argv, env=env, cwd=str(HOME), stdout=subprocess.DEVNULL,
                                    stderr=open(OUT / "probes/ghostty-stderr.txt", "w"), start_new_session=True)
            t0, wid = time.time(), ""
            while time.time() - t0 < 15 and not wid:
                time.sleep(0.5)
                r = subprocess.run(["xdotool", "search", "--onlyvisible", "--class", "ghostty"], capture_output=True, text=True)
                wid = next((w for w in r.stdout.split() if w not in before), "")
            p["window_after_s"] = round(time.time() - t0, 1) if wid else None
            p["window"] = wid
            if wid:
                time.sleep(2)
                subprocess.run(["scrot", "-o", str(OUT / "probes/ghostty-window.png")], capture_output=True)
                p["title"] = subprocess.run(["xdotool", "getwindowname", wid], capture_output=True, text=True).stdout.strip()
            p["alive"] = proc.poll() is None
            if OTHER:
                subprocess.run(["sudo", "-n", "pkill", "-u", RUN["user"], "-x", "ghostty"], capture_output=True)
            else:
                os.killpg(proc.pid, signal.SIGTERM)
    (OUT / "probes" / f"{name}.json").write_text(json.dumps(p, indent=1))


# --- the checks --------------------------------------------------------------------------------
class Checks:
    def __init__(self):
        self.items = []

    def add(self, cid, ok, label, evidence=""):
        self.items.append({"id": cid, "ok": ok, "label": label, "evidence": evidence})
        return ok


def load_snap(label):
    p = OUT / "snaps" / f"{label}.json"
    return json.loads(p.read_text()) if p.exists() else {}


def load_probe(name):
    p = OUT / "probes" / f"{name}.json"
    return json.loads(p.read_text()) if p.exists() else {}


def sorted_panthera(c, expected_shas):
    desk = HOME / "Desktop"
    genus = [d for d in (Path(x) for x in glob.glob(str(desk / "*"))) if d.name.lower() == "panthera"]
    root = genus[0] if genus else None
    by_species, bad_folders, extra_files = {}, [], []
    if root:
        for d in sorted(Path(x) for x in glob.glob(str(root / "*"))):
            imgs = [image_info(p) for p in image_candidates(d, 2)]
            sp = species_of(d.name) if d.is_dir() else None
            if sp:
                by_species.setdefault(sp, []).extend(imgs)
            elif imgs or d.is_dir():
                bad_folders.append(d.name)
            else:
                extra_files.append(d.name)
    ok_counts = root is not None and set(by_species) == set(SPECIES) and not bad_folders and all(
        len(v) == 3 and all(i["valid"] for i in v) for v in by_species.values())
    summary = {sp: [f"{Path(i['path']).name}{'' if i['valid'] else ' (INVALID: ' + i['why'] + ')'}" for i in v] for sp, v in by_species.items()}
    c.add("T2.2", ok_counts, "photos sorted into Desktop/Panthera/<species>/ with 3 valid images each",
          json.dumps({"genus_folder": str(root.relative_to(HOME)) if root else None, "species": summary,
                      "unrecognised": bad_folders, "other_files": extra_files}, ensure_ascii=False))
    sorted_imgs = [i for v in by_species.values() for i in v]
    sorted_shas = {i["sha256"]: sp for sp, v in by_species.items() for i in v}
    outside = [i for i in (image_info(p) for p in image_candidates(desk)) if not root or not i["path"].startswith(str(root) + "/")]
    stray = [str(Path(i["path"]).relative_to(desk)) for i in outside]
    dups = near_duplicates(sorted_imgs)
    rev = OUT / "species-review.json"
    dups += json.loads(rev.read_text()).get("duplicates", []) if rev.exists() else []
    replaced = len(set(expected_shas) - set(sorted_shas)) if expected_shas else None
    c.add("T2.3", bool(sorted_shas) and not stray and len(sorted_shas) == len(sorted_imgs) and not dups,
          "nothing left loose, no photo twice (same file or near-duplicate)",
          json.dumps({"loose_outside_genus": stray, "sorted": len(sorted_imgs), "distinct": len(sorted_shas),
                      "near_duplicates": dups, "turn1_photos_replaced": replaced}))
    return sorted_shas


def verify():
    c = Checks()
    ts = turns()
    ev = events()
    ca = cast_analysis() if (OUT / "session.cast").exists() else {"seen": {}, "activity": [], "end": 0}
    spans = workshop_spans()
    seen = {l for l, t in ca["seen"].items() if any(a <= t <= b for a, b in spans)}
    wins = turn_windows()
    metrics = {"turns_recorded": len(ts), "windows": []}
    for text, a, b, how in wins:
        gap, at = max_gap(ca["activity"], a, b if b is not None else ca["end"])
        metrics["windows"].append({"prompt": text[:80], "seconds": round((b or ca["end"]) - a, 1), "ended": how,
                                   "longest_still_s": round(gap, 1), "still_from_cast_t": round(at, 1)})
    home = str(HOME)

    if TASK == "T1":
        who = turn_text(ts[0]) if ts else ""
        c.add("T1.1", bool(re.search(r"workshop", who, re.I)) and not re.search(r"opencode|anomaly|grok|\bxai\b|x\.ai|claude code|chatgpt", who, re.I),
              "answers as Workshop", who[:300])
        thought = sorted(l for l in seen if re.search(r"Thought for|◆ Thought", l))
        c.add("T1.2", not thought, "no thinking shown", "; ".join(thought[:3]))
        books = [book_info(p) for p in walk(HOME / "Desktop/iBooks", 2) if not p.name.startswith(".")]
        valid = [b for b in books if b["valid"]]
        c.add("T1.3", (HOME / "Desktop/iBooks").exists() and len(valid) >= 2, "Desktop/iBooks holds 2 valid books",
              json.dumps([{k: b.get(k) for k in ("path", "bytes", "kind", "valid", "why", "title", "creator")} for b in books], ensure_ascii=False))
        c.add("T1.4", len([b for b in valid if b.get("pd_marker")]) >= 2, "both are public domain (Gutenberg / Standard Ebooks provenance)",
              json.dumps([{"file": Path(b["path"]).name, "title": b.get("title"), "creator": b.get("creator"),
                           "source": b.get("source"), "rights": b.get("rights"), "pd_marker": b.get("pd_marker")} for b in valid], ensure_ascii=False))
        rc, out = as_user("command -v ghostty && ghostty --version | cat")
        c.add("T1.5", rc == 0 and "Ghostty" in out, "Ghostty installed (in the user's PATH, --version works)", out.strip()[:300])
        g = load_probe("ghostty-launch")
        c.add("T1.6", bool(g.get("window")), "Ghostty launches (window within 15 s)",
              json.dumps({k: g.get(k) for k in ("which", "window_after_s", "title", "alive")}))

    elif TASK in ("T2", "T2v"):
        t1 = load_snap("turn1") if TASK == "T2" else {}
        if TASK == "T2":
            imgs = t1.get("images", [])
            valid = {i["sha256"] for i in imgs if i["valid"]}
            dups = near_duplicates([i for i in imgs if i["valid"]])
            rev = OUT / "species-review.json"
            dups += json.loads(rev.read_text()).get("turn1_duplicates", []) if rev.exists() else []
            c.add("T2.1", len(imgs) == 15 and len(valid) == 15 and not dups, "after prompt 1: 15 valid, distinct photos on the Desktop",
                  json.dumps({"candidates": len(imgs), "valid_distinct": len(valid), "near_duplicates": dups,
                              "invalid": [(i["rel"], i["why"]) for i in imgs if not i["valid"]]}))
            expected = valid
        else:
            expected = set(json.loads((OUT / "fixture.json").read_text())["photos"])
        sorted_shas = sorted_panthera(c, expected)
        if TASK == "T2v":
            truth = json.loads((OUT / "fixture.json").read_text())["photos"]
            wrong = [(truth[s]["name"], truth[s]["species"], sp) for s, sp in sorted_shas.items() if s in truth and truth[s]["species"] != sp]
            right = sum(1 for s, sp in sorted_shas.items() if s in truth and truth[s]["species"] == sp)
            c.add("T2v.4", len(sorted_shas) == 15 and not wrong and right == 15, "every photo is in its true species folder (fixture ground truth)",
                  json.dumps({"right": right, "wrong (file, is, put in)": wrong}))
        else:
            rev = OUT / "species-review.json"
            if rev.exists():
                r = json.loads(rev.read_text()).get("photos", {})
                bad = [(v.get("file"), sp, v.get("shows"), v.get("verdict")) for s, sp in sorted_shas.items()
                       if (v := r.get(s, {})).get("verdict") != "correct" or v.get("shows") != sp]
                c.add("T2.4", bool(sorted_shas) and not bad, "each photo shows its folder's species (reviewed by eye)",
                      json.dumps({"reviewed": len(r), "not correct (file, folder, shows, verdict)": bad}))
            else:
                c.add("T2.4", None if sorted_shas else False, "each photo shows its folder's species (reviewed by eye)",
                      "pending: review contact-sheet.jpg into species-review.json" if sorted_shas else "nothing sorted")

    elif TASK == "T3":
        p1, p2 = load_probe("t3-server"), load_probe("t3-server-later")
        up = [f for f in p1.get("fetch", []) if f["status"] == 200 and "html" in f["type"]]
        later = [f for f in p2.get("fetch", []) if f["status"] == 200 and "html" in f["type"]]
        c.add("T3.1", bool(p1.get("new_listeners")) and bool(later), "a local server is running after the turn, and 30 s later",
              json.dumps({"new_listeners": p1.get("new_listeners"), "still_up_30s": [f["url"] for f in later]}))
        c.add("T3.2", bool(p1.get("answer_urls")) and len(up) == len(p1.get("fetch", [])) and bool(up),
              "the address in the answer opens (HTTP 200 HTML)",
              json.dumps({"answer_urls": p1.get("answer_urls"), "fetch": [(f["url"], f["status"], f["type"]) for f in p1.get("fetch", [])]}))
        page = up[0] if up else {}
        c.add("T3.3", all(page.get(k) for k in ("input", "button", "script", "todo")), "it is a to-do page (input, button/form, script, to-do text)",
              json.dumps({k: page.get(k) for k in ("input", "button", "script", "todo", "bytes")} | {"screenshot": p1.get("screenshot")}))

    elif TASK == "T4":
        fx = json.loads((OUT / "fixture.json").read_text())
        repo = HOME / "projects/more-itertools"
        rc, out = as_user("python3 -m unittest discover -s tests -t . 2>&1 | tail -4", cwd=str(repo), timeout=600)
        ran = int(m.group(1)) if (m := re.search(r"Ran (\d+) tests", out)) else 0
        c.add("T4.1", bool(re.search(r"^OK", out, re.M)) and ran == fx["baseline"]["ran"], "the whole suite passes (same test count)",
              f"ran {ran} (baseline {fx['baseline']['ran']}): {out.strip()[-200:]}")
        _, tdiff = as_user(f"git diff {fx['head']} --stat -- tests/; git status --porcelain -- tests/", cwd=str(repo))
        c.add("T4.2", not tdiff.strip(), "the tests are untouched", tdiff.strip()[:300])
        _, diff = as_user(f"git diff -U0 {fx['head']} -- more_itertools/more.py", cwd=str(repo))
        src = read_bytes(repo / "more_itertools/more.py").decode()
        lines = src.splitlines()
        start = next((i + 1 for i, l in enumerate(lines) if l.startswith("def ilen(")), 0)
        end = next((i + 1 for i, l in enumerate(lines[start:], start) if l.startswith("def ")), len(lines))
        hunks = [(int(m.group(1)), int(m.group(2) or 1)) for m in re.finditer(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", diff, re.M)]
        touches = any(a <= end and a + max(n, 1) - 1 >= start for a, n in hunks)
        _, names = as_user(f"git diff --name-only {fx['head']}", cwd=str(repo))
        c.add("T4.3", touches, "the fix is in ilen() in more_itertools/more.py", f"changed files: {names.split()}; ilen lines {start}-{end}; hunks {hunks}")

    elif TASK == "T5":
        rc, out = as_user("dpkg -s ffmpeg 2>&1 | grep -m1 '^Status'; command -v ffmpeg; ffmpeg -version 2>&1 | head -1")
        c.add("T5.1", "install ok installed" in out and "/usr/bin/ffmpeg" in out and "ffmpeg version" in out,
              "ffmpeg installed with the package manager", out.strip())
        gifs = [p for p in walk(HOME / "Desktop", 2) if read_bytes(p)[:4] == b"GIF8"]
        c.add("T5.2", bool(gifs), "a GIF is on the Desktop", ", ".join(str(p.relative_to(HOME)) for p in gifs) or
              ", ".join(str(p.relative_to(HOME)) for p in walk(HOME / "Desktop", 2)))
        from PIL import Image
        best = {}
        for g in gifs:
            im = Image.open(io.BytesIO(read_bytes(g)))
            dur, n = 0, 0
            try:
                while True:
                    dur += im.info.get("duration", 0)
                    n += 1
                    im.seek(im.tell() + 1)
            except EOFError:
                pass
            best = {"file": g.name, "frames": n, "seconds": round(dur / 1000, 2), "size": list(im.size)}
            if n >= 10 and abs(dur / 1000 - 5) <= 1.5 and 100 <= im.size[0] <= 640:
                break
        c.add("T5.3", bool(best) and best["frames"] >= 10 and abs(best["seconds"] - 5) <= 1.5 and 100 <= best["size"][0] <= 640,
              "it is the 5 s clip, animated", json.dumps(best))

    elif TASK == "T6":
        started = RUN["started"]
        skip = {".workshop", ".local", ".cache", ".npm", ".config", "node_modules", "venv", ".venv", "site-packages", "__pycache__"}
        scripts = [p for p in walk(HOME, 4, skip=skip) if p.suffix in (".py", ".js", ".mjs", ".sh", ".ts")
                   and p.stat().st_mtime > started and re.search(rb"https?://", read_bytes(p))]
        c.add("T6.1", bool(scripts), "a script that calls an HTTP API exists", ", ".join(str(p.relative_to(HOME)) for p in scripts))
        csvs = [p for p in walk(HOME / "Desktop", 2) if p.suffix.lower() == ".csv"]
        rows, header, cities = [], [], {}
        if csvs:
            rows = list(csv.reader(io.StringIO(read_bytes(csvs[0]).decode(errors="replace"))))
            header = [h.lower() for h in rows[0]] if rows else []
            for r in rows[1:]:
                for city in ("london", "paris", "tokyo"):
                    if any(city in x.lower() for x in r):
                        cities.setdefault(city, r)
        c.add("T6.2", bool(csvs) and len(rows) >= 4 and len(cities) == 3, "a CSV on the Desktop with London, Paris and Tokyo rows",
              json.dumps({"csv": [str(p.relative_to(HOME)) for p in csvs], "header": header, "rows": rows[1:6]}, ensure_ascii=False))
        coords = {"london": (51.5074, -0.1278), "paris": (48.8566, 2.3522), "tokyo": (35.6762, 139.6503)}
        tcols = [i for i, h in enumerate(header) if "temp" in h]
        near = {}
        for city, r in cities.items():
            vals = [float(x) for i, x in enumerate(r) if (not tcols or i in tcols) and re.fullmatch(r"-?\d+(\.\d+)?", x.strip())]
            vals = [v for v in vals if -40 <= v <= 50]
            lat, lon = coords[city]
            st, _, body = http_get(f"https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m"
                                   "&daily=temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=1", 15)
            live = []
            if st == 200:
                j = json.loads(body)
                live = [j["current"]["temperature_2m"], j["daily"]["temperature_2m_max"][0], j["daily"]["temperature_2m_min"][0]]
            near[city] = {"csv_values": vals, "open_meteo_now_max_min": live,
                          "ok": any(abs(v - w) <= 8 for v in vals for w in live) if live else None}
        c.add("T6.3", len(near) == 3 and all(v["ok"] for v in near.values()), "the temperatures are real (within 8 °C of Open-Meteo)", json.dumps(near))
        rerun = {}
        if scripts and csvs:
            s = scripts[0]
            venv = next((str(d / v / "bin/python") for d in [s.parent, *s.parents] if str(d).startswith(home)
                         for v in ("venv", ".venv") if (d / v / "bin/python").exists()), None)
            interp = {".py": venv or "python3", ".js": "node", ".mjs": "node", ".ts": "npx -y tsx", ".sh": "bash"}[s.suffix]
            before = max(p.stat().st_mtime for p in walk(HOME / "Desktop", 2) if p.suffix.lower() == ".csv")
            time.sleep(1.1)
            for attempt in (1, 2):  # a network hiccup on the API is not the script's fault: one retry
                rc, out = as_user(f"{interp} '{s}'", cwd=str(s.parent), timeout=60)
                after = [p for p in walk(HOME / "Desktop", 2) if p.suffix.lower() == ".csv" and p.stat().st_mtime > before]
                if rc == 0 and after:
                    break
                time.sleep(10)
            rerun = {"cmd": f"{interp} {s.relative_to(HOME)}", "rc": rc, "rewrote": [p.name for p in after], "out": out.strip()[-300:]}
        c.add("T6.4", rerun.get("rc") == 0 and bool(rerun.get("rewrote")), "re-running the script works and rewrites the CSV", json.dumps(rerun))

    elif TASK == "T7":
        fx = json.loads((OUT / "fixture.json").read_text())["files"]
        dl = HOME / "Downloads"
        found = {}
        for p in walk(dl, 5):
            found.setdefault(sha(p), []).append(p.relative_to(dl))
        lost = [v["name"] for s, v in fx.items() if len(found.get(s, [])) != 1]
        extra = [str(p) for s, ps in found.items() if s not in fx for p in ps]
        c.add("T7.1", not lost, "nothing lost or duplicated", json.dumps({"missing_or_duplicated": lost, "other_files": extra[:10]}))
        syn = {"images": {"image", "images", "photos", "pictures", "pics"}, "documents": {"document", "documents", "docs"},
               "music": {"music", "audio", "songs"}, "videos": {"video", "videos", "movies"},
               "archives": {"archive", "archives", "compressed", "zips"}, "other": {"other", "others", "misc", "miscellaneous"}}
        wrong, names = [], []
        for s, v in fx.items():
            for rel in found.get(s, []):
                cat = next((k for k, names_ in syn.items() if len(rel.parts) == 2 and rel.parts[0].lower() in names_), None)
                if cat not in v["categories"]:
                    wrong.append(f"{v['name']} -> {rel}")
                n = rel.name
                ext = ".tar.gz" if v["name"].endswith(".tar.gz") else Path(v["name"]).suffix.lower()
                if n != n.lower() or " " in n or not n.endswith(ext):
                    names.append(f"{v['name']} -> {n}")
        c.add("T7.2", not wrong and not lost, "every file is in the folder for its type", "; ".join(wrong[:10]))
        c.add("T7.3", not names and not lost, "clean lowercase names without spaces, extension kept", "; ".join(names[:10]))

    elif TASK == "T8":
        t1 = load_snap("turn1").get("scripts", {})
        first = next((p for p, v in t1.items() if v["fib20"]), None)
        c.add("T8.1", bool(first), "turn 1: a script prints the first 20 Fibonacci numbers",
              json.dumps({p: {"rc": v["rc"], "fib20": v["fib20"], "out": v["out"][:120]} for p, v in t1.items()}))
        pys = [p for p in walk(HOME / "myproject", 3, skip={"venv", ".venv", "__pycache__"}) if p.suffix == ".py"]
        others = [str(p) for p in pys if str(p) != first and re.search(rb"fib", read_bytes(p), re.I)]
        changed = bool(first) and Path(first).exists() and sha(first) != t1[first]["sha256"]
        c.add("T8.2", changed and not others, "turn 2 changed that same script (no second Fibonacci script)",
              json.dumps({"script": first, "changed": changed, "other_fib_scripts": others}))
        res = {}
        if first and Path(first).exists():
            rc, out = as_user(f"python3 '{first}' 7", cwd=str(Path(first).parent), timeout=15, stdin="")
            res = {"argv": [rc, out.strip()[:200], fib_ok(out, 7)]}
            if not res["argv"][2]:
                rc, out = as_user(f"python3 '{first}'", cwd=str(Path(first).parent), timeout=15, stdin="7\n")
                res["stdin"] = [rc, out.strip()[-200:], fib_ok(out.split(":")[-1] if ":" in out else out, 7)]
        c.add("T8.3", any(v[2] for v in res.values()), "a count of 7 prints exactly 7 Fibonacci numbers", json.dumps(res))
        f30 = [p for p in walk(HOME, 4, skip={".workshop", ".local", ".cache"}) if p.name == "fib30.txt"]
        body = read_bytes(f30[0]).decode(errors="replace") if f30 else ""
        c.add("T8.4", bool(f30) and fib_ok(body, 30), "fib30.txt holds the first 30 Fibonacci numbers",
              f"{[str(p.relative_to(HOME)) for p in f30]}: {body[:160]!r}")

    elif TASK == "T9":
        p1 = next((e["text"] for e in ev if e["ev"] == "prompt_sent"), "")
        working = next((e for e in ev if e["ev"] in ("waitre_ok", "waitre_timeout") and e["text"].startswith("◆")), None)
        first = ts[0] if ts else {}
        c.add("T9.0", bool(working) and working["ev"] == "waitre_ok", "the model was at work (a tool row on screen) when the user quit",
              json.dumps({"tool_row": working["ev"] if working else None, "turn1_tools": sum(1 for i in first.get("items", []) if i.get("kind") == "tool"),
                          "turn1_text_end": turn_text(first)[-160:] if first else ""}))
        resumed_at = next((i for i, e in enumerate(ev) if e["ev"] == "line" and e["text"].startswith("workshop -c")), None)
        replay = [e for e in ev[resumed_at or 0:] if e["ev"] in ("waitre_ok", "waitre_timeout")] if resumed_at is not None else []
        c.add("T9.1", bool(replay) and replay[0]["ev"] == "waitre_ok", "workshop -c shows the earlier conversation within 10 s",
              json.dumps(replay[:1]))
        both = [s["id"] for s in sessions() if {t.get("prompt") for t in s.get("turns", [])} >= {p1, "please continue where you left off"}]
        c.add("T9.2", bool(both), "the resumed turn is in the same engine session as prompt 1",
              json.dumps({"sessions": [(s["id"], [t.get("prompt", "")[:40] for t in s.get("turns", [])]) for s in sessions()]}))
        proj = HOME / "myproject"
        res = {}
        if (proj / "todo.py").exists():
            scratch = Path(tempfile.mkdtemp(prefix="acc-t9-"))
            scratch.chmod(0o755)
            shutil.copytree(proj, scratch / "p", ignore=shutil.ignore_patterns("todos.json", ".git"))
            if OTHER:
                subprocess.run(["sudo", "chown", "-R", RUN["user"], str(scratch)])
            steps = [("add", 'add "buy milk"'), ("list", "list"), ("done", "done 1")]
            for k, a in steps:
                rc, out = as_user(f"python3 todo.py {a}", cwd=str(scratch / "p"), timeout=15, stdin="")
                if k == "done" and rc != 0:
                    rc, out = as_user("python3 todo.py done 0", cwd=str(scratch / "p"), timeout=15, stdin="")
                res[k] = [rc, out.strip()[:200]]
            j = scratch / "p/todos.json"
            try:
                data = json.loads(read_bytes(j))
                flat = json.dumps(data).lower()
                res["json"] = flat[:300]
                res["json_ok"] = ("buy milk" not in flat) or bool(re.search(r'"(done|completed|complete|status)":\s*(true|"done"|"completed")', flat))
            except (ValueError, FileNotFoundError) as e:
                res["json"], res["json_ok"] = str(e), False
            subprocess.run(["sudo", "-n", "rm", "-rf", str(scratch)] if OTHER else ["rm", "-rf", str(scratch)])
        ok = bool(res) and all(res[k][0] == 0 for k in ("add", "list", "done")) and "buy milk" in res["list"][1].lower() and res.get("json_ok")
        c.add("T9.3", bool(ok), "the todo app works: add, list, done, valid todos.json", json.dumps(res))

    elif TASK == "T10":
        app = HOME / "myproject/my-app"
        pkg = json.loads(read_bytes(app / "package.json") or b"{}") if (app / "package.json").exists() else {}
        deps = {**pkg.get("dependencies", {}), **pkg.get("devDependencies", {})}
        mods = all((app / "node_modules" / m).exists() for m in ("react", "vite"))
        c.add("T10.1", "react" in deps and "vite" in deps and mods, "my-app exists with react and vite installed",
              json.dumps({"deps": sorted(deps)[:20], "node_modules": mods}))
        rc, out = as_user("rm -rf dist && npm run build 2>&1 | tail -6", cwd=str(app), timeout=300) if app.exists() else (1, "no my-app")
        c.add("T10.2", rc == 0 and (app / "dist/index.html").exists(), "npm run build works", out.strip()[-300:])
        gaps = [w["longest_still_s"] for w in metrics["windows"]]
        c.add("T10.3", bool(gaps) and max(gaps) <= 30, "the screen never looks frozen for more than 30 s", json.dumps(metrics["windows"]))

    elif TASK == "T11":
        w = metrics["windows"]
        c.add("T11.1", bool(w) and w[0]["ended"].startswith("turn_end") and w[0]["seconds"] <= 300 and w[0]["longest_still_s"] <= 60,
              "never hangs silently (turn ends within 5 min, never still for over 60 s)", json.dumps(w))
        rc, out = as_user("dpkg -s htop 2>/dev/null | grep -m1 '^Status'")
        final = turn_text(ts[-1]) if ts else ""
        handoff = bool(re.search(r"sudo apt(-get)? install (-y )?htop", final)) and bool(re.search(r"password", final, re.I))
        pw = [p for p in prompts_log() if p["kind"].startswith("password")]
        c.add("T11.2", "install ok installed" in out or handoff, "htop installed, or a clear hand-off (exact command + password)",
              json.dumps({"dpkg": out.strip(), "handoff": handoff, "final_answer": final[-400:], "password_prompts": pw}))
        echo = [e for e in ev if e["ev"] in ("probe_ok", "probe_fail")]
        final_screen = (OUT / "probes/final-screen.txt").read_text() if (OUT / "probes/final-screen.txt").exists() else ""
        c.add("T11.3", bool(echo) and echo[-1]["ev"] == "probe_ok" and not re.search(r"\[sudo\] password", final_screen),
              "the screen stays usable (composer echoes, no stray password prompt)", json.dumps(echo))

    elif TASK == "TV":
        truth = json.loads((OUT / "fixture.json").read_text())["photos"]
        final = turn_text(ts[-1]) if ts else ""
        low = final.lower()
        spots = {name: [m.start() for m in re.finditer(r"snow[\s_-]?leopard" if sp == "snow leopard" else rf"(?<!snow[\s_-]){sp}", low)]
                 for name, sp in truth.items()}
        read = sorted({Path(str(i.get("input", {}).get("filePath") or i.get("input", {}).get("path") or "")).name
                       for t in ts for i in t.get("items", []) if i.get("kind") == "tool" and i.get("name") == "read"})
        c.add("TV.1", all(read.count(n) for n in truth), "the model opened each photo (read tool)", json.dumps(read))
        order = [min(v) if v else None for v in spots.values()]
        c.add("TV.2", all(o is not None for o in order) and order == sorted(order), "it names the right animal for each photo, in order",
              json.dumps({"truth": truth, "answer": final[:300], "model": RUN.get("model")}))

    # U: every run
    approvals = [p for p in prompts_log() if p["kind"] == "approval_shown"]
    mode = next((e["text"] for e in ev if e["ev"] == "mode_on_screen"), "")
    c.add("U0", mode == "always-approve", "starts in always-approve (shown on screen)", mode)
    c.add("U1", not approvals, "no approval prompt in always-approve", json.dumps([a["text"] for a in approvals]))
    stall_limit = RUN.get("stall", 120)
    ended = [w for w in metrics["windows"] if w["ended"].startswith("turn_end")]
    c.add("U2", bool(wins) and len(ended) == len(metrics["windows"]) and all(w["longest_still_s"] <= stall_limit for w in metrics["windows"]),
          f"every turn ends, none still for over {stall_limit} s", json.dumps(metrics["windows"]))
    fails = sorted(l for l in seen if "Couldn't reach" in l)
    c.add("U3", not fails, "no failure line", "; ".join(fails[:3]))
    plumbing = sorted(l for l in seen if re.search(r"OpenCode|opencode|\bKilo\b|\bGrok\b|\bengine\b|ses_[A-Za-z0-9]{10,}", l))
    c.add("U4", not plumbing, "no plumbing words on screen (reported only)", " | ".join(plumbing[:8]))

    task_checks = [x for x in c.items if not x["id"].startswith("U")]
    gate = task_checks + [x for x in c.items if x["id"] in ("U1", "U2", "U3")]
    passed = all(x["ok"] is True for x in gate)
    pending = any(x["ok"] is None for x in gate) and all(x["ok"] is not False for x in gate)
    result = {"task": TASK, "run": RUN["run_id"], "pass": passed, "pending": pending, "checks": c.items, "metrics": metrics,
              "turns": [{"prompt": t.get("prompt"), "tools": sum(1 for i in t.get("items", []) if i.get("kind") == "tool"),
                         "final_text": turn_text(t, final=True)[-600:]} for t in ts]}
    (OUT / "verify.json").write_text(json.dumps(result, indent=1, ensure_ascii=False))
    lines = [f"# {TASK} {RUN['run_id']}", ""]
    for x in c.items:
        mark = {True: "PASS", False: "FAIL", None: "PENDING"}[x["ok"]]
        lines.append(f"{mark:7} {x['id']:6} {x['label']}")
        if x["evidence"]:
            lines.append(f"        {x['evidence'][:600]}")
    failed = [x["id"] for x in gate if x["ok"] is not True]
    verdict = "PASS" if passed else ("PENDING" if pending else "FAIL")
    lines.append(f"RESULT {TASK} {RUN['run_id']}: {verdict}" + (f" (not passed: {', '.join(failed)})" if failed else ""))
    (OUT / "verify.txt").write_text("\n".join(lines) + "\n")
    print(lines[-1])


def sheet(label=None):
    """A contact sheet of the photos (sorted ones by folder, else the Desktop), for the by-eye review."""
    from PIL import Image, ImageDraw
    desk = HOME / "Desktop"
    imgs = [image_info(p) for p in image_candidates(desk)]
    imgs = [i for i in imgs if i["valid"]]
    imgs.sort(key=lambda i: i["path"])
    tile, cols = 300, 5
    rows = (len(imgs) + cols - 1) // cols or 1
    out = Image.new("RGB", (cols * tile, rows * (tile + 40)), "white")
    d = ImageDraw.Draw(out)
    index = []
    for n, i in enumerate(imgs):
        im = Image.open(io.BytesIO(read_bytes(i["path"]))).convert("RGB")
        im.thumbnail((tile - 8, tile - 8))
        x, y = (n % cols) * tile, (n // cols) * (tile + 40)
        out.paste(im, (x + 4, y + 4))
        rel = str(Path(i["path"]).relative_to(desk))
        d.text((x + 6, y + tile + 4), f"{n + 1}. {rel}"[:46], fill="black")
        index.append({"n": n + 1, "file": rel, "sha256": i["sha256"]})
    out.save(OUT / "contact-sheet.jpg", quality=85)
    (OUT / "contact-sheet.json").write_text(json.dumps(index, indent=1))
    print(f"contact sheet: {len(imgs)} photos")


def cleanup():
    """Stop anything the run left running (servers, engines) and keep its logs."""
    dst = OUT / "workshop-home"
    shutil.rmtree(dst, ignore_errors=True)
    for rel in (".workshop/logs", ".workshop/engine", ".workshop/config.toml", ".workshop/active-connection.json",
                ".local/share/opencode/log"):
        src = HOME / rel
        target = dst / rel.replace(".local/share/opencode/log", "opencode-log").replace(".workshop/", "")
        target.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["sudo", "-n", "cp", "-r", str(src), str(target)] if OTHER else ["cp", "-r", str(src), str(target)], capture_output=True)
    if OTHER:
        subprocess.run(["sudo", "-n", "chown", "-R", ME, str(dst)])
    home = str(HOME)
    for pid in os.listdir("/proc"):
        if not pid.isdigit() or int(pid) == os.getpid():
            continue
        try:
            cwd = os.readlink(f"/proc/{pid}/cwd")
            env = open(f"/proc/{pid}/environ", "rb").read()
        except OSError:
            if not OTHER:
                continue
            cwd = subprocess.run(["sudo", "-n", "readlink", f"/proc/{pid}/cwd"], capture_output=True, text=True).stdout.strip()
            env = subprocess.run(["sudo", "-n", "cat", f"/proc/{pid}/environ"], capture_output=True).stdout
        if cwd.startswith(home) or f"HOME={home}".encode() in env:
            subprocess.run(["sudo", "-n", "kill", "-9", pid] if OTHER else ["kill", "-9", pid], capture_output=True)
    print("cleanup done")


if MODE == "snap":
    snap(sys.argv[4])
elif MODE == "probe":
    probe(sys.argv[4])
elif MODE == "verify":
    verify()
elif MODE == "cleanup":
    cleanup()
elif MODE == "sheet":
    sheet(sys.argv[3] if len(sys.argv) > 3 else None)
