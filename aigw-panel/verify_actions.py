# -*- coding: utf-8 -*-
"""
实测 ACTIONS 表里每条接口是否真实存在。
判定：401/403/400 = 存在（要登录）  404 = 路径错  000 = 网络不通
"""
import io
import json
import os
import ssl
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")


def clean():
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)


def probe(url, method):
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    op = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                                     urllib.request.HTTPSHandler(context=ctx))
    body = b"{}" if method != "GET" else None
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("User-Agent", UA)
    req.add_header("Accept", "application/json")
    if body:
        req.add_header("Content-Type", "application/json")
    try:
        with op.open(req, timeout=12) as r:
            return r.status, "OK"
    except urllib.error.HTTPError as e:
        return e.code, ""
    except Exception as e:
        return 0, "%s: %s" % (type(e).__name__, str(e)[:60])


def main():
    clean()
    from app import gwextra
    out = []
    for plat, acts in gwextra.ACTIONS.items():
        base = gwextra.BASE.get(plat, "")
        print("\n### %s   base=%s" % (plat, base or "（未配置）"))
        if not base:
            continue
        for a in acts:
            if a.get("local"):
                print("  [--] %-18s %-6s %-52s %s" % (
                    a["id"], a.get("method", "GET"), a["path"], "APP 本地路由，跳过"))
                out.append({"platform": plat, "id": a["id"], "path": a["path"],
                            "code": 0, "verdict": "本地路由"})
                continue
            if a.get("needs_proxy"):
                print("  [~~] %-18s %-6s %-52s %s" % (
                    a["id"], a.get("method", "GET"), a["path"],
                    "需代理（Google 系国内直连不通）"))
                out.append({"platform": plat, "id": a["id"], "path": a["path"],
                            "code": 0, "verdict": "需代理"})
                continue
            base = a.get("base") or gwextra.BASE.get(plat, "")
            url = base.rstrip("/") + a["path"]
            code, err = probe(url, a.get("method", "GET"))
            if code in (200, 401, 403, 400, 405, 422):
                verdict = "存在"
            elif code == 404:
                verdict = "!! 404 路径错"
            elif code == 0:
                verdict = "网络不通"
            else:
                verdict = "HTTP %s" % code
            flag = "OK " if verdict == "存在" else "BAD"
            print("  [%s] %-18s %-6s %-52s %s%s" % (
                flag, a["id"], a.get("method", "GET"), a["path"],
                code or "-", ("  " + err) if err else ""))
            out.append({"platform": plat, "id": a["id"], "path": a["path"],
                        "code": code, "verdict": verdict})
    bad = [x for x in out if x["verdict"] != "存在"]
    print("\n" + "=" * 70)
    print("合计 %d 条，存在 %d，异常 %d" % (len(out), len(out) - len(bad), len(bad)))
    for x in bad:
        print("  BAD  %s %s %s → %s" % (
            x["platform"], x["id"], x["path"], x["verdict"]))
    with io.open(os.path.join(HERE, "data", "verify_actions.json"), "w",
                 encoding="utf-8", newline="\n") as f:
        f.write(json.dumps(out, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
