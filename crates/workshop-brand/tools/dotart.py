#!/usr/bin/env python3
"""Photo -> braille dot-art for the Workshop welcome hero.

Regenerates the `assets/*.txt` glyph grids from a source photo. The output uses only
U+2800..U+28FF (blank cells are U+2800, never spaces), one line per terminal row, so the
pager can size and recolor it exactly like the upstream Grok logo.

Recipes used for the committed assets (photo 1280x720; dots = light areas, the pager flips bits
for light themes):

    # 1. person mask (one-off, `pip install rembg[cpu]`; white = person)
    python3 -c "from rembg import remove, new_session; from PIL import Image; \
      remove(Image.open('photo.jpg').convert('RGB'), session=new_session('u2net_human_seg'), \
      only_mask=True).save('mask.png')"
    # 2a. bust (head + shoulders, crop x=450 y=80 side=400, background = solid dots)
    python3 dotart.py photo.jpg --mask mask.png --crop 450 80 400 --rows 7  --cols 14 \
        --levels 0.10 0.45 --strength 0.5 --out ../assets/portrait-7x14.txt
    python3 dotart.py photo.jpg --mask mask.png --crop 450 80 400 --rows 5  --cols 10 \
        --levels 0.10 0.45 --strength 0.4 --out ../assets/portrait-5x10.txt
    python3 dotart.py photo.jpg --mask mask.png --crop 450 80 400 --rows 14 --cols 28 \
        --levels 0.10 0.60 --strength 0.8 --out ../assets/portrait-14x28.txt
    # 2b. face (passport crop x=555 y=215 side=210, background empty, features sharpened)
    python3 dotart.py photo.jpg --mask mask.png --crop 555 215 210 --rows 7  --cols 14 --bg 0 \
        --levels 0.20 0.50 --strength 0.4 --local 1.5 --local-div 12 --out ../assets/face-7x14.txt
    python3 dotart.py photo.jpg --mask mask.png --crop 555 215 210 --rows 5  --cols 10 --bg 0 \
        --levels 0.20 0.50 --strength 0.3 --local 1.5 --local-div 12 --out ../assets/face-5x10.txt
    python3 dotart.py photo.jpg --mask mask.png --crop 555 215 210 --rows 14 --cols 28 --bg 0 \
        --levels 0.15 0.65 --strength 0.8 --local 1.0 --local-div 12 --out ../assets/face-14x28.txt

Requires Pillow and numpy. Add `--preview out.png` to render an ideal-lattice preview.
"""

import argparse

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageOps

# (col, row) inside a 2x4 braille cell -> dot bit
BRAILLE_BITS = {
    (0, 0): 0x01, (0, 1): 0x02, (0, 2): 0x04, (1, 0): 0x08,
    (1, 1): 0x10, (1, 2): 0x20, (0, 3): 0x40, (1, 3): 0x80,
}


def crop_square(path, crop, mode):
    x0, y0, side = crop
    return Image.open(path).convert(mode).crop((x0, y0, x0 + side, y0 + side))


