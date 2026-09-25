# SPDX-License-Identifier: AGPL-3.0-only
"""Pure-stdlib image helpers for the VM harness: PPM/PNG codec, diffing, heuristics.

Images are `Image(w, h, data)` with `data` = packed 8-bit RGB (len == 3*w*h).
"""
import struct
import zlib

PNG_SIG = b"\x89PNG\r\n\x1a\n"


class ImageError(Exception):
    pass


class Image:
    def __init__(self, w, h, data):
        if len(data) != 3 * w * h:
            raise ImageError("pixel buffer size does not match %dx%d" % (w, h))
        self.w, self.h, self.data = w, h, bytes(data)


# ---------------------------------------------------------------- PPM

def parse_ppm(raw):
    """Parse a binary P6 PPM (what QMP screendump emits by default)."""
    pos, toks = 0, []
    if raw[:2] != b"P6":
        raise ImageError("not a P6 PPM")
    pos = 2
    while len(toks) < 3:
        while pos < len(raw) and raw[pos:pos + 1].isspace():
            pos += 1
        if raw[pos:pos + 1] == b"#":
            while pos < len(raw) and raw[pos:pos + 1] != b"\n":
                pos += 1
            continue
        start = pos
        while pos < len(raw) and not raw[pos:pos + 1].isspace():
            pos += 1
        if start == pos:
            raise ImageError("truncated PPM header")
        toks.append(int(raw[start:pos]))
    pos += 1  # single whitespace after maxval
    w, h, maxval = toks
    if maxval != 255:
        raise ImageError("unsupported PPM maxval %d" % maxval)
    body = raw[pos:pos + 3 * w * h]
    if w <= 0 or h <= 0 or len(body) != 3 * w * h:
        raise ImageError("PPM body truncated")
    return Image(w, h, body)


# ---------------------------------------------------------------- PNG

def _chunk(tag, payload):
    crc = zlib.crc32(tag + payload) & 0xFFFFFFFF
    return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", crc)


def encode_png(img):
    """RGB8 PNG, filter 0 on every row (fast to write and to read back)."""
    stride = 3 * img.w
    rows = b"".join(b"\x00" + img.data[y * stride:(y + 1) * stride] for y in range(img.h))
    return (PNG_SIG
            + _chunk(b"IHDR", struct.pack(">IIBBBBB", img.w, img.h, 8, 2, 0, 0, 0))
            + _chunk(b"IDAT", zlib.compress(rows, 6))
            + _chunk(b"IEND", b""))


def _paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def decode_png(raw):
    """Decode a non-interlaced 8-bit gray/RGB/palette/GA/RGBA PNG to RGB."""
    if raw[:8] != PNG_SIG:
        raise ImageError("bad PNG signature")
    pos, ihdr, plte, idat = 8, None, None, []
    while pos + 8 <= len(raw):
        (n,) = struct.unpack(">I", raw[pos:pos + 4])
        tag, payload = raw[pos + 4:pos + 8], raw[pos + 8:pos + 8 + n]
        if len(payload) != n:
            raise ImageError("truncated PNG chunk")
        if struct.unpack(">I", raw[pos + 8 + n:pos + 12 + n])[0] != (
                zlib.crc32(tag + payload) & 0xFFFFFFFF):
            raise ImageError("PNG chunk CRC mismatch")
        if tag == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", payload)
        elif tag == b"PLTE":
            plte = payload
        elif tag == b"IDAT":
            idat.append(payload)
        elif tag == b"IEND":
            break
        pos += 12 + n
    if ihdr is None or not idat:
        raise ImageError("PNG missing IHDR/IDAT")
    w, h, depth, ctype, _c, _f, interlace = ihdr
    if depth != 8 or interlace != 0 or ctype not in (0, 2, 3, 4, 6):
        raise ImageError("unsupported PNG (depth=%d ctype=%d interlace=%d)" % (depth, ctype, interlace))
    bpp = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    stride = w * bpp
    data = zlib.decompress(b"".join(idat))
    if len(data) != h * (stride + 1):
        raise ImageError("PNG pixel data size mismatch")
    prev = bytearray(stride)
    out = bytearray()
    for y in range(h):
        base = y * (stride + 1)
        ft = data[base]
        row = bytearray(data[base + 1:base + 1 + stride])
        if ft == 0:
            pass
        elif ft == 1:
            for i in range(bpp, stride):
                row[i] = (row[i] + row[i - bpp]) & 255
        elif ft == 2:
            row = bytearray((a + b) & 255 for a, b in zip(row, prev))
        elif ft == 3:
            for i in range(stride):
                left = row[i - bpp] if i >= bpp else 0
                row[i] = (row[i] + ((left + prev[i]) >> 1)) & 255
        elif ft == 4:
            for i in range(stride):
                left = row[i - bpp] if i >= bpp else 0
                ul = prev[i - bpp] if i >= bpp else 0
                row[i] = (row[i] + _paeth(left, prev[i], ul)) & 255
        else:
            raise ImageError("bad PNG filter %d" % ft)
        prev = row
        if ctype == 2:
            out += row
        elif ctype == 6:
            out += b"".join(row[i:i + 3] for i in range(0, stride, 4))
        elif ctype == 0:
            out += b"".join(bytes((v, v, v)) for v in row)
        elif ctype == 4:
            out += b"".join(bytes((row[i],) * 3) for i in range(0, stride, 2))
        else:
            if plte is None:
                raise ImageError("palette PNG without PLTE")
            out += b"".join(plte[3 * v:3 * v + 3] for v in row)
    return Image(w, h, out)


