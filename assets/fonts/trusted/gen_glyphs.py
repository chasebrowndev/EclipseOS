# SPDX-License-Identifier: AGPL-3.0-only
"""Pre-rasterise the trusted UI's face into coverage maps (ADR 0009).

abyss links no font library: this runs once, offline, and its output is
checked in beside this file as `jbm-<scale>x.bin` and compiled into
`abyss/crates/ec-abyss-render/src/glyphs.rs`. Each file holds printable ASCII
(0x20..=0x7E) in Regular then Medium, one 8s x 16s cell per glyph, one
coverage byte per pixel, row-major. The face is JetBrains Mono (OFL,
assets/fonts/OFL-JetBrainsMono.txt), sized so its 600-unit advance is
exactly 8 logical px; the baseline sits at 12 px of the 16 px cell.

    python3 -I assets/fonts/trusted/gen_glyphs.py

Needs Pillow built with FreeType. Rerunning it must give identical bytes.
"""
import pathlib
from PIL import Image, ImageDraw, ImageFont

ROOT = pathlib.Path(__file__).resolve().parents[3]
OUT = pathlib.Path(__file__).resolve().parent
FACES = [ROOT / "assets/fonts/JetBrainsMono-Regular.ttf", ROOT / "assets/fonts/JetBrainsMono-Medium.ttf"]
W, H, BASE = 8, 16, 12

for s in (1, 2, 3):
    out = bytearray()
    for face in FACES:
        f = ImageFont.truetype(str(face), size=W * s / 0.6)
        for c in range(0x20, 0x7F):
            im = Image.new("L", (W * s, H * s), 0)
            ImageDraw.Draw(im).text((0, BASE * s), chr(c), font=f, fill=255, anchor="ls")
            out += im.tobytes()
    (OUT / f"jbm-{s}x.bin").write_bytes(bytes(out))
    print(f"jbm-{s}x.bin", len(out))
