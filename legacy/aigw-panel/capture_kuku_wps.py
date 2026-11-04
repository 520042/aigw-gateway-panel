# -*- coding: utf-8 -*-
"""
抓 kuku / wps 的真实接口（登录后才有）
=====================================
这两个平台的 verify 端点之前各试了 10 个候选全 404，只能登录后抓包定位。

录制方式：起一个带 CDP 的**可见 Chrome**，打开登录页，挂上 Network 域全量录制。
你在窗口里正常登录（扫码/账密都行），登录完成后回来说一声。

它会自动：
  1. 抓请求 URL / 方法 / 关键请求头 / POST body
  2. 抓响应体（>=200 <400 的都抓，含 401/403 的错误信息）
  3. **自动提取凭据形态** —— 哪些是 Cookie、哪些是 Authorization、有没有 uskey/x-tt-* 之类的签名头
  4. **自动推荐 verify 端点** —— 挑「返回体里带用户身份字段」的那些

运行：
    python capture_kuku_wps.py            # 录制（Ctrl+C 停止并出报告）
    python capture_kuku_wps.py --report   # 只看已抓到的分析报告
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from app.cdp import Recorder, find_browser      # noqa: E402

OUT = os.path.join(HERE, "data", "kuku_wps_capture.jsonl")
REPORT = os.path.join(HERE, "data", "kuku_wps_report.json")
PROFILE = os.path.join(HERE, "data", "chrome-capture-profile")
PORT = 9333

TARGETS = [
    ("kuku", "库库 AI（百度文库+网盘）", "https://kuku.baidu.com/"),
    ("wps", "WPS AI（金山办公）", "https://www.wps.cn/"),
]

# 登录态/身份相关的关键词 —— 命中这些的请求最可能是 verify 端点
IDENT_KW = ("user", "account", "profile", "me", "info", "point", "credit",
            "balance", "quota", "vip", "member", "login", "session", "token",
            "status", "config", "setting")
# 业务错误里的「未登录」提示，也是线索
ERR_KW = ("未登录", "登录", "unauthorized", "not login", "登录态", "auth")


def read_rows():
    if not os.path.exists(OUT):
        return []
    out = []
    for line in open(OUT, encoding="utf-8"):
        line = line.strip()
        if line:
            try:
                out.append(json.loads(line))
            except Exception:
                pass
    return out


def pick(tag, name, url):
    return {"tag": tag, "name": name, "url": url}


def analyze():
    rows = read_rows()
    if not rows:
        print("没有抓到任何请求：%s" % OUT)
        return 2
    reqs = [r for r in rows if r.get("kind") == "request"]
    resps = {}
    for r in rows:
        if r.get("kind") == "response" and r.get("requestId"):
            resps[r["requestId"]] = r
    bodies = {}
    for r in rows:
        if r.get("kind") == "body" and r.get("requestId"):
            bodies[r["requestId"]] = r.get("body") or ""

    print("=" * 100)
    print("共 %d 个事件 / %d 个请求" % (len(rows), len(reqs)))
    print("=" * 100)

    by_tag = {}
    for r in reqs:
        t = r.get("tag") or "?"
        by_tag.setdefault(t, []).append(r)

    report = {}
    for tag, name, _ in TARGETS:
        rs = by_tag.get(tag) or []
        if not rs:
            print("\n[%s] %s —— 还没有请求（是不是还没登录？）" % (tag, name))
            continue
        print("\n" + "=" * 100)
        print("[%s] %s —— %d 个请求" % (tag, name, len(rs)))
        print("=" * 100)

        # 域名分布
        hosts = {}
        for r in rs:
            u = r.get("url") or ""
            try:
                hosts[u.split("/")[2]] = hosts.get(u.split("/")[2], 0) + 1
            except Exception:
                pass
        print("域名分布：")
        for h, n in sorted(hosts.items(), key=lambda x: -x[1])[:10]:
            print("   %-42s %d" % (h, n))

        # 身份相关端点
        cand = []
        for r in rs:
            u = r.get("url") or ""
            low = u.lower()
            path = "/" + "/".join(u.split("/")[3:]).split("?")[0] if \
                len(u.split("/")) > 3 else u
            score = sum(1 for k in IDENT_KW if k in low)
            st = (resps.get(r.get("requestId") or {}) or {}).get("status")
            body = bodies.get(r.get("requestId") or "") or ""
            # 响应体里带用户身份字段的加权
            has_id = any(k in body for k in
                         ('"nickname"', '"nickName"', '"userId"', '"userid"',
                          '"avatar"', '"mobile"', '"email"', '"name"',
                          '"points"', '"balance"', '"credit"', '"vip'))
            if score and not any(x in low for x in
                                 (".js", ".css", ".png", ".jpg", ".svg",
                                  ".woff", "google", "beacon", "hm.baidu.com",
                                  "hm.baidu.com", "gstatic", "doubleclick")):
                cand.append((score + (3 if has_id else 0), r, path, st, body))
        cand.sort(key=lambda x: -x[0])

        print("\n身份相关端点（按可能性排序）：")
        picked = []
        for score, r, path, st, body in cand[:40]:
            if path in picked:
                continue
            picked.append(path)
            mark = "★" if score >= 4 else " "
            print("  %s %-4s %-58s score=%d" %
                  (mark, st or "-", path[:58], score))
            if body:
                print("        resp: %s" % body[:150].replace("\n", " "))
        report[tag] = {"host": hosts, "candidates": picked,
                       "count": len(rs)}

        # 凭据形态
        print("\n凭据形态（关键请求头）：")
        creds_seen = {}
        for r in rs:
            for k, v in (r.get("headers") or {}).items():
                kl = k.lower()
                if kl in ("cookie", "authorization") or \
                        kl.startswith("x-") or "token" in kl or "uskey" in kl:
                    creds_seen.setdefault(k, set()).add(str(v)[:60])
        for k, vals in sorted(creds_seen.items()):
            sample = list(vals)[0]
            print("   %-26s %s  (示例: %s)" % (k, len(vals), sample[:50]))
        report[tag]["cred_headers"] = {k: sorted(v)[0]
                                       for k, v in creds_seen.items()}

    fp = REPORT
    with open(fp, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=1)
    print("\n报告已存 %s" % fp)
    return 0


def main():
    if "--report" in sys.argv:
        return analyze()

    exe = find_browser()
    if not exe:
        print("没找到 Chrome / Edge")
        return 2
    if os.path.exists(OUT):
        os.remove(OUT)

    # 一次开一个标签，用 Recorder 的 tag 区分
    rec = Recorder(port=PORT, out=OUT, user_data_dir=PROFILE, exe=exe)
    # 给 Recorder 加 tag 支持：包一层 _write
    orig_write = rec._write
    cur = {"tag": "?"}

    def tagged(r):
        r["tag"] = cur["tag"]
        orig_write(r)
    rec._write = tagged

    for tag, name, url in TARGETS:
        cur["tag"] = tag
        if not rec.browser.is_up():
            rec.start(url)
        else:
            rec.browser.open_tab(url)
        time.sleep(4)
    cur["tag"] = "?"

    print("=" * 78)
    print("已打开两个登录页并在录制：")
    for tag, name, url in TARGETS:
        print("  %-6s %-22s %s" % (tag, name, url))
    print("=" * 78)
    print("请在浏览器窗口里**分别登录**这两个站（哪个先登都行）。")
    print("登录完成后回到这里按 Ctrl+C 停止，我会打印完整分析。\n")

    seen = set()
    try:
        while True:
            time.sleep(1)
            for r in read_rows()[-80:]:
                if r.get("kind") != "request":
                    continue
                u = r.get("url") or ""
                key = (r.get("tag"), u)
                if key in seen or len(key[1]) > 120:
                    continue
                seen.add(key)
                low = u.lower()
                if any(k in low for k in IDENT_KW) and \
                        not any(x in low for x in (".js", ".css", ".png",
                                                   ".svg", "google", "beacon",
                                                   "gstatic", "doubleclick",
                                                   "hm.baidu", "gdt.qq")):
                    print("  [%-5s] %s" % (r.get("tag"), u[:130]))
    except KeyboardInterrupt:
        pass
    finally:
        n = rec.stop()
        print("\n录制结束，%d 个事件 -> %s" % (n, OUT))
    return analyze()


if __name__ == "__main__":
    sys.exit(main())
