#!/usr/bin/env python3
"""Cut a --desktop run's raw recording down to where something happens, with a real-time clock.

usage: render.py OUTDIR FINAL.mp4 [GAP_S=10] [KEEP_S=2]

Activity is any keystroke or any real change of the terminal text in the cast (spinner frames,
clock times and ticking counters ignored). Every stretch longer than GAP_S with no activity is cut,
keeping KEEP_S on each side, unless it overlaps a `mark protect-start` … `mark protect-end` range in
events.jsonl (a GUI window opening outside the terminal). Burned in: the real elapsed time and UTC wall
clock of every frame (so the clock jumps across a cut), a label after each cut with how long the cut
wait really was, and a caption for each prompt as it is typed and each turn end. Writes OUTDIR/cuts.json.
"""
import json
import os
import re
import subprocess
import sys
import time

import pyte

out, final = sys.argv[1], sys.argv[2]
GAP = float(sys.argv[3]) if len(sys.argv) > 3 else 10.0
KEEP = float(sys.argv[4]) if len(sys.argv) > 4 else 2.0
raw = os.path.join(out, "raw-screen.mp4")
ffmpeg = os.environ.get("ACC_FFMPEG", "/opt/rec/bin/ffmpeg")
ffprobe = os.path.join(os.path.dirname(ffmpeg), "ffprobe")

sync = [tuple(map(float, l.split())) for l in open(os.path.join(out, "cast-wall-sync.txt")) if l.strip()]
cast_off = min(w - t for w, t in sync)
vs = dict(l.strip().split("=") for l in open(os.path.join(out, "video-sync.txt")) if "=" in l)
dur = float(subprocess.check_output([ffprobe, "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", raw]))
video_start = float(vs["ffmpeg_stop_wall"]) - 0.3 - dur
c2v = lambda t: t + cast_off - video_start  # noqa: E731
w2v = lambda w: w - video_start  # noqa: E731

with open(os.path.join(out, "session.cast"), encoding="utf-8", errors="replace") as f:
    hdr = json.loads(f.readline())
    events = []
    for line in f:
        try:
            events.append(json.loads(line))
        except ValueError:
            pass
sc = pyte.Screen(hdr["width"], hdr["height"])
st = pyte.Stream(sc)
NOISE = [re.compile(r"[\u2800-\u28ff]"), re.compile(r"\b\d{1,2}:\d{2}( [AP]M)?\b"),
         re.compile(r"\b\d+h\d+m\d+s\b|\b\d+m\d+s\b"), re.compile(r"⇣[0-9.]+k?"),
         re.compile(r"\b\d+(\.\d+)?\s?(ms|s|m|h|sec|min)\b"), re.compile(r"\.{1,3}(?=\s|$)")]


def norm():
    # A truncated status row ("Run <cmd>… 46s") jitters by a character as its counter widens: keep a fixed prefix.
    txt = "\n".join((l[:60] if "…" in l else l).rstrip() for l in sc.display)
    for rx in NOISE:
        txt = rx.sub("", txt)
    return txt


activity, prev, pending, bucket, last_t = [], None, False, None, 0.0
for t, k, d in events:
    if k == "i":
        activity.append(t)
        continue
    b = int(t * 5)
    if bucket is not None and b != bucket and pending:
        cur = norm()
        if cur != prev:
            activity.append(last_t)
            prev = cur
        pending = False
    st.feed(d)
    bucket, last_t, pending = b, t, True
if pending and norm() != prev:
    activity.append(last_t)

act = sorted(c2v(t) for t in activity)
act = [0.0] + act + [c2v(events[-1][0]) if events else 0.0, dur]
evs = [json.loads(l) for l in open(os.path.join(out, "events.jsonl")) if l.strip()]
protect, start = [], None
for e in evs:
    if e["ev"] == "mark" and e["text"] == "protect-start":
        start = w2v(e["wall"])
    elif e["ev"] == "mark" and e["text"] == "protect-end" and start is not None:
        protect.append((start, w2v(e["wall"]) + 3))
        start = None

