#!/usr/bin/env python3
"""Fresh machine state for one acceptance run.

usage: setup.py TASK HOME OUTDIR USER

Creates the fresh HOME with the folders a Mac user always has (Desktop, Documents, Downloads), puts the
task's fixtures in it, and resets whatever system state the task changes (packages it installs). What
the verifier needs to know about the fixtures goes to OUTDIR/fixture.json. Fixtures that are
downloaded or generated once are cached under $ACC_CACHE (default ~/.cache/workshop-acceptance).
"""
import hashlib
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

TASK, HOME, OUT, USER = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]), sys.argv[4]
HERE = Path(__file__).resolve().parent
CACHE = Path(os.environ.get("ACC_CACHE", Path.home() / ".cache/workshop-acceptance"))
FFMPEG = os.environ.get("ACC_FFMPEG", "/opt/rec/bin/ffmpeg")
ME = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
OTHER = USER != ME
fixture = {}


def sh(cmd, check=False):
    print(f"$ {cmd}", flush=True)
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    if r.stdout.strip():
        print(r.stdout.strip()[-2000:])
    if r.returncode and r.stderr.strip():
        print(r.stderr.strip()[-2000:])
    if check and r.returncode:
        sys.exit(f"setup step failed: {cmd}")
    return r


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def apt_purge(pkgs):
    sh("sudo DEBIAN_FRONTEND=noninteractive apt-get purge -y -qq " + " ".join(pkgs))


def stage(rel, src=None, data=None):
    """Place a fixture file at HOME/rel (the run's user owns it)."""
    dst = HOME / rel
    tmp = CACHE / "stage" / rel
    tmp.parent.mkdir(parents=True, exist_ok=True)
    if src is not None:
        shutil.copyfile(src, tmp)
    else:
        tmp.write_bytes(data)
    if OTHER:
        sh(f"sudo install -D -m 644 -o {USER} -g {USER} '{tmp}' '{dst}'", check=True)
    else:
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(tmp, dst)
    return dst


# --- the fresh HOME ----------------------------------------------------------------------------
if OTHER:
    sh(f"id {USER} >/dev/null 2>&1 || sudo useradd -m -s /bin/bash -G sudo {USER}", check=True)
    sh(f"sudo rm -rf '{HOME}'")
    sh(f"sudo install -d -m 755 '{HOME.parent}'")
    sh(f"sudo install -d -m 755 -o {USER} -g {USER} '{HOME}'", check=True)
    for d in ("Desktop", "Documents", "Downloads"):
        sh(f"sudo -u {USER} mkdir -p '{HOME / d}'", check=True)
else:
    shutil.rmtree(HOME, ignore_errors=True)
    for d in ("Desktop", "Documents", "Downloads"):
        (HOME / d).mkdir(parents=True, exist_ok=True)
CACHE.mkdir(parents=True, exist_ok=True)

# --- per task ----------------------------------------------------------------------------------
if TASK == "T1":
    apt_purge(["ghostty"])
    sh("sudo rm -f /etc/apt/sources.list.d/*ghostty* /usr/local/bin/ghostty")
    sh("command -v snap >/dev/null && snap list ghostty >/dev/null 2>&1 && sudo snap remove ghostty")
    fixture["ghostty_before"] = sh("command -v ghostty").stdout.strip()

elif TASK == "T2v":
    rows = [l.split("\t") for l in (HERE / "fixtures/panthera.tsv").read_text().splitlines()
            if l.strip() and not l.startswith("#")]
    photos = []
    for species, title, url, digest in rows:
        f = CACHE / "panthera" / digest
        if not f.exists() or sha(f) != digest:
            f.parent.mkdir(parents=True, exist_ok=True)
            sh(f"curl -fsSL -A 'workshop-acceptance/1.0' -o '{f}' '{url}'", check=True)
            if sha(f) != digest:
                sys.exit(f"fixture {title}: sha256 mismatch")
        photos.append((species, title, f, digest))
    order = list(range(len(photos)))
    random.Random(OUT.name).shuffle(order)
    fixture["photos"] = {}
    for n, i in enumerate(order, 1):
        species, title, f, digest = photos[i]
        stage(f"Desktop/img{n:02d}.jpg", src=f)
        fixture["photos"][digest] = {"name": f"img{n:02d}.jpg", "species": species, "source": title}

