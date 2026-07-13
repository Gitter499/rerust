#!/usr/bin/env python3
"""Generate ReRust pixel art assets (stdlib only, no PIL)."""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "assets"

PALETTE = {
    ".": None,
    "K": (0x2A, 0x0E, 0x14),
    "R": (0xCE, 0x41, 0x2B),
    "r": (0xFF, 0x6A, 0x4D),
    "W": (0xE6, 0xE9, 0xEF),
    "D": (0x9A, 0x34, 0x24),
    "T": (0x5E, 0xEA, 0xD4),
    "t": (0x1F, 0x3D, 0x3A),
    "G": (0xFF, 0xCF, 0x6A),
    "M": (0x55, 0x2C, 0x37),
    "w": (0xAD, 0xB3, 0xC2),
    "A": (0x4A, 0xDE, 0x80),  # diff-add green
    "E": (0xFF, 0x6B, 0x6B),  # diff-delete red
}

# Original crab mascot: gold migration arrow, rust shell, white eyes, dark claws.
LOGO_32 = """
................................
................................
..........GGG...................
.........GrRWG..................
........GrRRRWG.................
.......GrRRRRRWG................
......GrRRRRRRRWG...............
.....GrRRRRRRRRRWG..............
....GrRRRRRRRRRRRWG.............
...GrRRRRRRRRRRRRRWG............
..GrRRRRRRRRRRRRRRRWG...........
.GrRRRRRRRRRRRRRRRRRWG..........
DrrRRRRRRWWWWRRRRRRRrrD.........
DrrRRRRRRWWWWRRRRRRRrrD.........
DrrRRRRRRRRRRRRRRRRRrrD.........
DrrRRRRRRRRRRRRRRRRRrrD.........
.DrrRRRRRRRRRRRRRRRRrrD.........
..DrrRRRRRRRRRRRRRRrrD..........
...DrrD........DrrD.............
....DrD........DrD..............
.....DD........DD...............
................................
................................
................................
................................
................................
................................
................................
................................
................................
................................
................................
""".strip()

# Bidirectional arrows — rewrite / migration badge.
ICON_REWRITE_16 = """
................
................
....WW.....WW...
...WWWW...WWWW..
..WWWWWW.WWWWWW.
..WWWWWW.WWWWWW.
...WWWW...WWWW..
....WW.....WW...
................
................
................
................
................
................
................
""".strip()

# Split fork — replacement / alternative badge.
ICON_REPLACEMENT_16 = """
................
.......WW.......
......WWWW......
.....WW..WW.....
....WW....WW....
...WW......WW...
...WW......WW...
....WW....WW....
.....WW..WW.....
......WWWW......
.......WW.......
................
................
................
................
................
""".strip()

# ---------------------------------------------------------------------------
# Header lockup: rustacean mascot + "ReRust" pixel wordmark, one sprite.
#
# Mark: an original crab (ferris-adjacent, not Ferris) with a rounded rust
# shell, hot top-light bevel, eyes glancing toward the wordmark, and a pixel
# smile. Both claws are raised: the left grips a diff-green `+` and the right
# a diff-red `-` — the crab is literally applying the rewrite patch. Splayed
# legs land on the wordmark baseline to ground the mark.
#
# Wordmark: hand-set 3px-stroke glyphs, 16px cap height, 11px x-height.
# "Re" is light (#E6E9EF) with a cool bottom shade; "Rust" is rust (#CE412B)
# with a hot top bevel (#ff6a4d) and dark bottom shade (#9A3424) so the word
# glows like heated metal. No underline — the migration story is told by the
# +/- diff symbols in the claws.
#
# Native canvas is 24px tall, exported at 6x (144px). Display at 72px
# (desktop) or 48px (mobile) so each art pixel maps to exactly 3 or 2 CSS px
# and `image-rendering: pixelated` stays crisp.

_CAP_R = [
    "XXXXXXXXXX..",
    "XXXXXXXXXXX.",
    "XXXXXXXXXXXX",
    "XXX......XXX",
    "XXX......XXX",
    "XXX......XXX",
    "XXX.....XXXX",
    "XXXXXXXXXXX.",
    "XXXXXXXXXX..",
    "XXX...XXX...",
    "XXX...XXXX..",
    "XXX....XXX..",
    "XXX....XXXX.",
    "XXX.....XXX.",
    "XXX.....XXXX",
    "XXX......XXX",
]

