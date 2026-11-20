# SPDX-License-Identifier: AGPL-3.0-only
"""Check sans-{1,2,3}x.bin against the exact-integer-cell contract."""
import pathlib, struct, sys
D = pathlib.Path(__file__).resolve().parent
NG, PADX, CH, BASE = 95, 2, 16, 12
WN = ["Regular", "Medium", "SemiBold"]
bad = []
def load(s):
    b = (D / f"sans-{s}x.bin").read_bytes()
    assert b[:4] == b"TSN1" and tuple(b[4:10]) == (3, NG, PADX, CH, BASE, s), "header"
    adv = [list(b[10 + w * NG: 10 + (w + 1) * NG]) for w in range(3)]
    off = 10 + 3 * NG
    cells = {}
    for w in range(3):
        for g in range(NG):
            cw = (adv[w][g] + 2 * PADX) * s
            n = cw * CH * s
            cells[w, g] = (cw, b[off:off + n]); off += n
    if off != len(b): bad.append(f"size {s}x: {off} != {len(b)}")
    return adv, cells
def bbox(cw, d, s):
    xs, ys = [], []
    for i, v in enumerate(d):
        if v: xs.append(i % cw); ys.append(i // cw)
    return (min(xs), min(ys), max(xs) + 1, max(ys) + 1) if xs else None
S = {s: load(s) for s in (1, 2, 3)}
for s in (2, 3):
    if S[s][0] != S[1][0]: bad.append(f"advances differ at {s}x")
adv = S[1][0]
for w in range(3):
    if len({adv[w][0x30 - 0x20 + i] for i in range(10)}) != 1: bad.append(f"digits not tabular {WN[w]}")
    if adv[w][0] <= 0: bad.append("space adv 0")
for (w, g), (cw, d) in S[1][1].items():
    b1 = bbox(cw, d, 1)
    for s in (2, 3):
        cws, ds = S[s][1][w, g]
        bs = bbox(cws, ds, s)
        if (b1 is None) != (bs is None): bad.append(f"ink mismatch {WN[w]} {chr(g+32)!r} {s}x"); continue
        if b1 is None: continue
        sc = s
        # edge-pixel fuzz: 1x bbox is rounded outward, allow 1 px*s
        for a, c in zip(b1, bs):
            if abs(a * sc - c) > sc: bad.append(f"bbox drift {WN[w]} {chr(g+32)!r} {s}x {b1} {bs}"); break
        # clipping: ink touching the cell edge
        if bs[0] == 0 or bs[1] == 0 or bs[2] == cws or bs[3] == CH * s: bad.append(f"edge ink {WN[w]} {chr(g+32)!r} {s}x")
print("adv Regular: space", adv[0][0], "i", adv[0][ord('i')-32], "m", adv[0][ord('m')-32], "W", adv[0][ord('W')-32], "0", adv[0][16])
for w in range(3):
    print(WN[w], "ink bbox w x h (1x):", end=" ")
    for ch in "lI1|0Oo":
        cw, d = S[1][1][w, ord(ch) - 32]; b = bbox(cw, d, 1)
        print(f"{ch}={b[2]-b[0]}x{b[3]-b[1]}", end=" ")
    print()
# shape difference between l I 1 |
cw = {}
for ch in "lI1|":
    c, d = S[3][1][0, ord(ch) - 32]; cw[ch] = (c, d)
for a, b in (("l","I"),("l","|"),("I","|"),("l","1"),("I","1")):
    (c1, d1), (c2, d2) = cw[a], cw[b]
    if c1 != c2: print(f"{a} vs {b}: 3x Regular differ (cell widths {c1}/{c2})"); continue
    print(f"{a} vs {b}: 3x Regular pixel sum abs diff {sum(abs(x-y) for x,y in zip(d1,d2))}")
print("FAIL" if bad else "OK"); [print(" ", x) for x in bad[:30]]
sys.exit(1 if bad else 0)