def load_image(path):
    with open(path, "rb") as f:
        raw = f.read()
    if raw[:2] == b"P6":
        return parse_ppm(raw)
    return decode_png(raw)


def save_png(img, path):
    with open(path, "wb") as f:
        f.write(encode_png(img))


def validate_png_file(path):
    """Return (w, h, distinct_colours_sampled) or raise ImageError. Non-empty check."""
    img = load_image(path)
    seen = set()
    d = img.data
    step = max(1, (img.w * img.h) // 20000)
    for i in range(0, img.w * img.h, step):
        seen.add(d[3 * i:3 * i + 3])
    return img.w, img.h, len(seen)


# ---------------------------------------------------------------- diff

def parse_region(s):
    x, y, w, h = (int(v) for v in s.split(","))
    return (x, y, w, h)


def _mask(img, regions):
    if not regions:
        return img.data, 0
    buf = bytearray(img.data)
    masked = 0
    for x, y, w, h in regions:
        x0, y0 = max(0, x), max(0, y)
        x1, y1 = min(img.w, x + w), min(img.h, y + h)
        if x1 <= x0 or y1 <= y0:
            continue
        masked += (x1 - x0) * (y1 - y0)
        for yy in range(y0, y1):
            buf[3 * (yy * img.w + x0):3 * (yy * img.w + x1)] = bytes(3 * (x1 - x0))
    return bytes(buf), masked


def diff_images(a, b, tolerance=0, ignore=()):
    """Compare two Images. `tolerance` = per-channel delta (0..255) below which a pixel
    counts as equal. Returns dict: mean_abs (0..255 over compared bytes), frac_diff
    (fraction of compared pixels differing), size_mismatch."""
    if (a.w, a.h) != (b.w, b.h):
        return {"mean_abs": 255.0, "frac_diff": 1.0, "size_mismatch": True,
                "a_size": (a.w, a.h), "b_size": (b.w, b.h)}
    da, masked = _mask(a, ignore)
    db, _ = _mask(b, ignore)
    npx = a.w * a.h - min(masked, a.w * a.h)
    if npx <= 0:
        return {"mean_abs": 0.0, "frac_diff": 0.0, "size_mismatch": False}
    if da == db:
        return {"mean_abs": 0.0, "frac_diff": 0.0, "size_mismatch": False}
    d = [abs(x - y) for x, y in zip(da, db)]
    mean_abs = sum(d) / (3 * npx)
    bad = sum(1 for r, g, bl in zip(d[0::3], d[1::3], d[2::3])
              if r > tolerance or g > tolerance or bl > tolerance)
    return {"mean_abs": mean_abs, "frac_diff": bad / npx, "size_mismatch": False}


def matches(res, max_frac, max_mean=None):
    if res["size_mismatch"]:
        return False
    if res["frac_diff"] > max_frac:
        return False
    return max_mean is None or res["mean_abs"] <= max_mean


# ---------------------------------------------------------------- heuristics

def screen_stats(img, sample=4):
    """Sampled stats: nonblack fraction, colourful fraction among nonblack pixels,
    bounding box of nonblack pixels (in pixels)."""
    d, w, h = img.data, img.w, img.h
    total = nb = colourful = 0
    x0, y0, x1, y1 = w, h, -1, -1
    for y in range(0, h, sample):
        row = y * w
        for x in range(0, w, sample):
            i = 3 * (row + x)
            r, g, b = d[i], d[i + 1], d[i + 2]
            total += 1
            m = max(r, g, b)
            if m > 40:
                nb += 1
                if m - min(r, g, b) > 40:
                    colourful += 1
                if x < x0: x0 = x
                if y < y0: y0 = y
                if x > x1: x1 = x
                if y > y1: y1 = y
    return {"nonblack": nb / total, "colourful": (colourful / nb) if nb else 0.0,
            "bbox": (x0, y0, x1, y1) if nb else None}


def is_blank(img):
    return screen_stats(img)["nonblack"] < 0.001


def looks_like_text_console(img):
    """Heuristic for a Linux VT / getty `login:` screen: mostly black, sparse,
    grey (uncoloured) glyphs. A gold-on-black GUI, or any filled UI, fails this."""
    s = screen_stats(img)
    return 0.0005 <= s["nonblack"] <= 0.08 and s["colourful"] < 0.10
