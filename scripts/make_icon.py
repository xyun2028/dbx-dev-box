#!/usr/bin/env python3
"""生成插件图标（PNG + SVG）。

为什么用 PNG 兜底：宿主把 icon 当 <img> 渲染，SVG 一旦被它的清洗器动过
（比如只剩描边、或者缺命名空间），侧栏就是一个破图。PNG 不会。

图形：圆角方块 + 终端提示符（> _），在 16px 下依然能认出来。
不画细描边——小尺寸上描边会糊成一团，实心块才立得住。
"""

from __future__ import annotations

import math
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SIZE = 256
SS = 4  # 超采样倍数，最后缩回去做抗锯齿

C_TOP = (79, 124, 255)      # #4f7cff
C_BOTTOM = (47, 86, 200)    # #2f56c8
INK = (255, 255, 255)


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def rounded_rect_sdf(x: float, y: float, w: float, h: float, r: float) -> float:
    """返回点到圆角矩形的有符号距离（内部为负）。"""
    dx = abs(x - w / 2) - (w / 2 - r)
    dy = abs(y - h / 2) - (h / 2 - r)
    ax, ay = max(dx, 0.0), max(dy, 0.0)
    return math.hypot(ax, ay) + min(max(dx, dy), 0.0) - r


def segment_sdf(px: float, py: float, ax: float, ay: float, bx: float, by: float) -> float:
    vx, vy = bx - ax, by - ay
    wx, wy = px - ax, py - ay
    denom = vx * vx + vy * vy
    t = 0.0 if denom == 0 else max(0.0, min(1.0, (wx * vx + wy * vy) / denom))
    cx, cy = ax + vx * t, ay + vy * t
    return math.hypot(px - cx, py - cy)


def render() -> bytes:
    w = SIZE * SS
    # 画布坐标（0..1）→ 直接按像素算
    pad = 0.06 * w
    radius = 0.22 * w
    stroke = 0.075 * w

    # 字形几何（按画布比例）
    c1 = (0.34 * w, 0.36 * w)
    c2 = (0.50 * w, 0.50 * w)
    c3 = (0.34 * w, 0.64 * w)
    u1 = (0.56 * w, 0.64 * w)
    u2 = (0.70 * w, 0.64 * w)

    rows = []
    for y in range(w):
        row = bytearray()
        for x in range(w):
            # 背景圆角方块 + 垂直渐变
            d = rounded_rect_sdf(x + 0.5 - pad, y + 0.5 - pad, w - 2 * pad, w - 2 * pad, radius)
            bg_alpha = max(0.0, min(1.0, 0.5 - d))
            t = y / (w - 1)
            bg = (
                lerp(C_TOP[0], C_BOTTOM[0], t),
                lerp(C_TOP[1], C_BOTTOM[1], t),
                lerp(C_TOP[2], C_BOTTOM[2], t),
            )

            # 前景字形
            gd = min(
                segment_sdf(x, y, *c1, *c2),
                segment_sdf(x, y, *c2, *c3),
                segment_sdf(x, y, *u1, *u2),
            )
            ink_alpha = max(0.0, min(1.0, stroke / 2 - gd + 0.5)) * bg_alpha

            r = lerp(bg[0], INK[0], ink_alpha)
            g = lerp(bg[1], INK[1], ink_alpha)
            b = lerp(bg[2], INK[2], ink_alpha)
            a = bg_alpha * 255
            row += bytes((int(r), int(g), int(b), int(a)))
        rows.append(row)

    # 超采样缩回目标尺寸（盒式平均）
    out = bytearray()
    for y in range(SIZE):
        out.append(0)  # PNG 每行前的 filter 字节
        for x in range(SIZE):
            r = g = b = a = 0
            for sy in range(SS):
                row = rows[y * SS + sy]
                for sx in range(SS):
                    i = (x * SS + sx) * 4
                    r += row[i]
                    g += row[i + 1]
                    b += row[i + 2]
                    a += row[i + 3]
            n = SS * SS
            out += bytes((r // n, g // n, b // n, a // n))
    return bytes(out)


def write_png(path: Path, raw: bytes) -> None:
    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    body = zlib.compress(raw, 9)
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", body) + chunk(b"IEND", b"")
    )


SVG = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" width="128" height="128">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#4f7cff"/>
      <stop offset="1" stop-color="#2f56c8"/>
    </linearGradient>
  </defs>
  <rect x="4" y="4" width="120" height="120" rx="28" fill="url(#bg)"/>
  <path d="M42 44 60 64 42 84" fill="none" stroke="#fff" stroke-width="11"
        stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M70 84h20" fill="none" stroke="#fff" stroke-width="11" stroke-linecap="round"/>
</svg>
"""


def main() -> int:
    assets = ROOT / "assets"
    assets.mkdir(exist_ok=True)

    png = assets / "icon.png"
    write_png(png, render())
    (assets / "icon.svg").write_text(SVG, encoding="utf-8")
    # 旧文件名保留一份，避免别处引用断掉
    (assets / "plugin.svg").write_text(SVG, encoding="utf-8")

    print(f"✓ {png.relative_to(ROOT)}  {png.stat().st_size / 1024:.1f} KB")
    print(f"✓ {(assets / 'icon.svg').relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
