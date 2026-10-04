# -*- coding: utf-8 -*-
"""
全量扫 APK：抽出所有 API 路径，与面板已实现的 gwextra.ACTIONS 对比，
列出「APP 里有但面板没集成」的接口。
"""
import os
import re
import sys
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
ROOT = os.path.dirname(HERE)

# APK → 平台键（与 gwlogin.PLATFORMS / gwextra.ACTIONS 对齐）
APK_MAP = {
    "base.apk": "apk-trae",
    "base(1).apk": "apk-doubao",
    "base(2).apk": "apk-go",
    "base(3).apk": "apk-codebuddy",
    "base(4).apk": "apk-raccoon",
    "base(5).apk": "apk-yuanbao",
}

# 噪声：Android/Kotlin/Java 内建路径，不是业务接口
NOISE = re.compile(
    r"^/(?:android|androidx|com|kotlin|java|javax|sun|org|dalvik|okhttp|"
    r"retrofit|kotlinx|io|net|text|util|lang|collections|ranges|jvm|"
    r"annotation|reflect|concurrent|security|net|nio|charset)", re.I)
# Kotlin 内部符号
NOISE2 = re.compile(r"[<>$\\]|^\d|^(?:Metadata|Function\d|Pair|Triple|Unit|Exception)")

PATH_RE = re.compile(rb"/[a-zA-Z][a-zA-Z0-9_.\-]*(?:/[a-zA-Z0-9_{}.\-]+){0,6}")


def dex_blob(path):
    blob = b""
    try:
        with zipfile.ZipFile(path) as z:
            for n in sorted(z.namelist()):
                if n.endswith(".dex"):
                    blob += z.read(n)
    except Exception as e:
        print("  读取失败 %s: %s" % (path, e))
    return blob


def extract_paths(blob):
    """抽出像业务接口的路径"""
    out = set()
    for m in PATH_RE.finditer(blob):
        s = m.group().decode("ascii", "replace")
        if len(s) < 4 or len(s) > 90:
            continue
        if NOISE.match(s) or NOISE2.search(s):
            continue
        # 必须看起来像接口：含 api / v1 / v2 / v3 / 业务词
        low = s.lower()
        if not any(k in low for k in ("/api", "/v1", "/v2", "/v3", "/v4",
                                      "model", "point", "credit", "checkin",
                                      "sign", "user", "login", "task", "chat",
                                      "usage", "entitle", "bill", "balance",
                                      "refresh", "redeem", "quota", "conversation",
                                      "setting", "config", "info")):
            continue
        # 排除明显的文件/资源路径
        if low.endswith((".png", ".jpg", ".svg", ".json", ".js", ".css",
                         ".html", ".xml", ".so", ".dex", ".prof", ".txt")):
            continue
        out.add(s)
    return out


def main():
    from app import gwextra

    done = {}       # platform → set(已实现 path)
    for p, acts in gwextra.ACTIONS.items():
        done[p] = set(a["path"] for a in acts)

    report = {}
    for apk, plat in APK_MAP.items():
        p = os.path.join(ROOT, apk)
        if not os.path.exists(p):
            continue
        blob = dex_blob(p)
        paths = extract_paths(blob)
        have = done.get(plat, set())
        # 归一化比较：去尾斜杠
        def norm(s):
            return s.rstrip("/").lower()
        have_n = set(norm(x) for x in have)
        missing = sorted(x for x in paths if norm(x) not in have_n)
        report[plat] = (apk, sorted(paths), sorted(have), missing)

    print("=" * 78)
    print("全量对比：APK 里的接口  vs  面板已实现（gwextra.ACTIONS）")
    print("=" * 78)
    total_missing = 0
    for plat in sorted(report):
        apk, paths, have, missing = report[plat]
        print("\n### %s  （%s）" % (plat, apk))
        print("  APK 内接口 %d 个，面板已实现 %d 个" % (len(paths), len(have)))
        if missing:
            total_missing += len(missing)
            print("  ---- 未集成 %d 个 ----" % len(missing))
            for s in missing:
                print("     " + s)
        else:
            print("  （无缺失）")
    print("\n" + "=" * 78)
    print("合计未集成：%d 个接口" % total_missing)


if __name__ == "__main__":
    main()
