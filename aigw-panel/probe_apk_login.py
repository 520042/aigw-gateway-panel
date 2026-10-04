# -*- coding: utf-8 -*-
"""
6 个 APK 的登录 / 凭据获取机制 · 系统分析
==========================================
之前只零散扫过接口，这次按「登录怎么拿到」这条主线逐个挖透。

要回答的问题（每个 APP 都要答）：
  1. 它用什么方式拿凭据？   Cookie / Bearer / 本地服务 / token 交换 / 设备ID
  2. 需要哪些字段？        哪些是必须的
  3. 登录态存在哪？        SharedPreferences / localStorage / sqlite / 内存
  4. 怎么校验登录有效？    拿哪个接口验证
  5. 端点是 APP 自己的还是厂商的？

用法：python probe_apk_login.py [apk名 ...]
"""
import io
import os
import re
import sys
import zipfile
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

APKS = {
    "base.apk":   "Trae / aigw.app（字节）",
    "base(1).apk": "豆包 dev.doubao2api（字节）",
    "base(2).apk": "Go 原生网关 com.joy4fire.wb2apimobile（腾讯）",
    "base(3).apk": "CodeBuddy 桌面端（腾讯）",
    "base(4).apk": "小浣熊 dev.raccoon2api",
    "base(5).apk": "元宝 dev.yuanbao2api（腾讯）",
}

# 登录相关关键词（分四组，命中即说明它用什么方式）
SIGNALS = {
    "Cookie 类": [
        rb"sessionid", rb"session_id", rb"passport_csrf_token", rb"sso_ticket",
        rb"msToken", rb"web_session", rb"ttwid", rb"passport_auth_status",
        rb"Cookie:", rb"Set-Cookie", rb"cookieJar", rb"cookieStore",
    ],
    "Token / 授权类": [
        rb"access[_-]?token", rb"refresh[_-]?token", rb"Bearer", rb"id[_-]?token",
        rb"oauth", rb"authorize", rb"authorization_code", rb"device[_-]?code",
        rb"user[_-]?code", rb"login[_-]?ticket", rb"x-ide-type",
    ],
    "存储位置": [
        rb"SharedPreferences", rb"getSharedPreferences", rb"localStorage",
        rb"sessionStorage", rb"IndexedDB", rb"cookieStore", rb"sqlite",
        rb"WebView", rb"evaluateJavascript", rb"addJavascriptInterface",
    ],
    "校验 / 登录态": [
        rb"isLogin", rb"checkLogin", rb"loginStatus", rb"isLoggedIn",
        rb"getUserInfo", rb"loginState",
        "未登录".encode(), "请先登录".encode(),
        "登录已过期".encode(), "未授权".encode(),
        "token 已过期".encode(), "重新登录".encode(),
    ],
    "设备标识": [
        rb"qimei", rb"device[_-]?id", rb"deviceId", rb"uuid", rb"android_id",
        rb"imei", rb"mac", rb"fingerprint", rb"openudid", rb"oaid",
    ],
    "特殊字段": [
        rb"hy_token", rb"hy_user", rb"uskey", rb"uskeyMd5", rb"verifyFp",
        rb"fp", rb"session_key", rb"skey", rb"ticket", rb"csrf",
    ],
}


def strings_of(blob, minlen=4):
    return [m.group().decode("utf-8", "replace")
            for m in re.finditer(rb"[\x20-\x7e]{%d,}" % minlen, blob)]


def u16_of(blob):
    out = []
    for m in re.finditer(rb"(?:[\x20-\x7e]\x00){4,}", blob):
        try:
            out.append(m.group().decode("utf-16-le"))
        except Exception:
            pass
    return out


