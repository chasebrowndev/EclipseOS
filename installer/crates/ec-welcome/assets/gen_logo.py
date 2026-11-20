# SPDX-License-Identifier: AGPL-3.0-only
"""Regenerate eclipseos-logo.png: "Eclipse" in Onest Regular, the O as a gold
ring around a black disc with a soft glow, and "S" in Onest ExtraBold gold.

    python3 -I installer/crates/ec-welcome/assets/gen_logo.py [out.png]

Needs Pillow and numpy. Prints the constants `src/logo.rs` pins (SRC_W, SRC_H,
O_CENTRE). The O geometry (ring 110 px across, disc 78 px, glow profile) is the
same as the previous asset, so the eclipse hand-off lands exactly as before.
"""
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[4]
FONTS = ROOT / "assets/fonts"
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("eclipseos-logo.png")

SS = 4  # text is drawn at 4x and box-filtered down
GOLD = (245, 191, 60)
INK = (255, 255, 255)
RING_R, DISC_R = 55.0, 39.0  # px
CAP = 92.0  # cap height of the letters, px (Onest cap height is 0.707 em)
GAP = 12.0  # clear space between the e / S and the ring's edge
PAD = 10  # left padding
H = 150
CY = 75.0
# Glow alpha by radius from the O's centre (median of the previous asset).
GLOW = [(55, 0.30), (57, 0.149), (60, 0.122), (63, 0.098), (66, 0.075), (69, 0.055),
        (72, 0.039), (75, 0.027), (78, 0.016), (81, 0.012), (84, 0.008), (87, 0.004), (90, 0.0)]

size = CAP / 0.707
regular = ImageFont.truetype(str(FONTS / "Onest-Regular.ttf"), round(size * SS))
heavy = ImageFont.truetype(str(FONTS / "Onest-ExtraBold.ttf"), round(size * SS))
base_y = (CY + CAP / 2) * SS  # shared baseline


def mask(text, font, x_right=None, x_left=None, w=None):
    """Alpha of `text` on a W x H canvas, baseline `base_y`, aligned right or left."""
    img = Image.new("L", (w * SS, H * SS), 0)
    d = ImageDraw.Draw(img)
    bbox = d.textbbox((0, 0), text, font=font, anchor="ls")
    tw = bbox[2] - bbox[0]
    x = (x_right * SS - bbox[2]) if x_right is not None else (x_left * SS - bbox[0])
    d.text((x, base_y), text, font=font, fill=255, anchor="ls")
    return np.asarray(img.resize((w, H), Image.BOX), dtype=np.float64) / 255.0, tw / SS


# Width: measure first, then fix the O's centre.
probe = Image.new("L", (10, 10))
dp = ImageDraw.Draw(probe)
e_w = dp.textbbox((0, 0), "Eclipse", font=regular, anchor="ls")
s_w = dp.textbbox((0, 0), "S", font=heavy, anchor="ls")
e_px = (e_w[2] - e_w[0]) / SS
s_px = (s_w[2] - s_w[0]) / SS
cx = PAD + e_px + GAP + RING_R
cx = round(cx)
W = round(cx + RING_R + GAP + s_px + PAD)

yy, xx = np.mgrid[0:H, 0:W]
r = np.hypot(xx + 0.5 - cx, yy + 0.5 - CY)
ring_a = np.clip(RING_R - r + 0.5, 0, 1)  # 1 px AA ramp
disc_a = np.clip(DISC_R - r + 0.5, 0, 1)
glow = np.interp(r, [p[0] for p in GLOW], [p[1] for p in GLOW], right=0.0)
glow = np.where(r < RING_R, 0.0, glow)

m_e, _ = mask("Eclipse", regular, x_right=cx - RING_R - GAP, w=W)
m_s, _ = mask("S", heavy, x_left=cx + RING_R + GAP, w=W)

layers = [  # bottom to top: (alpha, rgb)
    (glow, GOLD),
    (m_e, INK),
    (m_s, GOLD),
    (ring_a, GOLD),
    (disc_a, (0, 0, 0)),
]
a_out = np.zeros((H, W))
c_out = np.zeros((H, W, 3))  # premultiplied
for a, rgb in layers:
    c_out = c_out * (1 - a)[..., None] + a[..., None] * np.array(rgb, dtype=np.float64)
    a_out = a_out * (1 - a) + a
straight = np.where(a_out[..., None] > 1e-6, c_out / np.maximum(a_out[..., None], 1e-6), 0)
img = np.dstack([np.clip(straight, 0, 255), a_out * 255]).round().astype(np.uint8)
Image.fromarray(img, "RGBA").save(OUT, optimize=True)
print(f"SRC_W={W} SRC_H={H} O_CENTRE=({cx}, {CY:g}) -> {OUT}")
