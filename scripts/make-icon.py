"""Generates assets/tilework.ico (standard library only).

Three green tiles (one tall column, two stacked) on a dark rounded square, rendered with 4x4
supersampling at every icon size so small sizes stay crisp. Run: python scripts/make-icon.py
"""
import struct
import zlib
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
BACKGROUND = (0x1F, 0x29, 0x37)
TILE = (0x2E, 0xCC, 0x71)
SS = 4


def inside_rounded(x, y, x0, y0, x1, y1, r):
    if not (x0 <= x <= x1 and y0 <= y <= y1):
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def render(n):
    # Geometry in unit coordinates (0..1).
    bg = (0.0, 0.0, 1.0, 1.0, 0.18)
    pad, gap = 0.17, 0.07 if n >= 32 else 0.09
    mid_x = 0.5
    tiles = [
        (pad, pad, mid_x - gap / 2, 1 - pad),
        (mid_x + gap / 2, pad, 1 - pad, 0.5 - gap / 2),
        (mid_x + gap / 2, 0.5 + gap / 2, 1 - pad, 1 - pad),
    ]
    tile_r = 0.05
    pixels = []
    for py in range(n):
        row = []
        for px in range(n):
            cover_bg = cover_tile = 0
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS) / n
                    y = (py + (sy + 0.5) / SS) / n
                    if inside_rounded(x, y, *bg):
                        cover_bg += 1
                        if any(inside_rounded(x, y, *t, tile_r) for t in tiles):
                            cover_tile += 1
            total = SS * SS
            alpha = cover_bg / total
            if cover_bg:
                t = cover_tile / cover_bg
                rgb = tuple(round(BACKGROUND[i] * (1 - t) + TILE[i] * t) for i in range(3))
            else:
                rgb = (0, 0, 0)
            row.append((*rgb, round(alpha * 255)))
        pixels.append(row)
    return pixels


def png(pixels):
    n = len(pixels)
    raw = b"".join(b"\x00" + bytes(c for p in row for c in p) for row in pixels)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def dib(pixels):
    n = len(pixels)
    header = struct.pack("<IiiHHIIiiII", 40, n, n * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    color = b"".join(bytes((b, g, r, a)) for row in reversed(pixels) for (r, g, b, a) in row)
    mask_row = ((n + 31) // 32) * 4
    return header + color + b"\x00" * (mask_row * n)


def main():
    images = []
    for n in SIZES:
        pixels = render(n)
        images.append((n, png(pixels) if n >= 256 else dib(pixels)))
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for n, data in images:
        dim = 0 if n >= 256 else n
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    out += b"".join(data for _, data in images)
    target = Path(__file__).resolve().parent.parent / "assets" / "tilework.ico"
    target.parent.mkdir(exist_ok=True)
    target.write_bytes(out)
    print(f"wrote {target} ({len(out)} bytes)")


if __name__ == "__main__":
    main()
