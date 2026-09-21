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
    # 2a'. tonal bust for the large tiers (tighter crop x=470 y=95 side=360, full tonal range, the
    #      wall kept as texture at 85% brightness, plus a per-cell shade map the renderer maps onto
    #      theme shades)
    python3 dotart.py photo.jpg --mask mask.png --crop 470 95 360 --rows 14 --cols 28 \
        --levels 0 1 --gamma 0.75 --strength 1.0 --method fs --keep-bg --bg-scale 0.85 \
        --shade-thresholds 0.3 0.55 --out ../assets/portrait-14x28.txt \
        --shade-out ../assets/portrait-14x28.shade.txt
    python3 dotart.py photo.jpg --mask mask.png --crop 470 95 360 --rows 21 --cols 42 \
        --levels 0 1 --gamma 0.75 --strength 1.0 --method fs --keep-bg --bg-scale 0.85 \
        --shade-thresholds 0.3 0.55 --out ../assets/portrait-21x42.txt \
        --shade-out ../assets/portrait-21x42.shade.txt
    # 2a''. second photo (1179x2293, seated on lit stairs; crop x=120 y=500 side=1000): the stairs
    #       dimmed to 60% so the side-lit face dominates, a touch of local contrast for the profile
    python3 dotart.py photo2.jpg --mask mask2.png --crop 120 500 1000 --rows 14 --cols 28 \
        --levels 0 1 --gamma 0.65 --strength 1.0 --method fs --keep-bg --bg-scale 0.6 \
        --local 0.6 --local-div 12 --shade-thresholds 0.3 0.55 \
        --out ../assets/portrait2-14x28.txt --shade-out ../assets/portrait2-14x28.shade.txt
    python3 dotart.py photo2.jpg --mask mask2.png --crop 120 500 1000 --rows 7  --cols 14 \
        --levels 0.10 0.45 --strength 0.5 --out ../assets/portrait2-7x14.txt
    python3 dotart.py photo2.jpg --mask mask2.png --crop 120 500 1000 --rows 5  --cols 10 \
        --levels 0.10 0.45 --strength 0.4 --out ../assets/portrait2-5x10.txt
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


def tone(photo, mask, cutoff, levels, feather, local, local_div, bg, gamma, bg_scale=1.0):
    """Grayscale in [0,1]: person autocontrasted, locally sharpened, level-squeezed, gamma-lifted.

    The masked background is forced to `bg`; `bg` = None keeps its own (normalized) luminance so
    the wall and street stay textures, scaled by `bg_scale` so they do not outshine the face.
    """
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
    if gamma != 1.0:
        a = np.power(a, gamma)
    if bg is None:
        return a * (person + bg_scale * (1.0 - person))
    return a * person + bg * (1.0 - person)


def downsample(a, dots_w, dots_h):
    im = Image.fromarray((a * 255).astype(np.uint8))
    im = im.filter(ImageFilter.GaussianBlur(radius=max(0.5, im.size[0] / dots_w / 2.5)))
    im = im.resize((dots_w, dots_h), Image.LANCZOS)
    return np.asarray(im, dtype=np.float32) / 255.0


DITHER_KERNELS = {
    # Floyd-Steinberg: full error diffusion, smooth tones
    "fs": (((0, 1), 7 / 16), ((1, -1), 3 / 16), ((1, 0), 5 / 16), ((1, 1), 1 / 16)),
    # Atkinson: diffuses 6/8 of the error, so highlights and shadows clip to a crisper, higher-contrast look
    "atkinson": (((0, 1), 1 / 8), ((0, 2), 1 / 8), ((1, -1), 1 / 8), ((1, 0), 1 / 8), ((1, 1), 1 / 8), ((2, 0), 1 / 8)),
}


def dither(a, method, strength):
    """Error diffusion; `strength` < 1 diffuses less error for cleaner shapes at tiny sizes."""
    err = a.astype(np.float32).copy()
    h, w = err.shape
    out = np.zeros((h, w), dtype=bool)
    kernel = DITHER_KERNELS[method]
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


def shade_map(a, cols, rows, thresholds):
    """Per-cell luminance tercile as digits 0 (dark) / 1 / 2 (bright), from the pre-dither tone."""
    cell = downsample(a, cols, rows)
    lo, hi = thresholds
    digits = np.where(cell < lo, 0, np.where(cell < hi, 1, 2))
    return ["".join(str(int(v)) for v in row) for row in digits]


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


def render_lattice(rows, path, spacing=8, fg=(200, 200, 200), bg=(13, 13, 13), shade=None, shade_fg=None):
    """Ideal-terminal preview: every dot on a uniform lattice (2x4 dots per cell).

    With `shade` (digit rows) and `shade_fg` (three colors), each cell takes the color of its shade level.
    """
    w, h = len(rows[0]) * 2, len(rows) * 4
    im = Image.new("RGB", (w * spacing + spacing, h * spacing + spacing), bg)
    d = ImageDraw.Draw(im)
    r = spacing * 0.36
    for cy, row in enumerate(rows):
        for cx, ch in enumerate(row):
            v = ord(ch) - 0x2800
            color = shade_fg[int(shade[cy][cx])] if shade else fg
            for (dx, dy), bit in BRAILLE_BITS.items():
                if v & bit:
                    x, y = (cx * 2 + dx + 1) * spacing, (cy * 4 + dy + 1) * spacing
                    d.ellipse([x - r, y - r, x + r, y + r], fill=color)
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
    p.add_argument("--keep-bg", action="store_true", help="keep the background's own luminance (texture) instead of --bg")
    p.add_argument("--bg-scale", type=float, default=1.0, help="with --keep-bg: multiply the background luminance")
    p.add_argument("--gamma", type=float, default=1.0, help="< 1 lifts midtones")
    p.add_argument("--method", choices=sorted(DITHER_KERNELS), default="fs")
    p.add_argument("--shade-out", help="also write a per-cell shade map (digits 0-2) for tone shading at render time")
    p.add_argument("--shade-thresholds", type=float, nargs=2, default=(0.33, 0.66), metavar=("LO", "HI"))
    p.add_argument("--out", required=True, help="glyph grid .txt")
    p.add_argument("--preview", help="optional lattice preview PNG")
    args = p.parse_args()

    photo = crop_square(args.photo, args.crop, "RGB")
    mask = crop_square(args.mask, args.crop, "L") if args.mask else None
    bg = None if args.keep_bg else args.bg
    a = tone(photo, mask, args.cutoff, args.levels, args.feather, args.local, args.local_div, bg, args.gamma, args.bg_scale)
    bits = dither(downsample(a, args.cols * 2, args.rows * 4), args.method, args.strength)
    rows = to_braille(bits)
    with open(args.out, "w", encoding="utf-8") as f:
        f.write("\n".join(rows) + "\n")
    shade = None
    if args.shade_out:
        shade = shade_map(a, args.cols, args.rows, args.shade_thresholds)
        with open(args.shade_out, "w", encoding="utf-8") as f:
            f.write("\n".join(shade) + "\n")
    if args.preview:
        # preview shades approximate grok-night: gray blended toward the background / the primary text color
        render_lattice(rows, args.preview, shade=shade, shade_fg=((64, 64, 64), (108, 108, 108), (172, 172, 172)))
    print("\n".join(rows))
    print(f"{len(rows[0])}x{len(rows)} cells, {bits.shape[1]}x{bits.shape[0]} dots, fill {bits.mean():.2f}")


if __name__ == "__main__":
    main()
