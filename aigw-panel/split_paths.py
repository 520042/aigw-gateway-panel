# -*- coding: utf-8 -*-
"""
还原 Go 字符串表里的「粘连路径」
===================================
问题：Go 的字符串常量在 .rodata 里连续存放，源码里
    const a = "/admin/api/settings"
    const b = "/admin/api/webhooks"
    const c = "/admin/api/checkins"
编译后变成一坨：/admin/api/settings/admin/api/webhooks/admin/api/checkins

解法：用「已知前缀」贪心切分。前缀来自
  1) 实测过的端点（Windows 版跑出来的）
  2) HTTP 语义前缀（/v1 /v2 /api /admin /activity）
  3) 全部以 / 开头且首段是已知词的边界

用法：python split_paths.py [二进制路径]
"""
import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CANDS = [
    os.path.join(ROOT, "workbuddy-gateway-x86-1.29.6", "workbuddy-gateway"),
    os.path.join(ROOT, "workbuddy-gateway-windows-1.29.6.exe"),
]

# 已知锚点：实测过的完整端点 + 常见段首
ANCHORS = [
    "/admin/api/login/start", "/admin/api/login/poll", "/admin/api/login/stop",
    "/admin/api/login/info", "/admin/api/login/key", "/admin/api/login/import",
    "/admin/api/credentials", "/admin/api/settings", "/admin/api/webhooks",
    "/admin/api/webhooks/test", "/admin/api/checkins", "/admin/api/status",
    "/admin/api/usage", "/admin/api/usage/series", "/admin/api/models",
    "/admin/api/models/probe", "/admin/api/growth/run",
    "/admin/api/growth/runall_accounts_cooldown",
    "/admin/api/growth/adopt", "/admin/api/growth/bonus",
    "/admin/api/growth/report", "/admin/api/growth/travel",
    "/admin/api/growth/redeem", "/admin/api/growth/makeup",
    "/activity/growth/buddy/first", "/activity/growth/heatmap",
    "/activity/growth/makeup-cards/use",
    "/v1/chat/completions", "/v1/messages", "/v1/responses",
    "/v1/models", "/v2/plugin/auth/token", "/v2/plugin/auth/state",
    "/v2/plugin/auth/token/refresh", "/v2/plugin/login/account",
    "/v2/billing/meter/daily-checkin", "/v2/billing/meter/get-user-resource",
    "/billing/meter/get-user-resource-summary",
    "/console/enterprises/personal/models", "/v3/config",
    "/login/setup", "/ready", "/data", "/favicon.ico", "/metrics", "/healthz",
    "/response", "/message", "/task",
]


def split_path(s):
    """用锚点把粘连串切开"""
    parts = []
    rest = s
    guard = 0
    while rest and guard < 40:
        guard += 1
        # 找最长的、能匹配开头的锚点
        hit = None
        for a in ANCHORS:
            if rest.startswith(a):
                if hit is None or len(a) > len(hit):
                    hit = a
        if hit:
            parts.append(hit)
            rest = rest[len(hit):]
            continue
        # 没有锚点：切到下一个 '/' 开始的已知段
        m = None
        for a in ANCHORS:
            k = a.find("/", 1)                    # 从第 2 个 '/' 找
            if k > 0:
                idx = rest.find(a[:k])
                if idx > 0 and (m is None or idx < m):
                    m = idx
        if m is None or m == 0:
            break
        parts.append(rest[:m])
        rest = rest[m:]
    if rest:
        parts.append(rest)
    return [x for x in parts if x.startswith("/") and len(x) > 1]


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else None
    if not path:
        for c in CANDS:
            if os.path.exists(c):
                path = c
                break
    d = open(path, "rb").read()
    strs = set(m.group().decode("utf-8", "replace")
                for m in re.finditer(rb"[\x20-\x7e]{4,}", d))

    found = set()
    for s in strs:
        if s.count("/") < 1 or len(s) > 260:
            continue
        if not s.startswith("/"):
            continue
        if any(x in s for x in ("\\", " ", "<", ">", "|", "://")):
            continue
        if re.search(r"//[a-zA-Z]", s):          # 明显是噪声
            continue
        for p in split_path(s):
            if re.fullmatch(r"/[a-zA-Z0-9_./\-]{2,70}", p):
                found.add(p)

    # 归一化：去掉尾部粘连的垃圾段
    print("=" * 74)
    print("还原后的端点清单（%d 条）" % len(found))
    print("=" * 74)
    groups = {}
    for p in sorted(found):
        key = "/".join(p.split("/")[:3]) or "/"
        groups.setdefault(key, []).append(p)
    for k in sorted(groups):
        print("\n[%s]" % k)
        for p in sorted(set(groups[k])):
            print("   " + p)

    out = os.path.join(HERE, "data", "gateway_endpoints.txt")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(sorted(found)))
    print("\n完整清单: %s" % out)


if __name__ == "__main__":
    main()