_LOW_E = [
    ".XXXXXXXXX.",
    "XXXXXXXXXXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXXXXXXXXXX",
    "XXXXXXXXXXX",
    "XXX........",
    "XXX........",
    "XXX.....XXX",
    "XXXXXXXXXXX",
    ".XXXXXXXXX.",
]

_LOW_U = [
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXX....XXXX",
    "XXXXXXXXXXX",
    ".XXXXXXXXX.",
]

_LOW_S = [
    ".XXXXXXXXX.",
    "XXXXXXXXXXX",
    "XXX.....XXX",
    "XXXX.......",
    ".XXXXXXXX..",
    "..XXXXXXXX.",
    ".......XXXX",
    "XXX.....XXX",
    "XXX.....XXX",
    "XXXXXXXXXXX",
    ".XXXXXXXXX.",
]

_LOW_T = [
    ".XXX.....",
    ".XXX.....",
    ".XXX.....",
    "XXXXXXXX.",
    "XXXXXXXX.",
    ".XXX.....",
    ".XXX.....",
    ".XXX.....",
    ".XXX.....",
    ".XXX.....",
    ".XXX.....",
    ".XXX..XXX",
    ".XXXXXXXX",
    "..XXXXXX.",
]

# (art, base color, top-bevel highlight, bottom shade); None = flat.
_WORD = [
    (_CAP_R, "W", None, "w"),
    (_LOW_E, "W", None, "w"),
    (_CAP_R, "R", "r", "D"),
    (_LOW_U, "R", "r", "D"),
    (_LOW_S, "R", "r", "D"),
    (_LOW_T, "R", "r", "D"),
]

_LOCKUP_H = 24
_BASELINE = 22  # first row below the letters
_KERN = 2
_MASCOT_GAP = 5  # px between the crab and the wordmark

# The lockup mascot: an original rustacean applying the rewrite patch.
# 30x21 grid. The left pincer grips a 2px-stroke diff-green `+` (its stem
# runs down into the claw gap — the Rust being added); the right pincer
# balances a diff-red `-` on its prongs (the legacy code being deleted).
# Rounded rust shell with a hot top bevel and dark right shade, eyes looking
# right toward the wordmark, wide smile, four splayed legs whose feet land
# on the wordmark baseline.
LOCKUP_CRAB = """
...AA.........................
...AA.........................
.AAAAAA.......................
.AAAAAA...............EEEEEEEE
...AA.................EEEEEEEE
...AA.........................
.RRAARR................RR..RR.
.RRAARR................RR..RR.
.RRRRRR................RRRRRR.
..RRRR..................RRRR..
...RR.....rrrrrrrrrr.....RR...
.....RRrrRRRRRRRRRRRRrrRR.....
......rRRRRRRRRRRRRRRRRD......
.....rRRRWWWRRRRRRWWWRRRD.....
.....rRRRWWKRRRRRRWWKRRRD.....
.....RRRRWWWRRRRRRWWWRRRD.....
.....RRRRRRKRRRRRRKRRRRRD.....
.....DDDDDDDKKKKKKDDDDDDD.....
......DDDDDDDDDDDDDDDDDD......
.......D..D........D..D.......
......D..D..........D..D......
""".strip()

# Compact favicon: shell + eyes only.
FAVICON_16 = """
................
.....GGG........
....GrRWG.......
...GrRRRWG......
..GrRRRRRWG.....
.GrRRRRRRRWG....
GrRRRRRRRRRWG...
GrRRWWWWRRRWG...
GrRRRRRRRRRWG...
.GrRRRRRRRWG....
..GrRRRRRWG.....
...GrRRRWG......
....GrRWG.......
.....GGG........
................
................
""".strip()


