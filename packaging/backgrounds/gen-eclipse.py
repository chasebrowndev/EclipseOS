#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Procedural EclipseOS wallpaper: a dark moon with a gold corona.

    python3 packaging/backgrounds/gen-eclipse.py [OUT_DIR]

Writes eclipse-{dark,light}-{3840x2160,1920x1080}.png (numpy for the maths,
ffmpeg for PNG encoding; no other dependency). Deterministic: the same script
gives the same bytes. The image is rendered once at 3840x2160 and the small
size is a box-filtered downscale, so both have the same composition.

Composition: the disc sits right of centre and high, so the centred glass
sheets of the installer and Settings overlap its limb and blur it, and the
left of the screen stays quiet for a bar or a launcher. Gold is the corona
only (the accent seed #f2c33c), everything else is the warm near-black / paper
white of docs/STYLE.md. Dither is a faint monochrome grain: smooth gradients
without it band visibly on 8-bit panels, with full-strength noise the PNG
would be five times the size.
"""
import subprocess
import sys
from pathlib import Path

import numpy as np

W, H = 3840, 2160
GOLD = np.array([0.949, 0.765, 0.235])          # #f2c33c
HOT = np.array([1.0, 0.93, 0.72])               # the diamond: gold lit toward white
DEEP = np.array([0.55, 0.30, 0.06])             # corona's far glow


def smooth(a, b, x):
    t = np.clip((x - a) / (b - a), 0.0, 1.0)
    return t * t * (3 - 2 * t)


def render(light: bool) -> np.ndarray:
    rng = np.random.default_rng(1969)
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    cx, cy, R = 0.765 * W, 0.40 * H, 0.20 * H
    dx, dy = x - cx, y - cy
    d = np.hypot(dx, dy)
    th = np.arctan2(dy, dx)
    r = d / R                                    # 1.0 at the limb

    # Ground: warm near-black with a long, soft lift toward the eclipse.
    if light:
        base, lift = np.array([0.980, 0.976, 0.965]), np.array([1.0, 0.985, 0.93])
    else:
        base, lift = np.array([0.043, 0.035, 0.024]), np.array([0.105, 0.078, 0.036])
    vx, vy = x / W - 0.5, y / H - 0.5
    vig = smooth(0.30, 0.95, np.hypot(vx * 1.1, vy))          # edges settle
    halo = np.exp(-(np.maximum(r - 1, 0) / 3.2) ** 1.3)       # broad warm air
    img = base[None, None, :] + (lift - base)[None, None, :] * halo[..., None] * (0.25 if light else 1.0)
    if light:
        img = img * (1 - 0.05 * vig[..., None]) 
    else:
        img = img * (1 - 0.55 * vig[..., None])

    # Corona: streamers (angular low-frequency noise) on a radial falloff.
    ang = np.zeros_like(th)
    for k, amp in ((3, 1.0), (5, 0.8), (8, 0.5), (13, 0.3), (21, 0.15)):
        ang += amp * np.sin(k * th + rng.uniform(0, 6.283))
    ang = 0.5 + 0.5 * ang / 2.2
    over = np.maximum(r - 1.0, 0.0)
    streamer = (0.45 + 0.6 * ang) * np.exp(-over / (0.30 + 0.30 * ang))
    wide = np.exp(-over / 1.1) * 0.35
    cor = (streamer * 0.30 + wide * 0.6) * smooth(0.97, 1.012, r)
    # Inner ring: a thin, bright line hugging the limb, brighter toward the
    # upper right where the diamond sits.
    lim = np.exp(-(over / 0.010) ** 1.0) * smooth(0.985, 1.0, r)
    toward = 0.5 + 0.5 * np.cos(th - (-0.62))                  # -35 degrees
    lim *= 0.50 + 0.50 * toward ** 2.2
    # Diamond: a point of light on the limb, with a short horizontal flare.
    px, py = cx + R * np.cos(-0.62), cy + R * np.sin(-0.62)
    pd = np.hypot(x - px, y - py)
    diamond = np.exp(-(pd / (0.020 * R)) ** 1.2) + 0.25 * np.exp(-(pd / (0.12 * R)))
    flare = np.exp(-(np.abs(y - py) / (0.004 * R))) * np.exp(-(np.abs(x - px) / (0.45 * R)))
    glow_col = DEEP[None, None, :] + (GOLD - DEEP)[None, None, :] * np.clip(1.4 - over, 0, 1)[..., None] ** 1.5
    if light:
        # Light: the corona is a deep-gold wash on paper (adds colour, not light).
        tint = np.array([0.93, 0.62, 0.10])
        k = np.clip(cor * 0.60 + lim * 0.9, 0, 1)[..., None]
        img = img * (1 - k) + tint[None, None, :] * k * 0.90 + img * k * 0.0
        img = img * (1 - np.clip(diamond * 0.8, 0, 1)[..., None] * 0.0)
    else:
        img = img + glow_col * cor[..., None] * 0.75
        img = img + GOLD[None, None, :] * lim[..., None] * 0.95
        img = img + HOT[None, None, :] * (diamond * 0.9 + flare * 0.35)[..., None]

    # The moon: near-black with a barely-there sphere shading and a thin
    # earthshine crescent on the lower left, opposite the diamond.
    inside = 1.0 - smooth(0.997, 1.0015, r)
    rr = np.clip(r, 0, 1)
    z = np.sqrt(np.clip(1 - rr ** 2, 0, 1))
    nl = np.clip((-0.45 * dx / R + 0.55 * dy / R) * -1 * 0.0 + z * 0.6 + (-0.35) * (dx / R) * 0 , 0, 1)
    shade = 0.012 + 0.020 * z
    earth = smooth(0.82, 1.0, rr) * smooth(0.0, 1.0, 0.5 - 0.5 * np.cos(th - 2.5)) * 0.10
    if light:
        moon = np.array([0.165, 0.148, 0.125])[None, None, :] * (0.55 + 1.2 * z[..., None] * 0.45)
        moon = moon + np.array([0.12, 0.09, 0.03])[None, None, :] * earth[..., None] * 2.0
    else:
        moon = np.array([0.30, 0.22, 0.12])[None, None, :] * shade[..., None]
        moon = moon + np.array([0.52, 0.36, 0.14])[None, None, :] * earth[..., None] * 0.55
    img = img * (1 - inside[..., None]) + moon * inside[..., None]

    # Glass depth: two hairline orbits and a faint soft orb, as if a sheet of
    # glass lay over the sky. Quiet; they exist to give blur something to bend.
    sign = -1.0 if light else 1.0
    for rad, wd, a in ((1.62, 0.0018, 0.050), (2.35, 0.0018, 0.032)):
        ring = np.exp(-(((r - rad) / wd) ** 2) * 0.5)
        img = img + sign * 0.5 * a * ring[..., None] * (np.array([1.0, 0.95, 0.85]) if not light else np.array([0.3, 0.25, 0.2]))[None, None, :]
    ox, oy, orad = 0.205 * W, 0.80 * H, 0.17 * H
    od = np.hypot(x - ox, y - oy) / orad
    orb = np.exp(-(od ** 4) * 1.6) * 0.07 + np.exp(-(((od - 1) / 0.012) ** 2) * 0.5) * 0.028
    if light:
        img = img - np.array([0.10, 0.12, 0.17])[None, None, :] * orb[..., None] * 0.7
    else:
        img = img + np.array([0.45, 0.38, 0.24])[None, None, :] * orb[..., None] * 0.55

    return np.clip(img, 0, 1).astype(np.float32)


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
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent
    out.mkdir(parents=True, exist_ok=True)
    for name, light in (("dark", False), ("light", True)):
        big = render(light)
        encode(big, out / f"eclipse-{name}-3840x2160.png")
        small = big.reshape(H // 2, 2, W // 2, 2, 3).mean(axis=(1, 3))
        encode(small.astype(np.float32), out / f"eclipse-{name}-1920x1080.png")


if __name__ == "__main__":
    main()