cuts = []
for a, b in zip(act, act[1:]):
    if b - a > GAP:
        c0, c1 = a + KEEP, b - KEEP
        if not any(p0 < c1 and c0 < p1 for p0, p1 in protect):
            cuts.append((round(c0, 2), round(c1, 2)))
keep, cur = [], 0.0
for c0, c1 in cuts:
    keep.append((cur, c0))
    cur = c1
keep.append((cur, dur))


def ts(x):
    return f"{int(x // 3600)}:{int(x % 3600 // 60):02d}:{x % 60:05.2f}"


def human(x):
    x = int(round(x))
    return f"{x // 60} min {x % 60:02d} s" if x >= 60 else f"{x} s"


def esc(s):
    return s.replace("\\", "\\\\").replace("{", "(").replace("}", ")")


ass = ["[Script Info]", "ScriptType: v4.00+", "PlayResX: 1920", "PlayResY: 1200", "", "[V4+ Styles]",
       "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding",
       "Style: Clock,DejaVu Sans Mono,32,&H00FFFFFF,&H00FFFFFF,&H90000000,&H90000000,1,0,0,0,100,100,0,0,3,10,0,3,1300,40,40,1",
       "Style: Skip,DejaVu Sans,30,&H0000D7FF,&H0000D7FF,&H90000000,&H90000000,1,0,0,0,100,100,0,0,3,10,0,3,1300,40,100,1",
       "Style: Note,DejaVu Sans,26,&H00FFFFFF,&H00FFFFFF,&H90000000,&HB0000000,0,0,0,0,100,100,0,0,3,10,0,3,1300,40,160,1",
       "", "[Events]", "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"]
for k0, k1 in keep:
    s = k0
    while s < k1:
        e = min(k1, int(s) + 1.0)
        wall = time.strftime("%H:%M:%S", time.gmtime(video_start + s))
        ass.append(f"Dialogue: 0,{ts(s)},{ts(e)},Clock,,0,0,0,,real time {int(s) // 60:02d}:{int(s) % 60:02d}  ·  {wall} UTC")
        s = e
for c0, c1 in cuts:
    ass.append(f"Dialogue: 0,{ts(max(c1 - 0.01, 0))},{ts(c1 + 4)},Skip,,0,0,0,,>> {human(c1 - c0)} of waiting cut here")
for e in evs:
    v = w2v(e["wall"])
    if e["ev"] == "prompt_sent":
        ass.append(f"Dialogue: 0,{ts(max(v - 1, 0))},{ts(v + 4)},Note,,0,0,0,,typed exactly (no nudges)")
    elif e["ev"] in ("turn_end", "turn_timeout"):
        label = "turn ended" if e["ev"] == "turn_end" else "turn did NOT end in time"
        ass.append(f"Dialogue: 0,{ts(v)},{ts(v + 4)},Note,,0,0,0,,{esc(label)}")
open(os.path.join(out, "overlay.ass"), "w").write("\n".join(ass) + "\n")

sel = "+".join(f"between(t,{a:.3f},{b:.3f})" for a, b in keep)
flt = f"subtitles={os.path.join(out, 'overlay.ass')},select='{sel}',setpts=N/(15*TB),scale=1728:1080:flags=lanczos"
open(os.path.join(out, "filter.txt"), "w").write(flt)
subprocess.run([ffmpeg, "-loglevel", "error", "-y", "-i", raw, "-filter_script:v", os.path.join(out, "filter.txt"),
                "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "22", "-pix_fmt", "yuv420p", "-r", "15",
                "-movflags", "+faststart", final], check=True)
kept = sum(b - a for a, b in keep)
json.dump({"raw_duration": dur, "kept_duration": kept, "cast_offset_wall": cast_off, "video_start_wall": video_start,
           "cuts_video_t": cuts, "keep_video_t": keep, "protect": protect}, open(os.path.join(out, "cuts.json"), "w"), indent=1)
print(f"raw {dur:.1f}s -> kept {kept:.1f}s, {len(cuts)} cuts totalling {sum(b - a for a, b in cuts):.1f}s")
