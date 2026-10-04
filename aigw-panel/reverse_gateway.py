# -*- coding: utf-8 -*-
"""
逆向 workbuddy-gateway（Linux ELF, Go 1.24.5）
==============================================
目标：搞清楚「哪些能力能在面板内重写，哪些必须靠这个二进制」，
为「是否还要依赖它」这个决策提供依据。

Linux 版比 Windows 版更适合逆向：同一份 Go 源码编译，
字符串池里保留了 main 包的函数名（不 strip）。

用法：python reverse_gateway.py [二进制路径]
"""
import os
import re
import struct
import sys
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CANDIDATES = [
    os.path.join(ROOT, "workbuddy-gateway-x86-1.29.6", "workbuddy-gateway"),
    os.path.join(ROOT, "workbuddy-gateway-windows-1.29.6.exe"),
]


def strings_of(d, minlen=4):
    return [m.group().decode("utf-8", "replace")
            for m in re.finditer(rb"[\x20-\x7e]{%d,}" % minlen, d)]


def go_strings(d):
    """
    Go 字符串表：定长头 + 内容，紧跟在 rodata 里。
    不用它的话只能靠正则，会漏掉非 ASCII 串（中文提示等）。
    """
    out = []
    # 抓 ASCII 可读的
    out += strings_of(d, 5)
    # 抓 UTF-8 中文（Go 的中文提示语）
    for m in re.finditer(rb"(?:[\xe4-\xe9][\x80-\xbf]{2}){2,}", d):
        try:
            s = m.group().decode("utf-8")
            if len(s) >= 2:
                out.append(s)
        except Exception:
            pass
    return out


def elf_sections(d):
    """解析 ELF64 section headers，拿到 .rodata / .text 的精确范围"""
    if d[:4] != b"\x7fELF" or d[4] != 2:
        return []
    e_shoff = struct.unpack_from("<Q", d, 0x28)[0]
    e_shentsize = struct.unpack_from("<H", d, 0x3A)[0]
    e_shnum = struct.unpack_from("<H", d, 0x3C)[0]
    e_shstrndx = struct.unpack_from("<H", d, 0x3E)[0]
    secs = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        (name, typ, flags, addr, offset, size, link, info, align,
         entsize) = struct.unpack_from("<IIQQQQIIQQ", d, off)
        secs.append({"name_off": name, "type": typ, "addr": addr,
                     "off": offset, "size": size})
    if e_shstrndx < len(secs):
        base = secs[e_shstrndx]["off"]
        for s in secs:
            end = d.index(b"\0", base + s["name_off"])
            s["name"] = d[base + s["name_off"]:end].decode("ascii", "replace")
    return secs


def find_api_paths(ss):
    """API 路径 —— 决定「哪些功能存在」的关键"""
    pats = [
        r"^/(?:v\d|admin|api|health|metrics|debug)[/\w\-.]*$",
        r"^/[a-z][\w\-/]{2,60}$",
    ]
    out = set()
    for s in ss:
        for p in pats:
            if re.match(p, s) and " " not in s and "\\" not in s:
                if len(s) > 3 and not s.startswith("/usr/") \
                        and not s.startswith("/proc/") and not s.startswith("/etc/"):
                    out.add(s)
    return out


def find_upstreams(ss):
    """上游域名与基址"""
    out = set()
    for s in ss:
        for m in re.finditer(r"https?://[\w.\-]+(?::\d+)?(/[\w\-./]*)?", s):
            u = m.group(0)
            if len(u) < 120:
                out.add(u)
    return out


def find_main_funcs(ss):
    """main 包的函数名 —— Go 没 strip 时这里就是「它实现了什么」"""
    out = set()
    for s in ss:
        if s.startswith("main.") and len(s) < 90:
            out.add(s)
        elif s.startswith("github.com/") and "/internal/" not in s \
                and len(s) < 90 and s.count("/") <= 4:
            out.add(s)
    return out