def parse_grid(art: str) -> list[list[str | None]]:
    rows = [line for line in art.splitlines() if line]
    width = max(len(r) for r in rows)
    grid: list[list[str | None]] = []
    for row in rows:
        cells = []
        for ch in row.ljust(width):
            cells.append(PALETTE.get(ch, PALETTE["K"]))
        grid.append(cells)
    return grid


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def write_png(path: Path, grid: list[list[str | None]], scale: int = 1) -> None:
    h = len(grid)
    w = len(grid[0]) if grid else 0
    raw = bytearray()
    for row in grid:
        raw.append(0)  # filter none
        for px in row:
            if px is None:
                raw.extend((0, 0, 0, 0))
            else:
                raw.extend((*px, 255))
    compressed = zlib.compress(bytes(raw), 9)

    ihdr = struct.pack(">IIBBBBB", w * scale, h * scale, 8, 6, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n"
    png += png_chunk(b"IHDR", ihdr)
    png += png_chunk(b"IDAT", compressed)
    png += png_chunk(b"IEND", b"")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png)


def upscale(grid: list[list], factor: int) -> list[list]:
    out: list[list] = []
    for row in grid:
        for _ in range(factor):
            line = []
            for px in row:
                line.extend([px] * factor)
            out.append(line)
    return out


def blank_grid(width: int, height: int) -> list[list[str | None]]:
    return [[None] * width for _ in range(height)]


def blit(
    dest: list[list[str | None]], src: list[list[str | None]], x: int, y: int
) -> None:
    for row_i, row in enumerate(src):
        for col_i, px in enumerate(row):
            if px is not None:
                dest[y + row_i][x + col_i] = px


def _bevel_glyph(
    art: list[str], base: str, hi: str | None, lo: str | None
) -> list[list[str | None]]:
    """Rasterize a glyph with a 2px top-edge highlight and 2px bottom-edge
    shade per column, for a lit-from-above metal look."""
    h = len(art)
    w = max(len(r) for r in art)
    rows = [r.ljust(w, ".") for r in art]
    top: dict[int, int] = {}
    bot: dict[int, int] = {}
    for x in range(w):
        ys = [y for y in range(h) if rows[y][x] == "X"]
        if ys:
            top[x] = min(ys)
            bot[x] = max(ys)
    grid: list[list[str | None]] = []
    for y in range(h):
        line: list[str | None] = []
        for x in range(w):
            if rows[y][x] != "X":
                line.append(None)
                continue
            color = base
            if hi is not None and y <= top[x] + 1:
                color = hi
            elif lo is not None and y >= bot[x] - 1:
                color = lo
            line.append(PALETTE[color])
        grid.append(line)
    return grid


def build_logo_lockup(mark_gap: int = _MASCOT_GAP) -> list[list[str | None]]:
    """Compose rustacean mascot + ReRust wordmark on a 24-row canvas.

    The crab holds a diff-green ``+`` in its left claw and a diff-red ``-`` in
    its right claw. No chevron, migration arrow, or underline — the rewrite
    story is told by the claw symbols alone. The crab's feet land on the same
    row as the letter bottoms so mascot and text share a ground line.
    """
    crab = parse_grid(LOCKUP_CRAB)
    crab_w = len(crab[0])
    crab_h = len(crab)

    letters = [_bevel_glyph(art, base, hi, lo) for art, base, hi, lo in _WORD]
    text_w = sum(len(g[0]) for g in letters) + _KERN * (len(letters) - 1)

    width = crab_w + mark_gap + text_w
    canvas = blank_grid(width, _LOCKUP_H)

    blit(canvas, crab, 0, _BASELINE - crab_h)

    cursor = crab_w + mark_gap
    for glyph in letters:
        blit(canvas, glyph, cursor, _BASELINE - len(glyph))
        cursor += len(glyph[0]) + _KERN

    return canvas


def main() -> None:
    logo = parse_grid(LOGO_32)
    rewrite = parse_grid(ICON_REWRITE_16)
    replacement = parse_grid(ICON_REPLACEMENT_16)
    favicon = parse_grid(FAVICON_16)

    lockup = build_logo_lockup()

    write_png(OUT / "logo-64.png", upscale(logo, 2))
    # 6x native scale (144px tall): crisp at 72px desktop / 48px mobile.
    write_png(OUT / "logo-lockup-144.png", upscale(lockup, 6))
    write_png(OUT / "favicon-32.png", upscale(favicon, 2))
    write_png(OUT / "favicon-16.png", favicon)
    write_png(OUT / "icon-rewrite-32.png", upscale(rewrite, 2))
    write_png(OUT / "icon-replacement-32.png", upscale(replacement, 2))

    print(f"Wrote sprites to {OUT}/")
    for p in sorted(OUT.glob("*.png")):
        print(f"  {p.name} ({p.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
