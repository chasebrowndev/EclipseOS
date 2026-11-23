#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Procedural EclipseOS wallpapers: corona rings arcing over colour.

    python3 packaging/backgrounds/gen-eclipse.py [OUT_DIR] [--all] [--preview]

Writes eclipse-{dark,light}-{3840x2160,1920x1080}.png (numpy for the maths,
ffmpeg for PNG encoding; no other dependency). Deterministic: the same script
gives the same bytes. The image is rendered once at 3840x2160 and the small
size is a box-filtered downscale, so both have the same composition. With
--all it also writes every variant as eclipse-<variant>-{dark,light}-*.png;
--preview renders the variants at 1920x1080 only.

The default (`arc`): a deep indigo ground with three broad glows (gold upper
right, blue lower left, pink along the top), and a bright corona ring whose arc
sweeps across the top edge, with a wider warm ring behind it, so a bar and
windows laid over it have something rich to bend. Variants move and reshape the
same ingredients: `fill` (the ring holds a second colour), `totality` (a moon
and its corona), `crescent`, `twin` (two rings in conjunction), `orbit`
(concentric rings from a corner), `horizon` (a rising arc) and `tilt` (a tilted
ellipse). Every length is a fraction of the screen height, so every size has the
same picture. Dark adds light to an indigo ground; light lays the same shapes
over a pale ground.
Dither is a faint monochrome grain: smooth gradients without it band visibly
on 8-bit panels, with full-strength noise the PNG would be five times the size.
"""
import subprocess
import sys
from pathlib import Path

import numpy as np

W, H = 3840, 2160
INK = (16, 13, 26)       # the light scheme's rim lines: near-black, under an orange glow
GLOW = (255, 132, 62)


def smooth(a, b, x):
    t = np.clip((x - a) / (b - a), 0.0, 1.0)
    return t * t * (3 - 2 * t)


def pastel(c):
    """The light-scheme twin of a dark-ground colour: the same hue, lifted."""
    return tuple(min(255.0, v * 0.86 + 255 * 0.14) for v in c)


class Canvas:
    """Shapes in screen fractions (x of the width, y and every length of the height)."""

    def __init__(self, w, h, light):
        self.w, self.h, self.light = w, h, light
        self.y, self.x = np.mgrid[0:h, 0:w].astype(np.float32)
        self.img = None

    def dist(self, cx, cy, ax=1.0, ay=1.0, rot=0.0):
        dx, dy = (self.x - cx * self.w) / self.h, (self.y - cy * self.h) / self.h
        if rot:
            c, s = np.cos(rot), np.sin(rot)
            dx, dy = c * dx + s * dy, -s * dx + c * dy
        return np.sqrt((dx / ax) ** 2 + (dy / ay) ** 2)

    def ground(self, dark=(14, 16, 40), light=(214, 214, 238)):
        yn = self.y / self.h
        if self.light:
            g = np.stack([light[0] + 14 * yn, light[1] + 12 * yn, light[2] - 4 * yn], -1)
        else:
            g = np.stack([dark[0] + 10 * yn, dark[1] + 14 * yn, dark[2] + 30 * yn], -1)
        self.img = g.astype(np.float32)

    def put(self, mask, col, k=1.0, lcol=None, lk=None):
        """Dark: add light. Light: lay colour over the ground."""
        m = mask[..., None]
        if self.light:
            c = np.array(lcol if lcol is not None else pastel(col), np.float32)
            a = np.clip(m * (k if lk is None else lk), 0, 1)
            self.img = self.img * (1 - a) + c * a
        else:
            self.img = self.img + m * np.array(col, np.float32) * k

    def cover(self, mask, col, lcol=None, k=1.0):
        c = np.array(lcol if (self.light and lcol is not None) else col, np.float32)
        a = np.clip(mask[..., None] * k, 0, 1)
        self.img = self.img * (1 - a) + c * a

    def glow(self, cx, cy, r, col, k=0.9, lk=None, **kw):
        d = self.dist(cx, cy, **kw)
        self.put(np.clip(1 - d / r, 0, 1) ** 1.6, col, k, lk=lk)

    def ring(self, cx, cy, rad, w, col, k=1.0, lcol=None, lk=None, **kw):
        d = self.dist(cx, cy, **kw)
        self.put(np.exp(-(((d - rad) / w) ** 2)), col, k, lcol, lk)

    def disc(self, cx, cy, rad, col, k=0.5, soft=0.006, lcol=None, lk=None, **kw):
        d = self.dist(cx, cy, **kw)
        self.put(1 - smooth(rad - soft, rad + soft, d), col, k, lcol, lk)

    def diamond(self, px, py, size=0.012):
        d = np.hypot(self.x - px * self.w, self.y - py * self.h) / self.h
        dia = np.exp(-((d / size) ** 1.2)) * 3.0 + 0.6 * np.exp(-((d / (size * 6.5)) ** 1.1))
        streak = (np.exp(-np.abs(self.y - py * self.h) / (0.0018 * self.h))
                  * np.exp(-np.abs(self.x - px * self.w) / (0.40 * self.h)))
        self.put(dia + 0.5 * streak, (255, 240, 215), 1.0, lcol=(255, 252, 240), lk=0.9)

    def done(self):
        return np.clip(self.img / 255.0, 0, 1).astype(np.float32)


def v_arc(c):
    c.ground()
    c.glow(0.729, 0.25, 0.75, (255, 170, 40), 0.9, lk=0.85)
    c.glow(0.156, 0.75, 0.667, (40, 140, 255), 0.8)
    c.glow(0.5, 0.0, 0.583, (255, 70, 140), 0.6, lk=0.65)
    c.ring(0.5, 0.75, 0.7833, 0.0333, (255, 120, 60), 0.7, lcol=(255, 132, 62), lk=0.85)
    c.ring(0.5, 0.75, 0.7333, 0.0117, (255, 225, 150), 1.0, lcol=INK, lk=0.95)


def v_fill(c):
    """The arc, with the ring holding a second colour."""
    c.ground(dark=(10, 12, 34))
    c.glow(0.729, 0.25, 0.75, (255, 170, 40), 0.7, lk=0.7)
    c.glow(0.5, 0.0, 0.583, (255, 70, 140), 0.5, lk=0.55)
    c.disc(0.5, 0.75, 0.7333, (70, 50, 190), 0.80, soft=0.05, lcol=(150, 128, 238), lk=0.62)
    c.glow(0.156, 0.75, 0.667, (40, 160, 255), 0.7, lk=0.7)
    c.ring(0.5, 0.75, 0.7833, 0.0333, (255, 120, 60), 0.7, lcol=(255, 132, 62), lk=0.85)
    c.ring(0.5, 0.75, 0.7333, 0.0117, (255, 225, 150), 1.0, lcol=INK, lk=0.95)


def v_totality(c):
    """A moon right of centre with its corona, as the name says."""
    c.ground(dark=(8, 10, 30))
    c.glow(0.70, 0.42, 0.75, (255, 150, 50), 0.80, lk=0.8)
    c.glow(0.12, 0.85, 0.60, (40, 130, 255), 0.75)
    c.glow(0.40, 0.0, 0.50, (255, 70, 150), 0.55, lk=0.6)
    c.ring(0.70, 0.42, 0.235, 0.040, (255, 130, 60), 0.85, lcol=(255, 138, 70), lk=0.85)
    c.ring(0.70, 0.42, 0.205, 0.008, (255, 235, 170), 1.2, lcol=INK, lk=0.95)
    c.cover(1 - smooth(0.199, 0.205, c.dist(0.70, 0.42)), (4, 5, 16), lcol=(34, 28, 80))
    c.diamond(0.70 + 0.205 * np.cos(-0.75) * c.h / c.w, 0.42 + 0.205 * np.sin(-0.75))


def v_crescent(c):
    c.ground(dark=(10, 10, 30))
    c.glow(0.27, 0.58, 0.85, (255, 140, 40), 0.55, lk=0.7)
    c.glow(0.85, 0.20, 0.60, (80, 90, 255), 0.7)
    c.glow(0.70, 0.95, 0.60, (255, 70, 140), 0.55, lk=0.6)
    lit = np.clip((1 - smooth(0.329, 0.341, c.dist(0.30, 0.55)))
                  - (1 - smooth(0.329, 0.341, c.dist(0.35, 0.50))), 0, 1)
    c.put(lit, (255, 215, 140), 1.5, lcol=(226, 140, 30), lk=0.95)
    c.ring(0.30, 0.55, 0.352, 0.030, (255, 140, 70), 0.35, lcol=(255, 138, 70), lk=0.6)
    c.cover(1 - smooth(0.329, 0.341, c.dist(0.35, 0.50)), (6, 7, 22), lcol=(56, 48, 110), k=0.92)


def v_twin(c):
    c.ground(dark=(12, 10, 36))
    c.glow(0.5, 0.5, 0.85, (150, 70, 240), 0.55, lk=0.6)
    c.glow(0.15, 0.1, 0.55, (255, 80, 140), 0.55, lk=0.6)
    c.glow(0.90, 0.95, 0.55, (40, 170, 220), 0.60)
    a, b = c.dist(0.405, 0.5), c.dist(0.595, 0.5)
    lens = (1 - smooth(0.33, 0.345, a)) * (1 - smooth(0.33, 0.345, b))
    c.put(lens, (255, 255, 255), 0.30, lcol=(255, 255, 255), lk=0.45)
    c.ring(0.405, 0.5, 0.34, 0.009, (255, 200, 110), 1.0, lcol=INK, lk=0.95)
    c.ring(0.595, 0.5, 0.34, 0.009, (120, 190, 255), 1.0, lcol=(60, 90, 215), lk=0.95)
    c.ring(0.405, 0.5, 0.37, 0.030, (255, 140, 60), 0.45, lcol=(255, 138, 70), lk=0.65)
    c.ring(0.595, 0.5, 0.37, 0.030, (70, 150, 255), 0.45, lcol=(120, 160, 255), lk=0.4)


def v_orbit(c):
    c.ground(dark=(10, 12, 34))
    c.glow(0.95, 0.95, 0.95, (255, 160, 50), 0.85, lk=0.85)
    c.glow(0.10, 0.15, 0.60, (80, 100, 255), 0.65)
    c.glow(0.45, 0.0, 0.50, (255, 70, 150), 0.45, lk=0.55)
    for r, w, col, lc in ((0.20, 0.006, (255, 235, 170), INK), (0.38, 0.012, (255, 150, 70), INK),
                          (0.60, 0.006, (255, 110, 170), (225, 100, 150)), (0.86, 0.020, (120, 130, 255), (110, 110, 230)),
                          (1.20, 0.008, (255, 235, 170), INK)):
        c.ring(1.0, 1.0, r, w, col, 1.0, lcol=lc, lk=0.9)
        c.ring(1.0, 1.0, r + 0.035, w * 4, col, 0.25, lcol=GLOW if lc == INK else lc, lk=0.55)


def v_horizon(c):
    c.ground(dark=(8, 10, 34))
    c.glow(0.5, 0.62, 0.80, (255, 140, 40), 0.85, lk=0.85)
    c.glow(0.15, 0.10, 0.60, (70, 100, 255), 0.65)
    c.glow(0.90, 0.05, 0.55, (255, 70, 150), 0.55, lk=0.6)
    c.disc(0.5, 1.55, 1.0, (255, 170, 60), 0.55, soft=0.09, lcol=(255, 200, 120), lk=0.6)
    c.ring(0.5, 1.55, 1.045, 0.040, (255, 110, 60), 0.7, lcol=(255, 132, 62), lk=0.8)
    c.ring(0.5, 1.55, 1.0, 0.010, (255, 235, 175), 1.2, lcol=INK, lk=0.95)
    c.diamond(0.5 + np.cos(-1.2) * c.h / c.w, 1.55 + np.sin(-1.2), 0.010)


def v_tilt(c):
    c.ground(dark=(8, 10, 32))
    c.glow(0.55, 0.45, 0.80, (255, 150, 50), 0.60, lk=0.65)
    c.glow(0.10, 0.90, 0.60, (40, 190, 200), 0.70)
    c.glow(0.90, 0.12, 0.55, (255, 70, 150), 0.60, lk=0.65)
    rot = -0.42
    c.ring(0.55, 0.50, 0.48, 0.050, (255, 130, 60), 0.50, lcol=(255, 138, 70), lk=0.75, ax=1.55, ay=0.50, rot=rot)
    c.ring(0.55, 0.50, 0.48, 0.0105, (255, 232, 165), 1.2, lcol=INK, lk=0.95, ax=1.55, ay=0.50, rot=rot)
    c.ring(0.55, 0.50, 0.34, 0.006, (120, 190, 255), 0.9, lcol=(70, 110, 225), lk=0.8, ax=1.55, ay=0.50, rot=rot)
    c.cover(1 - smooth(0.099, 0.105, c.dist(0.55, 0.50)), (6, 7, 22), lcol=(40, 34, 96))
    c.ring(0.55, 0.50, 0.105, 0.007, (255, 232, 165), 1.0, lcol=INK, lk=0.9)


VARIANTS = {"arc": v_arc, "fill": v_fill, "totality": v_totality, "crescent": v_crescent,
            "twin": v_twin, "orbit": v_orbit, "horizon": v_horizon, "tilt": v_tilt}


def render(light: bool, variant: str = "arc", w: int = W, h: int = H) -> np.ndarray:
    c = Canvas(w, h, light)
    VARIANTS[variant](c)
    return c.done()


def encode(img: np.ndarray, path: Path) -> None:
    h, w, _ = img.shape
    rng = np.random.default_rng(7)
    # One grey grain value per pixel, applied to all channels, on a third of
    # the pixels: enough to break 8-bit banding, cheap for deflate.
    n = rng.random((h, w), dtype=np.float32) - 0.5
    n = np.where(rng.random((h, w)) < 0.03, n * 2.0, 0.0).astype(np.float32)
    q = np.clip(np.floor(img * 255.0 + 0.5 + n[..., None]), 0, 255).astype(np.uint8)
    subprocess.run(
        ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24",
         "-s", f"{w}x{h}", "-i", "-", "-compression_level", "100", "-frames:v", "1", str(path)],
        input=q.tobytes(), check=True)


def main() -> None:
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    out = Path(args[0]) if args else Path(__file__).resolve().parent
    out.mkdir(parents=True, exist_ok=True)
    if "--preview" in sys.argv:
        for v in VARIANTS:
            for name, light in (("dark", False), ("light", True)):
                encode(render(light, v, 1920, 1080), out / f"eclipse-{v}-{name}-1920x1080.png")
        return
    for v in (VARIANTS if "--all" in sys.argv else ["arc"]):
        for name, light in (("dark", False), ("light", True)):
            big = render(light, v)
            tag = "eclipse" if v == "arc" else f"eclipse-{v}"
            encode(big, out / f"{tag}-{name}-3840x2160.png")
            small = big.reshape(H // 2, 2, W // 2, 2, 3).mean(axis=(1, 3))
            encode(small.astype(np.float32), out / f"{tag}-{name}-1920x1080.png")


if __name__ == "__main__":
    main()
