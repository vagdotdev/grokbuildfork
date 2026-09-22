#!/usr/bin/env python3
"""Draw the Workshop `v` monogram onto the braille grids the welcome hero paints.

    tools/monogram.py [OUTDIR]        (default: ../assets)

Writes monogram-<style>-7x14.txt and monogram-<style>-5x10.txt for each style: a square dot
canvas of 28 or 20 dots is rasterized from a few line segments and polygons (coverage sampled at
16x16 per dot, on at >= 50%), then packed into braille cells (2 x 4 dots each, blanks as U+2800).
Pure Python, no dependencies. Every coordinate scales with the canvas size N so both tiers come
from the same shape.
"""

import math
import sys
from pathlib import Path

SUPERSAMPLE = 16


def seg_dist(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    len2 = vx * vx + vy * vy
    t = 0.0 if len2 == 0 else max(0.0, min(1.0, ((px - ax) * vx + (py - ay) * vy) / len2))
    return math.hypot(px - (ax + t * vx), py - (ay + t * vy))


def in_poly(px, py, poly):
    inside = False
    for i, (x1, y1) in enumerate(poly):
        x2, y2 = poly[(i + 1) % len(poly)]
        if (y1 > py) != (y2 > py) and px < x1 + (py - y1) * (x2 - x1) / (y2 - y1):
            inside = not inside
    return inside


class Shape:
    def __init__(self):
        self.parts = []

    def seg(self, a, b, width):
        """Line segment with round caps."""
        self.parts.append(lambda x, y: seg_dist(x, y, *a, *b) <= width / 2)
        return self

    def poly(self, points):
        self.parts.append(lambda x, y: in_poly(x, y, points))
        return self

    def rect(self, x0, y0, x1, y1):
        self.parts.append(lambda x, y: x0 <= x <= x1 and y0 <= y <= y1)
        return self

    def hit(self, x, y):
        return any(part(x, y) for part in self.parts)


def sans(n):
    """Uniform stroke, mitered apex: the geometric mark (default)."""
    yt, yb = n * 0.12, n * 0.94
    cx, hs = n / 2, n * 0.41
    w = n * 0.15  # horizontal stroke width
    yi = yt + (hs - w) * (yb - yt) / hs  # where the inner edges meet
    return Shape().poly(
        [(cx - hs, yt), (cx - hs + w, yt), (cx, yi), (cx + hs - w, yt), (cx + hs, yt), (cx, yb)]
    )


def serif(n):
    """Thick left downstroke tapering to the apex, thin right arm, flat serifs on both tops."""
    yt, yb = n * 0.10, n * 0.92
    cx, hs = n / 2, n * 0.38
    wl, wr = n * 0.19, n * 0.075
    sh = n * 0.06  # serif slab height
    s = Shape()
    s.poly([(cx - hs, yt), (cx - hs + wl, yt), (cx + wr * 0.6, yb - n * 0.06), (cx - n * 0.02, yb)])
    s.seg((cx + hs - n * 0.01, yt + sh), (cx + wr * 0.3, yb - n * 0.04), wr)
    s.rect(cx - hs - n * 0.07, yt, cx - hs + wl + n * 0.05, yt + sh)
    s.rect(cx + hs - n * 0.10, yt, cx + hs + n * 0.06, yt + sh)
    return s


def hairline(n):
    """Thin uniform stroke with a rounded apex: the lightest mark."""
    yt, yb = n * 0.14, n * 0.90
    cx, hs = n / 2, n * 0.38
    w = n * 0.095
    apex = (cx, yb - w / 2)
    return Shape().seg((cx - hs, yt), apex, w).seg((cx + hs, yt), apex, w)


STYLES = {"sans": sans, "serif": serif, "hairline": hairline}
TIERS = {"7x14": 28, "5x10": 20}

# Dot (column, row) inside a cell -> braille bit.
BITS = {
    (0, 0): 0x01, (0, 1): 0x02, (0, 2): 0x04, (0, 3): 0x40,
    (1, 0): 0x08, (1, 1): 0x10, (1, 2): 0x20, (1, 3): 0x80,
}


def rasterize(shape, n):
    step = 1.0 / SUPERSAMPLE
    grid = []
    for j in range(n):
        row = []
        for i in range(n):
            hits = sum(
                shape.hit(i + (sx + 0.5) * step, j + (sy + 0.5) * step)
                for sy in range(SUPERSAMPLE)
                for sx in range(SUPERSAMPLE)
            )
            row.append(hits * 2 >= SUPERSAMPLE * SUPERSAMPLE)
        grid.append(row)
    return grid


def to_braille(grid):
    lines = []
    for r in range(len(grid) // 4):
        cells = []
        for c in range(len(grid[0]) // 2):
            bits = 0
            for (dx, dy), bit in BITS.items():
                if grid[4 * r + dy][2 * c + dx]:
                    bits |= bit
            cells.append(chr(0x2800 + bits))
        lines.append("".join(cells))
    return "\n".join(lines) + "\n"


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "assets"
    out.mkdir(parents=True, exist_ok=True)
    for style, shape in STYLES.items():
        for tier, n in TIERS.items():
            art = to_braille(rasterize(shape(n), n))
            (out / f"monogram-{style}-{tier}.txt").write_text(art, encoding="utf-8")
            print(f"{style} {tier}\n{art}")


if __name__ == "__main__":
    main()
