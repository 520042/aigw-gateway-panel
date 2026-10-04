# -*- coding: utf-8 -*-
"""
深挖 base.apk（aigw.app）里 Loomy / Antigravity 两个供应商：
域名、接口路径、登录方式、User-Agent、鉴权头。
base.apk 顶部字符串明写「Trae / Loomy / WorkBuddy / Antigravity」，
说明它本身就是多供应商反代，之前只做了 Trae。
"""
import io
import os
import re
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
APK = os.path.join(ROOT, "base.apk")
OUT = os.path.join(HERE, "data", "vendor_probe.txt")

KEYS = [b"loomy", b"Loomy", b"LOOMY",
        b"antigravity", b"Antigravity", b"ANTIGRAVITY"]


def dex_blob():
    blob = b""
    with zipfile.ZipFile(APK) as z:
        for n in sorted(z.namelist()):
            if n.endswith(".dex"):
                blob += z.read(n)
    return blob


def strings(blob):
    return [(m.start(), m.group().decode("ascii", "replace"))
            for m in re.finditer(rb"[\x20-\x7e]{3,}", blob)]


def main():
    blob = dex_blob()
    ss = strings(blob)
    lines = []
    lines.append("base.apk dex = %d B, 串 = %d" % (len(blob), len(ss)))
    lines.append("")

    # 1) 所有含 loomy/antigravity 的串
    lines.append("=" * 78)
    lines.append("1) 含 loomy / antigravity 的字符串（去重）")
    lines.append("=" * 78)
    seen = set()
    for off, s in ss:
        if any(k.decode() in s for k in KEYS) and s not in seen:
            seen.add(s)
            lines.append("  0x%08X  %s" % (off, s[:200]))

    # 2) 域名
    lines.append("")
    lines.append("=" * 78)
    lines.append("2) 全部域名")
    lines.append("=" * 78)
    hosts = sorted(set(m.group().decode("ascii", "replace")
                       for m in re.finditer(
                           rb"https?://[a-zA-Z0-9._\-]{4,70}", blob)))
    for h in hosts:
        mark = "  <<< Loomy" if "loomy" in h.lower() else (
            "  <<< Antigravity" if "antigravity" in h.lower() else "")
        lines.append("  " + h + mark)

    # 3) 含 loomy/antigravity 的路径
    lines.append("")
    lines.append("=" * 78)
    lines.append("3) 相关 API 路径（含上下文）")
    lines.append("=" * 78)
    for m in re.finditer(rb"(?:loomy|antigravity)[a-zA-Z0-9_/\-\.]*", blob):
        lo = max(0, m.start() - 120)
        seg = blob[lo:m.start() + 160]
        near = [x.group().decode("ascii", "replace")
                for x in re.finditer(rb"[\x20-\x7e]{4,90}", seg)]
        near = [x for x in near if x.startswith("/") or "loomy" in x.lower()
                or "antigravity" in x.lower()]
        if near:
            lines.append("  @0x%X  %s" % (
                m.start(), m.group().decode("ascii", "replace")[:70]))
            for x in near[:8]:
                lines.append("        " + x[:120])

    # 4) UA / header 特征
    lines.append("")
    lines.append("=" * 78)
    lines.append("4) User-Agent / Header 特征")
    lines.append("=" * 78)
    for w in [b"loomy-version", b"antigravity/", b"ideType", b"pluginType",
              b"require_usage", b"x-loomy", b"x-antigravity", b"Loomy-"]:
        for m in list(re.finditer(re.escape(w), blob))[:4]:
            seg = blob[max(0, m.start() - 140):m.start() + 180]
            near = [x.group().decode("ascii", "replace")
                    for x in re.finditer(rb"[\x20-\x7e]{4,110}", seg)]
            lines.append("  %r @0x%X" % (w.decode(), m.start()))
            for x in near[:10]:
                lines.append("        " + x[:130])

    # 5) UI 文案（说明支持哪些功能）
    lines.append("")
    lines.append("=" * 78)
    lines.append("5) UI 文案里的供应商/功能线索")
    lines.append("=" * 78)
    for m in re.finditer(rb"(?:[\x20-\x7e]\x00){4,}", blob):
        try:
            u = m.group().decode("utf-16-le")
        except Exception:
            continue
        if any(k.decode() in u for k in KEYS) and 3 < len(u) < 120:
            lines.append("  " + u)

    txt = "\n".join(lines)
    with io.open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write(txt)
    print("已写入 %s（%d 行）" % (OUT, len(lines)))


if __name__ == "__main__":
    main()
