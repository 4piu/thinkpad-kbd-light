"""Render the application icon.

Kept as source rather than a checked-in binary blob: it is the same keyboard-plus-rays
motif the tray draws at runtime (src/tray.rs), and generating both from geometry keeps
them consistent. Produces a multi-size .ico with a transparent background, plus previews.

    python assets/make_icon.py
"""
import math, struct, zlib, os

OUTLINE = (176, 186, 200)
BODY    = (58, 64, 74)
KEY     = (96, 105, 118)
RAY     = (96, 178, 246)

# Normalised geometry, so every size renders from one description.
BODY_RECT = (0.055, 0.40, 0.945, 0.925)
BODY_R    = 0.075
EDGE      = 0.05
RAYS_X    = (0.275, 0.50, 0.725)
RAY_W     = 0.10
RAY_Y     = (0.075, 0.325)
RAY_R     = 0.045
# Key grid: mostly texture. At 16px it blurs into a tone, which is what stops the icon
# reading as a plain dark slab.
KEYS_X    = (0.145, 0.855)
KEYS_Y    = (0.505, 0.845)
KEY_COLS  = 5
KEY_ROWS  = 2
KEY_GAP   = 0.022
KEY_R     = 0.012


def rr(px, py, x0, y0, x1, y1, r):
    """Signed distance to a rounded rectangle; negative inside."""
    cx, cy = (x0 + x1) / 2.0, (y0 + y1) / 2.0
    hx, hy = (x1 - x0) / 2.0 - r, (y1 - y0) / 2.0 - r
    dx, dy = abs(px - cx) - hx, abs(py - cy) - hy
    return math.hypot(max(dx, 0.0), max(dy, 0.0)) + min(max(dx, dy), 0.0) - r


def shade(u, v):
    """Colour at normalised point, or None for transparent."""
    for rx in RAYS_X:
        if rr(u, v, rx - RAY_W / 2, RAY_Y[0], rx + RAY_W / 2, RAY_Y[1], RAY_R) < 0:
            return RAY
    d = rr(u, v, BODY_RECT[0], BODY_RECT[1], BODY_RECT[2], BODY_RECT[3], BODY_R)
    if d >= 0:
        return None
    if d > -EDGE:
        return OUTLINE
    kw = (KEYS_X[1] - KEYS_X[0] - KEY_GAP * (KEY_COLS - 1)) / KEY_COLS
    kh = (KEYS_Y[1] - KEYS_Y[0] - KEY_GAP * (KEY_ROWS - 1)) / KEY_ROWS
    for row in range(KEY_ROWS):
        ky = KEYS_Y[0] + row * (kh + KEY_GAP)
        for col in range(KEY_COLS):
            kx = KEYS_X[0] + col * (kw + KEY_GAP)
            if rr(u, v, kx, ky, kx + kw, ky + kh, KEY_R) < 0:
                return KEY
    return BODY


def render(size, ss=4):
    """Supersample then box-filter, so edges anti-alias into the alpha channel."""
    n = size * ss
    acc = [[0, 0, 0, 0] for _ in range(size * size)]
    for sy in range(n):
        v = (sy + 0.5) / n
        base = (sy // ss) * size
        for sx in range(n):
            c = shade((sx + 0.5) / n, v)
            if c is None:
                continue
            a = acc[base + sx // ss]
            a[0] += c[0]; a[1] += c[1]; a[2] += c[2]; a[3] += 1
    total = ss * ss
    out = bytearray()
    for px in acc:
        if px[3] == 0:
            out += bytes(4)
        else:
            out += bytes((px[0] // px[3], px[1] // px[3], px[2] // px[3],
                          min(255, px[3] * 255 // total)))
    return bytes(out)


def chunk(tag, data):
    return (struct.pack(">I", len(data)) + tag + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))


def png_bytes(size, rgba):
    raw = bytearray()
    for y in range(size):
        raw += bytes(1)
        raw += rgba[y * size * 4:(y + 1) * size * 4]
    sig = bytes((137, 80, 78, 71, 13, 10, 26, 10))
    return (sig
            + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))


def dib(size, rgba):
    """32-bit BGRA DIB, bottom-up, with the (unused) 1bpp AND mask ICO still expects."""
    hdr = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    rows = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            o = (y * size + x) * 4
            r, g, b, a = rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]
            rows += bytes((b, g, r, a))
    mask = bytes(((size + 31) // 32) * 4 * size)
    return hdr + bytes(rows) + mask


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    # Small sizes as uncompressed DIB, large ones PNG-compressed. Storing 128 and 256 as
    # DIB added ~340 KB to every binary, absurd for a program otherwise a few hundred KB.
    # Windows has understood PNG entries in .ico since Vista.
    sizes = [16, 20, 24, 32, 48, 64, 128, 256]
    images = []
    for s in sizes:
        rgba = render(s)
        images.append((s, png_bytes(s, rgba) if s >= 128 else dib(s, rgba)))

    offset = 6 + 16 * len(images)
    out = struct.pack("<HHH", 0, 1, len(images))
    blobs = b""
    for s, data in images:
        out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    with open(os.path.join(here, "tkbl.ico"), "wb") as f:
        f.write(out + blobs)

    with open(os.path.join(here, "icon-preview.png"), "wb") as f:
        f.write(png_bytes(256, render(256)))

    print("tkbl.ico: %d sizes, %d bytes" % (len(images), len(out) + len(blobs)))


if __name__ == "__main__":
    main()