def tone(photo, mask, cutoff, levels, feather, local, local_div, bg):
    """Grayscale in [0,1]: person autocontrasted, locally sharpened, level-squeezed; background forced to `bg`."""
    gray = np.asarray(ImageOps.grayscale(photo), dtype=np.float32)
    if mask is None:
        person = np.ones_like(gray)
    else:
        m = mask.filter(ImageFilter.GaussianBlur(feather)) if feather > 0 else mask
        person = np.asarray(m, dtype=np.float32) / 255.0
    sel = gray[person > 0.5] if mask is not None else gray
    lo, hi = np.percentile(sel, cutoff), np.percentile(sel, 100 - cutoff)
    a = np.clip((gray - lo) / max(hi - lo, 1.0), 0.0, 1.0)
    if local > 0:
        # unsharp mask at feature scale: pushes eyes, nostrils and lips away from the surrounding skin tone
        blurred = Image.fromarray((a * 255).astype(np.uint8)).filter(
            ImageFilter.GaussianBlur(max(1, photo.size[0] // local_div))
        )
        a = np.clip(a + local * (a - np.asarray(blurred, dtype=np.float32) / 255.0), 0.0, 1.0)
    lo_in, hi_in = levels
    a = np.clip((a - lo_in) / max(hi_in - lo_in, 1e-3), 0.0, 1.0)
    return a * person + bg * (1.0 - person)


def downsample(a, dots_w, dots_h):
    im = Image.fromarray((a * 255).astype(np.uint8))
    im = im.filter(ImageFilter.GaussianBlur(radius=max(0.5, im.size[0] / dots_w / 2.5)))
    im = im.resize((dots_w, dots_h), Image.LANCZOS)
    return np.asarray(im, dtype=np.float32) / 255.0


def floyd_steinberg(a, strength):
    """Error diffusion; `strength` < 1 diffuses less error for cleaner shapes at tiny sizes."""
    err = a.astype(np.float32).copy()
    h, w = err.shape
    out = np.zeros((h, w), dtype=bool)
    kernel = (((0, 1), 7 / 16), ((1, -1), 3 / 16), ((1, 0), 5 / 16), ((1, 1), 1 / 16))
    for y in range(h):
        for x in range(w):
            old = err[y, x]
            new = 1.0 if old >= 0.5 else 0.0
            out[y, x] = new > 0.5
            e = (old - new) * strength
            for (dy, dx), k in kernel:
                yy, xx = y + dy, x + dx
                if 0 <= yy < h and 0 <= xx < w:
                    err[yy, xx] += e * k
    return out


def to_braille(bits):
    h, w = bits.shape
    rows = []
    for cy in range(h // 4):
        cells = []
        for cx in range(w // 2):
            v = 0
            for (dx, dy), bit in BRAILLE_BITS.items():
                if bits[cy * 4 + dy, cx * 2 + dx]:
                    v |= bit
            cells.append(chr(0x2800 + v))
        rows.append("".join(cells))
    return rows


def render_lattice(rows, path, spacing=8, fg=(200, 200, 200), bg=(13, 13, 13)):
    """Ideal-terminal preview: every dot on a uniform lattice (2x4 dots per cell)."""
    w, h = len(rows[0]) * 2, len(rows) * 4
    im = Image.new("RGB", (w * spacing + spacing, h * spacing + spacing), bg)
    d = ImageDraw.Draw(im)
    r = spacing * 0.36
    for cy, row in enumerate(rows):
        for cx, ch in enumerate(row):
            v = ord(ch) - 0x2800
            for (dx, dy), bit in BRAILLE_BITS.items():
                if v & bit:
                    x, y = (cx * 2 + dx + 1) * spacing, (cy * 4 + dy + 1) * spacing
                    d.ellipse([x - r, y - r, x + r, y + r], fill=fg)
    im.save(path)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("photo")
    p.add_argument("--crop", type=int, nargs=3, metavar=("X", "Y", "SIDE"), required=True)
    p.add_argument("--mask", help="person mask PNG (white = person); background becomes solid dots")
    p.add_argument("--rows", type=int, default=7)
    p.add_argument("--cols", type=int, default=14)
    p.add_argument("--cutoff", type=float, default=2.0, help="autocontrast percentile clip")
    p.add_argument("--levels", type=float, nargs=2, default=(0.10, 0.45), metavar=("LO", "HI"))
    p.add_argument("--strength", type=float, default=0.5, help="error-diffusion strength 0..1")
    p.add_argument("--feather", type=float, default=2.0)
    p.add_argument("--local", type=float, default=0.0, help="local-contrast amount (0 = off)")
    p.add_argument("--local-div", type=int, default=12, help="local-contrast radius = side / this")
    p.add_argument("--bg", type=float, default=1.0, help="masked background tone: 1 = solid dots, 0 = empty")
    p.add_argument("--out", required=True, help="glyph grid .txt")
    p.add_argument("--preview", help="optional lattice preview PNG")
    args = p.parse_args()

    photo = crop_square(args.photo, args.crop, "RGB")
    mask = crop_square(args.mask, args.crop, "L") if args.mask else None
    a = tone(photo, mask, args.cutoff, args.levels, args.feather, args.local, args.local_div, args.bg)
    bits = floyd_steinberg(downsample(a, args.cols * 2, args.rows * 4), args.strength)
    rows = to_braille(bits)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write("\n".join(rows) + "\n")
    if args.preview:
        render_lattice(rows, args.preview)
    print("\n".join(rows))
    print(f"{len(rows[0])}x{len(rows)} cells, {bits.shape[1]}x{bits.shape[0]} dots, fill {bits.mean():.2f}")


if __name__ == "__main__":
    main()
