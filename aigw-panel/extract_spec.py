# -*- coding: utf-8 -*-
"""
提取 workbuddy-gateway 的完整能力规格
======================================
目标不是「看它有什么字符串」，而是拿到**可复刻的行为规格**：
  - 全部 HTTP 端点（含参数）
  - 请求头
  - 错误码 / 状态机
  - 内部调用的上游接口

手段：解析 Go 的 gostring 表（有 .gopclntab 就行，不依赖符号表）。
Go 把所有字符串常量按「长度 + 内容」连续存放，能拿到**有序无粘连**的完整列表，
比正则扫准得多。

用法：python extract_spec.py [二进制路径]
"""
import os
import re
import struct
import sys
import zlib
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CANDS = [
    os.path.join(ROOT, "workbuddy-gateway-x86-1.29.6", "workbuddy-gateway"),
    os.path.join(ROOT, "workbuddy-gateway-windows-1.29.6.exe"),
]


def sections(d):
    if d[:4] != b"\x7fELF" or d[4] != 2:
        return {}
    shoff = struct.unpack_from("<Q", d, 0x28)[0]
    shent = struct.unpack_from("<H", d, 0x3A)[0]
    shnum = struct.unpack_from("<H", d, 0x3C)[0]
    shstr = struct.unpack_from("<H", d, 0x3E)[0]
    out = []
    for i in range(shnum):
        o = shoff + i * shent
        (nm, ty, fl, ad, off, sz, lk, inf, al, es) = struct.unpack_from(
            "<IIQQQQIIQQ", d, o)
        out.append({"name_off": nm, "off": off, "size": sz, "addr": ad})
    base = out[shstr]["off"] if shstr < len(out) else 0
    for s in out:
        try:
            e = d.index(b"\0", base + s["name_off"])
            s["name"] = d[base + s["name_off"]:e].decode("ascii", "replace")
        except Exception:
            s["name"] = ""
    return {s["name"]: s for s in out if s.get("name")}


def gostrings(d, secs):
    """
    Go 1.20+ 的字符串在 .rodata 里是连续的 [varint len][bytes] 序列。
    没有符号表就从一个高概率起点开始扫 —— 字符串区里可读字节密度最高。
    """
    rod = secs.get(".rodata")
    if not rod:
        return []
    blob = d[rod["off"]:rod["off"] + rod["size"]]
    out = []
    i = 0
    n = len(blob)
    while i < n:
        # varint 长度
        ln = 0
        shift = 0
        start = i
        while i < n:
            b = blob[i]
            ln |= (b & 0x7F) << shift
            i += 1
            if not (b & 0x80):
                break
            shift += 7
            if shift > 35:
                i = start
                break
        else:
            break
        if shift > 35:
            i = start + 1
            continue
        if ln <= 0 or ln > 4000 or i + ln > n:
            i = start + 1
            continue
        raw = blob[i:i + ln]
        i += ln
        try:
            s = raw.decode("utf-8")
        except UnicodeDecodeError:
            continue
        if not s:
            continue
        # 只保留「像人写的字符串」
        if sum(1 for c in s if c.isprintable()) / len(s) < 0.9:
            continue
        out.append(s)
    return out


def classify(ss):
    """把字符串按用途分桶"""
    buckets = defaultdict(list)
    for s in ss:
        if re.fullmatch(r"/[a-zA-Z0-9_./\-]{2,70}", s) and s.count("/") >= 1:
            if not s.startswith(("/usr/", "/proc/", "/etc/", "/sys/", "/dev/")):
                buckets["path"].append(s)
        elif re.fullmatch(r"[a-z][a-z0-9\-]{3,30}", s) and "-" in s:
            buckets["header"].append(s)
        elif re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]{2,40}", s) and "_" in s:
            buckets["field"].append(s)
        elif re.fullmatch(r"[A-Z][A-Z0-9_]{2,40}", s):
            buckets["const"].append(s)
        elif " " in s and len(s) < 70:
            buckets["msg"].append(s)
    return buckets


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else None
    if not path:
        for c in CANDS:
            if os.path.exists(c):
                path = c
                break
    if not path:
        print("找不到二进制")
        return 2
    d = open(path, "rb").read()
    print("=" * 76)
    print("目标 %s（%.1f MB）" % (os.path.basename(path), len(d) / 1048576))
    secs = sections(d)
    print("节区: %s" % ", ".join(
        "%s(%dKB)" % (k, v["size"] // 1024) for k, v in secs.items()
        if v["size"] > 4096))

    ss = gostrings(d, secs)
    ss += [m.group().decode("utf-8", "replace")
           for m in re.finditer(rb"[\x20-\x7e]{4,}", d)]
    # 去重保序
    seen = set()
    uniq = []
    for s in ss:
        if s not in seen:
            seen.add(s)
            uniq.append(s)
    print("有序字符串: %d" % len(uniq))

    b = classify(uniq)

    print("\n" + "=" * 76)
    print("【A】HTTP 端点与路径（%d）" % len(b["path"]))
    for p in sorted(set(b["path"])):
        print("  " + p)

    print("\n" + "=" * 76)
    print("【B】HTTP 请求头（%d）" % len(b["header"]))
    for h in sorted(set(b["header"])):
        if any(k in h.lower() for k in ("x-", "content", "auth", "cookie",
                                       "user-agent", "referer", "origin")):
            print("  " + h)

    print("\n" + "=" * 76)
    print("【C】关键字段（下划线命名，%d）" % len(b["field"]))
    kw = ("token", "key", "cookie", "sign", "uskey", "skey", "fp",
          "device", "session", "csrf", "quota", "credit", "model",
          "account", "login", "checkin", "growth", "region", "edition",
          "project", "rate", "usage", "limit")
    for f in sorted(set(b["field"])):
        if any(k in f.lower() for k in kw):
            print("  " + f)

    print("\n" + "=" * 76)
    print("【D】消息与提示（能看出状态机，%d）" % len(b["msg"]))
    pat = ("未登录", "登录", "过期", "失效", "限流", "冷却", "配额",
           "额度", "倍率", "签到", "成长", "账号", "凭据", "授权",
           "冷却结束", "不可用", "失败")
    for m in sorted(set(b["msg"])):
        if any(k in m for k in pat) and len(m) < 60:
            print("  " + m)

    out = os.path.join(HERE, "data", "gateway_spec.txt")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write("目标: %s\n\n" % path)
        for tag, key in (("端点", "path"), ("请求头", "header"),
                         ("字段", "field"), ("消息", "msg")):
            f.write("【%s】\n%s\n\n" % (tag, "\n".join(sorted(set(b[key])))))
    print("\n完整规格: %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
