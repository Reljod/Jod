"""Generate Jod's source app icon: an arc-reactor style mark, stdlib only."""
import math
import struct
import sys
import zlib

S = 1024
BG = (11, 15, 20)
ACCENT = (76, 194, 255)
CORNER = 200.0


def rounded_alpha(x, y):
    """Alpha for a rounded-square mask, antialiased at the corners."""
    cx = min(x, S - 1 - x)
    cy = min(y, S - 1 - y)
    if cx >= CORNER or cy >= CORNER:
        return 255
    dx = CORNER - cx
    dy = CORNER - cy
    d = math.hypot(dx, dy)
    return max(0, min(255, int(round((CORNER - d) * 255))))


def coverage(d, inner, outer):
    """Antialiased coverage of an annulus at distance d."""
    if d < inner - 1 or d > outer + 1:
        return 0.0
    a = min(1.0, max(0.0, d - (inner - 1)))
    b = min(1.0, max(0.0, (outer + 1) - d))
    return min(a, b)


rows = []
mid = (S - 1) / 2.0
for y in range(S):
    row = bytearray()
    row.append(0)  # PNG filter: none
    for x in range(S):
        d = math.hypot(x - mid, y - mid)

        # Outer ring, broken by a gap at the top so it reads as a dial.
        ring = coverage(d, 300, 372)
        if ring > 0:
            angle = math.degrees(math.atan2(y - mid, x - mid)) % 360
            # atan2 measures clockwise from +x in image space; 270 is the top.
            if 258 <= angle <= 282:
                ring = 0.0

        core = coverage(d, 0, 150)
        halo = coverage(d, 196, 214) * 0.6
        ink = min(1.0, ring + core + halo)

        r = int(round(BG[0] + (ACCENT[0] - BG[0]) * ink))
        g = int(round(BG[1] + (ACCENT[1] - BG[1]) * ink))
        b = int(round(BG[2] + (ACCENT[2] - BG[2]) * ink))
        row += bytes((r, g, b, rounded_alpha(x, y)))
    rows.append(bytes(row))

raw = b"".join(rows)


def chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


png = (
    b"\x89PNG\r\n\x1a\n"
    + chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0))
    + chunk(b"IDAT", zlib.compress(raw, 9))
    + chunk(b"IEND", b"")
)

with open(sys.argv[1], "wb") as fh:
    fh.write(png)
print(f"wrote {sys.argv[1]} ({len(png)} bytes)")