elif TASK == "T4":
    repo = CACHE / "more-itertools"
    if not (repo / ".git").exists():
        sh(f"git clone -q --depth 1 --branch v10.5.0 https://github.com/more-itertools/more-itertools '{repo}'", check=True)
    base = CACHE / "more-itertools.baseline.json"
    if not base.exists():
        r = sh(f"cd '{repo}' && python3 -m unittest discover -s tests -t . 2>&1 | tail -5")
        ran = [l for l in r.stdout.splitlines() if l.startswith("Ran ")]
        base.write_text(json.dumps({"ran": int(ran[0].split()[1]) if ran else None, "tail": r.stdout}))
    dst = HOME / "projects/more-itertools"
    if OTHER:
        sys.exit("T4 runs as the invoking user")
    shutil.copytree(repo, dst, symlinks=True)
    more = dst / "more_itertools/more.py"
    src = more.read_text()
    good = "    return sum(compress(repeat(1), zip(iterable)))\n"
    assert src.count(good) == 1, "ilen body changed upstream"
    more.write_text(src.replace(good, "    it = iter(iterable)\n    return sum(compress(repeat(1), zip(it, it)))\n"))
    sh(f"cd '{dst}' && git -c user.name='Sam Lee' -c user.email=sam@example.com commit -qam "
       "'ilen: pair items to halve the zip overhead'", check=True)
    fixture["baseline"] = json.loads(base.read_text())
    fixture["head"] = sh(f"cd '{dst}' && git rev-parse HEAD").stdout.strip()
    r = sh(f"cd '{dst}' && python3 -m unittest discover -s tests -t . 2>&1 | tail -3")
    fixture["broken_tail"] = r.stdout

elif TASK == "T5":
    clip = CACHE / "clip.mp4"
    if not clip.exists():
        sh(f"'{FFMPEG}' -loglevel error -y -f lavfi -i testsrc2=size=640x360:rate=25 -t 5 "
           f"-c:v libx264 -pix_fmt yuv420p '{clip}'", check=True)
    stage("Desktop/clip.mp4", src=clip)
    sh("sudo DEBIAN_FRONTEND=noninteractive apt-get remove -y -qq ffmpeg")
    fixture["ffmpeg_before"] = sh("command -v ffmpeg").stdout.strip()