def find_config(ss):
    """配置项 —— 反推它需要什么、怎么存"""
    keys = set()
    for s in ss:
        if re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{2,28}", s):
            if any(k in s.lower() for k in (
                    "key", "token", "secret", "url", "host", "port", "path",
                    "dir", "file", "enable", "disable", "timeout", "retry",
                    "interval", "proxy", "api", "auth", "user", "pass",
                    "cookie", "model", "quota", "credit", "checkin", "login",
                    "usage", "webhook", "notify", "schedule", "cron", "seed",
                    "project", "region", "edition")):
                keys.add(s)
    return keys


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else None
    if not path:
        for c in CANDIDATES:
            if os.path.exists(c):
                path = c
                break
    if not path:
        print("找不到二进制，候选：%s" % CANDIDATES)
        return 2
    d = open(path, "rb").read()
    print("=" * 74)
    print("目标: %s" % path)
    print("大小: %.1f MB   格式: %s" % (
        len(d) / 1048576, "ELF64" if d[:4] == b"\x7fELF" else "PE"))
    m = re.search(rb"go1\.\d+(\.\d+)?", d)
    print("Go: %s" % (m.group().decode() if m else "?"))

    secs = elf_sections(d)
    if secs:
        print("节区: %s" % ", ".join(
            "%s(%dKB)" % (s["name"], s["size"] // 1024)
            for s in secs if s.get("name") and s["size"] > 4096))

    ss = go_strings(d)
    print("可读串: %d（含中文 %d）" % (
        len(ss), sum(1 for x in ss if any('\u4e00' <= ch <= '\u9fff' for ch in x))))

    # 1) HTTP 路由
    paths = sorted(find_api_paths(ss))
    print("\n" + "=" * 74)
    print("【1】HTTP 路由（%d 条）" % len(paths))
    groups = defaultdict(list)
    for p in paths:
        key = p.split("/")[1] if "/" in p[1:] else p
        groups[key].append(p)
    for k in sorted(groups):
        print("  %-22s %s" % (k, " ".join(sorted(groups[k])[:8])))

    # 2) 上游
    ups = sorted(find_upstreams(ss))
    print("\n" + "=" * 74)
    print("【2】上游端点（%d 条，去重后的域名级）" % len(ups))
    doms = sorted(set(re.sub(r"https?://([^/]+).*", r"\1", u) for u in ups))
    for x in doms[:40]:
        print("  " + x)

    # 3) main 包函数 —— 它实现了什么
    funcs = sorted(find_main_funcs(ss))
    print("\n" + "=" * 74)
    print("【3】main 包 / 内部包函数（%d 条，只列有信息量的）" % len(funcs))
    keep = [f for f in funcs if any(
        k in f.lower() for k in (
            "login", "auth", "checkin", "sign", "model", "usage", "credit",
            "quota", "token", "session", "account", "schedule", "cron",
            "proxy", "upstream", "route", "task", "growth", "notify", "webhook",
            "region", "edition", "project"))]
    for f in keep[:70]:
        print("  " + f)
    if len(keep) > 70:
        print("  … 还有 %d 条" % (len(keep) - 70))

    # 4) 配置键
    keys = sorted(find_config(ss))
    print("\n" + "=" * 74)
    print("【4】疑似配置/字段键（%d 条，前 60）" % len(keys))
    for k in keys[:60]:
        print("  " + k)

    # 5) 中文提示 —— 直接告诉你它会返回什么错误
    cn = sorted(set(x for x in ss
                    if len(x) >= 4 and sum('\u4e00' <= ch <= '\u9fff' for ch in x) > 3
                    and len(x) < 90))
    print("\n" + "=" * 74)
    print("【5】中文提示/错误文案（%d 条，前 40）—— 能反推它管什么" % len(cn))
    for c in cn[:40]:
        print("  " + c)

    out = os.path.join(HERE, "data", "gateway_reverse.txt")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write("目标: %s\n\n" % path)
        f.write("【HTTP 路由】\n" + "\n".join(paths) + "\n\n")
        f.write("【上游】\n" + "\n".join(ups) + "\n\n")
        f.write("【main 函数】\n" + "\n".join(funcs) + "\n\n")
        f.write("【配置键】\n" + "\n".join(keys) + "\n\n")
        f.write("【中文文案】\n" + "\n".join(cn) + "\n")
    print("\n完整结果: %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
