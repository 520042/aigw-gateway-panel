# -*- coding: utf-8 -*-
"""
生成应用图标
============
不依赖 Pillow，直接构造 PNG → 自拼多尺寸 ICO（Vista+ 支持 PNG 压缩）。

图形：蓝色圆角方块 + 白色圆环 + 中央绿色节点 + 三个外圈刻度（表示多上游）。
"""

import math
import os
import struct
import zlib


# ---------------------------------------------------------------- PNG
def _png(w, h, pixels):
    raw = b""
    for row in pixels:
        raw += b"\x00" + b"".join(bytes(p) for p in row)

    def chunk(typ, data):
        c = typ + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))


# ---------------------------------------------------------------- 绘制工具
def _lerp(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(4))


def _blend(px, S, x, y, col, cov):
    """把 col 以 cov 覆盖率混合到 (x,y)"""
    if cov <= 0.002:
        return
    if cov >= 0.998:
        px[y][x] = col
        return
    d = px[y][x]
    a = (col[3] / 255.0) * cov
    da = d[3] / 255.0
    na = a + da * (1 - a)
    if na <= 0.003:
        px[y][x] = (0, 0, 0, 0)
        return
    out = []
    for i in range(3):
        v = (col[i] * a + d[i] * da * (1 - a)) / na
        out.append(int(max(0, min(255, v))))
    px[y][x] = tuple(out) + (int(na * 255),)


def _cov(dist, edge, aa):
    if dist <= edge - aa:
        return 1.0
    if dist >= edge + aa:
        return 0.0
    return (edge + aa - dist) / (2 * aa)


def _rounded_rect(px, S, pad, radius, top, bot):
    """圆角矩形渐变填充"""
    aa = max(0.55, S / 400.0)
    x0 = y0 = pad
    x1 = y1 = S - pad
    W = x1 - x0
    H = y1 - y0
    R = radius
    for y in range(max(0, int(y0 - 2)), min(S, int(y1 + 3))):
        t = (y - y0) / max(1.0, H)
        base = _lerp(top, bot, t)
        for x in range(max(0, int(x0 - 2)), min(S, int(x1 + 3))):
            # 到圆角矩形边界的有符号距离
            qx = abs(x + 0.5 - (x0 + x1) / 2.0) - (W / 2.0 - R)
            qy = abs(y + 0.5 - (y0 + y1) / 2.0) - (H / 2.0 - R)
            outside = math.hypot(max(qx, 0.0), max(qy, 0.0))
            inside = min(max(qx, qy), 0.0)
            dist = outside + inside - R     # <0 在内部
            c = _cov(dist, 0.0, aa)
            if c > 0.002:
                px[y][x] = (base[0], base[1], base[2], int(255 * c))


def _disc(px, S, ccx, ccy, rad, col):
    aa = max(0.55, S / 400.0)
    y0 = int(ccy - rad - 2)
    y1 = int(ccy + rad + 2)
    x0 = int(ccx - rad - 2)
    x1 = int(ccx + rad + 2)
    for y in range(max(0, y0), min(S, y1 + 1)):
        for x in range(max(0, x0), min(S, x1 + 1)):
            d = math.hypot(x + 0.5 - ccx, y + 0.5 - ccy)
            c = _cov(d, rad, aa)
            if c > 0.002:
                _blend(px, S, x, y, col, c)


def _ring(px, S, ccx, ccy, rad, width, col):
    aa = max(0.55, S / 400.0)
    half = width / 2.0
    y0 = int(ccy - rad - width - 2)
    y1 = int(ccy + rad + width + 3)
    x0 = int(ccx - rad - width - 2)
    x1 = int(ccx + rad + width + 3)
    for y in range(max(0, y0), min(S, y1 + 1)):
        for x in range(max(0, x0), min(S, x1 + 1)):
            d = math.hypot(x + 0.5 - ccx, y + 0.5 - ccy)
            c = _cov(abs(d - rad), half, aa)
            if c > 0.002:
                _blend(px, S, x, y, col, c)


def _arc(px, S, ccx, ccy, rad, width, a0, a1, col):
    """角度制，逆时针从 a0 到 a1"""
    aa = max(0.55, S / 400.0)
    half = width / 2.0
    y0 = int(ccy - rad - width - 2)
    y1 = int(ccy + rad + width + 3)
    x0 = int(ccx - rad - width - 2)
    x1 = int(ccx + rad + width + 3)
    for y in range(max(0, y0), min(S, y1 + 1)):
        for x in range(max(0, x0), min(S, x1 + 1)):
            dx = x + 0.5 - ccx
            dy = y + 0.5 - ccy
            d = math.hypot(dx, dy)
            if d < 1e-6:
                continue
            ang = math.degrees(math.atan2(-dy, dx)) % 360   # 屏幕 y 向下，取反得逆时针
            span = (a1 - a0) % 360
            rel = (ang - a0) % 360
            if rel > span:
                continue
            c = _cov(abs(d - rad), half, aa)
            if c > 0.002:
                _blend(px, S, x, y, col, c)


# ---------------------------------------------------------------- 图标
def render(S=256):
    px = [[(0, 0, 0, 0) for _ in range(S)] for _ in range(S)]

    bg_top = (58, 122, 245, 255)
    bg_bot = (21, 66, 150, 255)
    _rounded_rect(px, S, pad=max(1, int(S * 0.035)),
                  radius=S * 0.235, top=bg_top, bot=bg_bot)

    cx = cy = S / 2.0
    white = (255, 255, 255, 240)
    soft = (255, 255, 255, 110)
    green = (34, 197, 94, 255)

    # 外圈：三个刻度弧（多上游）
    r_out = S * 0.325
    for a0, a1 in ((-24, 24), (96, 144), (216, 264)):
        _arc(px, S, cx, cy, r_out, S * 0.055, a0, a1, soft)

    # 主圆环
    r_mid = S * 0.215
    _ring(px, S, cx, cy, r_mid, S * 0.050, white)

    # 中央节点
    _disc(px, S, cx, cy, S * 0.105, white)
    _disc(px, S, cx, cy, S * 0.062, green)

    return _png(S, S, px)


# ---------------------------------------------------------------- ICO
def build_ico(pngs, out_path):
    n = len(pngs)
    head = struct.pack("<HHH", 0, 1, n)
    offset = 6 + 16 * n
    entries = b""
    datas = b""
    for size, data in pngs:
        w = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
        datas += data
    with open(out_path, "wb") as f:
        f.write(head + entries + datas)
    return out_path


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    png_path = os.path.join(here, "app-icon-256.png")
    ico_path = os.path.join(here, "app", "static", "aigw.ico")

    with open(png_path, "wb") as f:
        f.write(render(256))
    print("PNG:", png_path, os.path.getsize(png_path), "bytes")

    use = [16, 32, 48, 64, 128, 256]
    pngs = [(s, render(s)) for s in use]
    build_ico(pngs, ico_path)
    print("ICO:", ico_path, os.path.getsize(ico_path), "bytes  尺寸:", use)
    return ico_path


if __name__ == "__main__":
    main()
