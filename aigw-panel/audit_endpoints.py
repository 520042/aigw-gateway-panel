# -*- coding: utf-8 -*-
"""
端点真实性审计（基准对比法）
================================
为什么需要这个
--------------
元宝那次踩了坑：实测 `yuanbao.tencent.com/api/models` 返回 401，我据此认定
"接口存在只是要鉴权"，加了动作表。后来深挖 APK 才发现 `/v1/models` 是那个 APP
自己的**本地网关**端点 —— 腾讯**根本没有**这个接口，401 只是整站鉴权中间件的通用响应。

判别方法
--------
对一个站发两条请求：
  A) 目标路径
  B) **基准路径** —— 同一站上一个肯定不存在的随机路径
如果 A 和 B 的**状态码 + 响应体特征**完全一致，说明该站的 401/404 是无信息量的
通用响应，**不能作为"端点存在"的证据**。

只有当 A 与 B 不一致时，才判定端点真实存在。

用法：python audit_endpoints.py [平台id ...]
"""
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
_ctx = ssl.create_default_context()
_ctx.check_hostname = False
_ctx.verify_mode = ssl.CERT_NONE
_op = urllib.request.build_opener(
    urllib.request.ProxyHandler({}),
    urllib.request.HTTPSHandler(context=_ctx))


def probe(url, method="GET", secret="", timeout=12):
    """只回 (status, body_head)，不抛异常"""
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)
    body = b"{}" if method != "GET" else None
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("User-Agent", UA)
    req.add_header("Accept", "application/json")
    if secret:
        if "=" in secret[:60] and " " not in secret[:60]:
            req.add_header("Cookie", secret)
        else:
            req.add_header("Authorization", "Bearer " + secret)
    if body:
        req.add_header("Content-Type", "application/json")
    try:
        with _op.open(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")[:200]
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")[:200]
    except Exception as e:
        return 0, "%s: %s" % (type(e).__name__, str(e)[:70])


def fingerprint(code, body):
    """把响应压成可比对的特征：状态码 + 归一化后的响应体骨架"""
    b = (body or "").strip()
    # 去掉 trace_id / 随机 id / 时间戳这类噪声
    import re
    b = re.sub(r"[0-9a-f]{16,}", "<HEX>", b)
    b = re.sub(r"\d{10,}", "<NUM>", b)
    b = re.sub(r"\s+", " ", b)
    return "%d|%s" % (code, b[:150])


def audit_platform(pid, base, paths, methods=None):
    """
    对一个平台的若干路径做基准对比。
    返回该平台的审计结论。
    """
    methods = methods or {}
    prefix = None
    for p in paths:
        seg = p.rsplit("/", 1)[0]
        if len(seg) > len(prefix or ""):
            prefix = seg
    prefix = prefix or ""
    # 基准：同前缀下的随机不存在路径
    stamp = str(int(time.time() * 1000))
    bpath = "%s/__aigw_audit_%s__" % (prefix, stamp)
    bcode, bbody = probe(base.rstrip("/") + bpath)
    bfp = fingerprint(bcode, bbody)

    gated = bcode in (401, 403) or bcode == 0
    out = {
        "platform": pid, "base": base, "baseline": {
            "path": bpath, "code": bcode, "fp": bfp,
            "note": "基准（肯定不存在）"},
        "gated": gated, "paths": [],
    }
    for p in paths:
        for m in methods.get(p, ["GET", "POST"]):
            code, body = probe(base.rstrip("/") + p, m)
            fp = fingerprint(code, body)
            same = (fp == bfp)
            verdict = ("无法判定（与基准响应完全一致）"
                       if same else "端点存在（与基准不同）")
            if code == 404 and not same:
                verdict = "不存在（404 且与基准不同）"
            out["paths"].append({
                "path": p, "method": m, "code": code,
                "verdict": verdict, "body": body[:110],
            })
            break
    return out


def main():
    from app import gwextra as G

    targets = sys.argv[1:]
    # 收集要审的 (平台 → base → 路径集)
    plan = {}
    for pid, acts in G.ACTIONS.items():
        base = G.BASE.get(pid)
        if not base:
            continue
        ps = [a["path"] for a in acts
              if a.get("base") is None and a["path"].startswith("/")]
        for a in acts:
            if a.get("base"):
                plan.setdefault((pid, a["base"]), []).append(a["path"])
        if ps:
            plan.setdefault((pid, base), []).extend(ps)
    # MODEL_CATALOGS 的端点也审
    for pid, urls in (G.MODEL_CATALOGS or {}).items():
        for u in urls:
            p = urllib.parse.urlparse(u)
            plan.setdefault((pid, "%s://%s" % (p.scheme, p.netloc)), []).append(p.path)

    if targets:
        plan = {k: v for k, v in plan.items() if k[0] in targets}

    results = []
    for (pid, base), paths in sorted(plan.items()):
        paths = sorted(set(paths))
        if not paths:
            continue
        r = audit_platform(pid, base, paths)
        results.append(r)
        print("=" * 76)
        print("【%s】%s" % (pid, base))
        print("  基准 %s → HTTP %d %s" % (
            r["baseline"]["path"][-28:], r["baseline"]["code"],
            "（该站鉴权/不可达，401/404 无信息量）" if r["gated"] else ""))
        for p in r["paths"]:
            mark = "OK  " if p["verdict"].startswith("端点存在") else (
                "BAD " if "不存在" in p["verdict"] else "?   ")
            print("  %s %-42s %-4s %s" % (
                mark, p["path"][:42], p["code"], p["verdict"]))
            if not p["verdict"].startswith("端点存在"):
                print("        响应: %s" % p["body"][:96].replace("\n", " "))
        print()

    # 汇总
    print("=" * 76)
    print("汇总")
    ok = bad = unk = 0
    problems = []
    for r in results:
        for p in r["paths"]:
            if p["verdict"].startswith("端点存在"):
                ok += 1
            elif "不存在" in p["verdict"]:
                bad += 1
                problems.append((r["platform"], p["path"], p["code"]))
            else:
                unk += 1
    print("  确认存在 %d / 确认不存在 %d / 无法判定 %d" % (ok, bad, unk))
    if problems:
        print("  确认不存在的端点（应从 ACTIONS 里删掉）：")
        for pid, p, c in problems:
            print("    %-20s %-46s %s" % (pid, p, c))
    out = os.path.join(HERE, "data", "endpoint_audit.json")
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print("  详细结果：%s" % out)


if __name__ == "__main__":
    main()
