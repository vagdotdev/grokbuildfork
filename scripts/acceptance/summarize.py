#!/usr/bin/env python3
"""Pass-rate table over every run under a results folder.

usage: summarize.py RESULTS_DIR   (reads RESULTS_DIR/<task>-r<N>/verify.json, writes RESULTS_DIR/summary.md)
"""
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

root = Path(sys.argv[1])
runs = defaultdict(list)
for f in sorted(root.glob("*/verify.json")):
    v = json.loads(f.read_text())
    runs[v["task"]].append(v)


def order(t):
    m = re.match(r"T(\d+)(\w*)", t)
    return (int(m.group(1)), m.group(2)) if m else (999, t)


lines = ["| Task | Passed | Checks that failed (runs) | Turn times (s) |", "|---|---|---|---|"]
for task in sorted(runs, key=order):
    vs = sorted(runs[task], key=lambda v: v["run"])
    passed = sum(v["pass"] for v in vs)
    pending = sum(v.get("pending", False) for v in vs)
    fails = defaultdict(list)
    for v in vs:
        for c in v["checks"]:
            if c["ok"] is False and c["id"] != "U4":
                fails[c["id"]].append(v["run"].rsplit("-", 1)[-1])
    times = "; ".join(",".join(str(round(w["seconds"])) for w in v["metrics"]["windows"]) for v in vs)
    cell = ", ".join(f"{k} ({' '.join(r)})" for k, r in sorted(fails.items())) or "—"
    lines.append(f"| {task} | {passed}/{len(vs)}{f' ({pending} pending)' if pending else ''} | {cell} | {times} |")
total = sum(len(v) for v in runs.values())
ok = sum(v["pass"] for vs in runs.values() for v in vs)
lines.append(f"\n**{ok}/{total} runs passed.**")
(root / "summary.md").write_text("\n".join(lines) + "\n")
print("\n".join(lines))
