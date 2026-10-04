#!/usr/bin/env python3
"""Medians per (label, home kind) from startup_bench.py's JSON lines, as a Markdown table.

    summarize_startup.py runs.jsonl [platform-label]
"""
import json
import sys


def median(xs):
    xs = sorted(x for x in xs if isinstance(x, (int, float)))
    return xs[len(xs) // 2] if xs else None


def fmt(x):
    return "–" if x is None else f"{x:.3f} s"


def main():
    path = sys.argv[1]
    platform = sys.argv[2] if len(sys.argv) > 2 else ""
    rows = []
    try:
        with open(path) as f:
            rows = [json.loads(line) for line in f if line.strip()]
    except FileNotFoundError:
        print(f"no results at {path}")
        return
    groups = {}
    for r in rows:
        groups.setdefault((r.get("label", ""), r.get("home_kind", "")), []).append(r)
    title = f"### Startup bench{' — ' + platform if platform else ''}"
    print(title)
    print()
    print("| binary | home | n | first byte | first frame | composer usable | engine ready |")
    print("|---|---|---|---|---|---|---|")
    for (label, kind), rs in sorted(groups.items()):
        errors = sum(1 for r in rs if r.get("engine_ready") == "error")
        eng = median([r.get("engine_ready") for r in rs])
        eng_s = fmt(eng) + (f" ({errors} failed)" if errors else "")
        print(f"| {label} | {kind} | {len(rs)} | {fmt(median([r.get('first_output') for r in rs]))} | "
              f"{fmt(median([r.get('first_frame') for r in rs]))} | {fmt(median([r.get('composer') for r in rs]))} | {eng_s} |")
    print()
    print("Medians; times from exec in a 120×40 PTY. Fresh = empty home (the engine installs); warm = the home that launch left.")


if __name__ == "__main__":
    main()