def analyze(path, label):
    z = zipfile.ZipFile(path)
    blob = b""
    for n in sorted(z.namelist()):
        if n.endswith(".dex"):
            blob += z.read(n)
    ss = strings_of(blob, 4) + u16_of(blob)
    pool = set(ss)
    low = blob.lower()

    print("=" * 78)
    print("【%s】%s" % (os.path.basename(path), label))
    print("  dex %d KB   字符串 %d 条" % (len(blob) // 1024, len(ss)))

    # 1) 登录机制信号
    print("\n  ── 登录相关信号 ──")
    for group, pats in SIGNALS.items():
        hits = []
        for p in pats:
            n = len(re.findall(re.escape(p), blob, re.I))
            if n:
                hits.append("%s×%d" % (p.decode("ascii", "replace")[:22], n))
        mark = "●" if hits else "○"
        print("   %s %-14s %s" % (mark, group, "  ".join(hits[:6]) or "无"))

    # 2) 域名
    doms = Counter()
    for s in ss:
        for m in re.finditer(r"https?://([a-zA-Z0-9.\-]+)", s):
            d = m.group(1).lower()
            if len(d) > 4 and "w3.org" not in d and "example" not in d:
                doms[d] += 1
    top = [d for d, _ in doms.most_common(8)]
    print("\n  ── 域名 TOP8 ──\n   " + "  ".join(top))

    # 3) 端点（区分是 APP 自己的还是厂商的）
    paths = set()
    for s in ss:
        for m in re.finditer(r"/(?:v\d|api|admin|oauth|login|auth|session)[/\w\-.]*", s):
            t = m.group(0)
            if 3 < len(t) < 60 and " " not in t:
                paths.add(t)
    print("\n  ── 登录相关端点（%d）──" % len(paths))
    for p in sorted(paths)[:22]:
        print("   " + p)

    # 4) 存储方式
    print("\n  ── 凭据存储 ──")
    for k in ("SharedPreferences", "localStorage", "sqlite", "WebView",
              "evaluateJavascript"):
        n = len(re.findall(k.encode(), blob, re.I))
        if n:
            print("   %-22s ×%d" % (k, n))

    # 5) 中文登录提示（最能说明它怎么判登录态）
    cn = set()
    for s in ss:
        if 2 <= len(s) <= 40 and sum("\u4e00" <= c <= "\u9fff" for c in s) >= 2:
            cn.add(s)
    tips = [x for x in cn if any(k in x for k in
              ("登录", "授权", "凭据", "凭证", "token", "Token",
               "未登录", "过期", "失效"))]
    print("\n  ── 登录相关中文提示（%d）──" % len(tips))
    for t in sorted(tips)[:18]:
        print("   " + t)
    return {
        "apk": os.path.basename(path), "label": label,
        "signals": {g: sum(len(re.findall(re.escape(p), blob, re.I))
                              for p in ps)
                    for g, ps in SIGNALS.items()},
        "domains": top, "paths": sorted(paths), "tips": sorted(tips),
    }


def main():
    targets = sys.argv[1:] or list(APKS)
    results = []
    for name in targets:
        p = os.path.join(ROOT, name)
        if not os.path.exists(p):
            print("跳过（不存在）%s" % name)
            continue
        try:
            results.append(analyze(p, APKS.get(name, "")))
        except Exception as e:
            print("分析 %s 失败：%s" % (name, e))
    out = os.path.join(HERE, "data", "apk_login_analysis.txt")
    with io.open(out, "w", encoding="utf-8", newline="\n") as f:
        for r in results:
            f.write("=" * 78 + "\n")
            f.write("【%s】%s\n" % (r["apk"], r["label"]))
            f.write("信号: %s\n" % r["signals"])
            f.write("域名: %s\n" % "  ".join(r["domains"]))
            f.write("端点(%d):\n  %s\n" % (len(r["paths"]), "\n  ".join(r["paths"])))
            f.write("登录提示(%d):\n  %s\n\n" % (len(r["tips"]), "\n  ".join(r["tips"])))
    print("\n完整结果: %s" % out)


if __name__ == "__main__":
    main()