elif TASK == "T7":
    from PIL import Image, ImageDraw

    def jpg(color, size=(800, 600), fmt="JPEG"):
        im = Image.new("RGB", size, color)
        d = ImageDraw.Draw(im)
        for i in range(0, size[0], 40):
            d.line([(i, 0), (size[0] - i, size[1])], fill=(255 - color[0], 120, color[2]), width=3)
        b = io.BytesIO()
        im.save(b, fmt)
        return b.getvalue()

    def office(kind):
        b = io.BytesIO()
        main = {"docx": ("word/document.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"),
                "xlsx": ("xl/workbook.xml", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"),
                "pptx": ("ppt/presentation.xml", "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml")}[kind]
        with zipfile.ZipFile(b, "w") as z:
            z.writestr("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
                       f'<Override PartName="/{main[0]}" ContentType="{main[1]}"/></Types>')
            z.writestr(main[0], '<?xml version="1.0"?><root/>')
        return b.getvalue()

    def media(name, args):
        f = CACHE / "t7" / name
        if not f.exists():
            f.parent.mkdir(parents=True, exist_ok=True)
            sh(f"'{FFMPEG}' -loglevel error -y {args} '{f}'", check=True)
        return f.read_bytes()

    def zipped():
        b = io.BytesIO()
        with zipfile.ZipFile(b, "w") as z:
            z.writestr("beach.jpg", jpg((30, 120, 200)))
            z.writestr("mountain.jpg", jpg((90, 140, 60)))
        return b.getvalue()

    def targz():
        b = io.BytesIO()
        with tarfile.open(fileobj=b, mode="w:gz") as t:
            data = b"old project notes\n" * 50
            info = tarfile.TarInfo("old-project/notes.txt")
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
        return b.getvalue()

    pdf = io.BytesIO()
    Image.new("RGB", (595, 842), (255, 255, 255)).save(pdf, "PDF")
    files = {
        "IMG_2041.JPG": (jpg((200, 80, 40)), ["images"]),
        "Screenshot 2026-09-20 at 10.12.33.png": (jpg((20, 20, 20), (1280, 800), "PNG"), ["images"]),
        "vacation 001.jpeg": (jpg((40, 160, 220)), ["images"]),
        "logo.svg": (b'<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><circle cx="32" cy="32" r="30" fill="teal"/></svg>\n',
                     ["images", "other"]),
        "Invoice March (1).pdf": (pdf.getvalue(), ["documents"]),
        "resume FINAL final.docx": (office("docx"), ["documents"]),
        "budget 2026.xlsx": (office("xlsx"), ["documents"]),
        "presentation v2.pptx": (office("pptx"), ["documents"]),
        "notes.txt": (b"call the dentist\nbuy stamps\n", ["documents"]),
        "README.md": (b"# Old project\n\nNothing to see here.\n", ["documents", "other"]),
        "data export.csv": (b"date,amount\n2026-09-01,12.50\n2026-09-02,8.00\n", ["documents", "other"]),
        "song - demo.mp3": (media("song.mp3", "-f lavfi -i sine=frequency=440:duration=3 -c:a libmp3lame"), ["music"]),
        "podcast episode 12.m4a": (media("podcast.m4a", "-f lavfi -i sine=frequency=220:duration=3 -c:a aac"), ["music"]),
        "movie clip.mov": (media("clip.mov", "-f lavfi -i testsrc2=size=320x240:rate=15 -t 2 -c:v libx264 -pix_fmt yuv420p"), ["videos"]),
        "Screen Recording 2026-09-18.mp4": (media("rec.mp4", "-f lavfi -i testsrc=size=320x240:rate=15 -t 2 -c:v libx264 -pix_fmt yuv420p"), ["videos"]),
        "photos backup.zip": (zipped(), ["archives"]),
        "archive.tar.gz": (targz(), ["archives"]),
        "DejaVu Sans.ttf": (Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf").read_bytes(), ["other"]),
    }
    fixture["files"] = {}
    for name, (data, cats) in files.items():
        p = stage(f"Downloads/{name}", data=data)
        fixture["files"][hashlib.sha256(data).hexdigest()] = {"name": name, "categories": cats}

elif TASK == "T11":
    r = sh(f"sudo -u {USER} sudo -n true")
    if r.returncode == 0:
        sys.exit(f"sudo for {USER} does not ask for a password; T11 needs it to")
    pw = next((l.split(None, 1)[1] for l in (HERE / "tasks/T11.steps").read_text().splitlines()
               if l.startswith("@password ")), "").strip()
    subprocess.run(["sudo", "chpasswd"], input=f"{USER}:{pw}\n", text=True, check=True)
    sh(f"sudo rm -rf /var/run/sudo/ts/{USER}")
    apt_purge(["htop"])
    fixture["htop_before"] = sh("dpkg -s htop 2>/dev/null | grep -m1 ^Status").stdout.strip()

(OUT / "fixture.json").write_text(json.dumps(fixture, indent=1))
print(f"setup {TASK}: ok")
