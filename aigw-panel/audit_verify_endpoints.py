# -*- coding: utf-8 -*-
"""
逐平台 verify 端点审计（不需要凭据，靠基线对比法判存在性）
=========================================================
上一轮发现 `apk-codebuddy-cn` 的 verify 指向 404 —— 端点定义本身错了。
本脚本对每个平台的 verify 端点做**无凭据基线对比**：

  无凭据 GET → 401/403  → 端点存在，要凭据
  无凭据 GET → 404      → 端点不存在（定义错了）
  无凭据 GET → 200      → 端点免登录（要复核是不是通用首页）
  与随机基线完全一致   → 无信息量，不能判定

用本机已有的 CodeBuddy token 再复测一遍，能验活的直接确认。
"""
import json
import os
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "app"))

from audit_all_platforms import (                       # noqa: E402
    hit, classify, load_platforms, spec_verify, local_creds, hdr, line)


def base_of(url):
    return "/".join(url.split("/")[:3])


def probe_exists(url, method="GET", headers=None):
    """
    返回 (verdict, detail)
    verdict ∈ 存在-要凭据 / 不存在-404 / 疑似免登录 / 与基线一致 / 连不上
    """
    b = base_of(url)
    bs, bb, _ = hit(b + "/__probe_%s" % uuid.uuid4().hex[:12])
    s, body, err = hit(url, method=method, headers=headers)
    if s == 0:
        return "连不上", "%s %s" % (s, err)
    if s == 404:
        return "不存在-404", body[:60].replace("\n", " ")
    if s in (401, 403):
        return "存在-要凭据", classify(s, body)
    if s == 200:
        # 和基线一样就是通用响应，不能算「端点存在」
        if s == bs and body[:60] == bb[:60]:
            return "与基线一致", body[:60].replace("\n", " ")
        return "疑似免登录", classify(s, body)
    return "HTTP%d" % s, body[:60].replace("\n", " ")


def main():
    plats = load_platforms()
    creds = local_creds()
    tok = ""
    try:
        from app import tlogin
        tok = (tlogin.load() or {}).get("accessToken") or ""
    except Exception:
        pass

    hdr("平台 verify 端点存在性审计（无凭据，基线对比法）")
    print("%-22s %-16s %-42s %s" % ("平台", "判定", "端点", "说明"))
    print("-" * 110)
    rows = []
    for pid, spec in plats.items():
        vv = spec_verify(spec)
        if not vv:
            print("%-22s %-16s %-42s %s" % (pid, "未定义", "-", "gwlogin 里没写 verify"))
            rows.append({"id": pid, "verdict": "未定义", "url": None})
            continue
        url, vtype, vmethod, ok_keys, vua = vv
        hdrs = {"User-Agent": vua} if vua else {}
        v, d = probe_exists(url, method=vmethod, headers=hdrs)
        print("%-22s %-16s %-42s %s" % (pid, v, url[:42], d[:34]))
        rows.append({"id": pid, "verdict": v, "url": url, "detail": d,
                     "method": vmethod, "type": vtype, "ua": vua,
                     "ok_keys": ok_keys})

    # 用真 token 复测有凭据的
    if tok:
        hdr("用本机 CodeBuddy token 复测")
        hdrs = {"Authorization": "Bearer " + tok,
                "User-Agent": "CLI/2.143.1 CodeBuddy/2.143.1",
                "X-Ide-Type": "production", "X-Ide-Name": "workbuddy"}
        cands = [
            ("CodeBuddy 国内 · 倍率", "GET",
             "https://copilot.tencent.com/v3/config"),
            ("CodeBuddy 国内 · 模型", "GET",
             "https://copilot.tencent.com/console/enterprises/personal/models"),
            ("CodeBuddy 国内 · 额度(POST)", "POST",
             "https://copilot.tencent.com/v2/billing/meter/get-user-resource"),
            ("CodeBuddy 国内 · 签到(POST)", "POST",
             "https://copilot.tencent.com/v2/billing/meter/daily-checkin"),
            ("gwlogin 写的额度(wrong域)", "GET",
             "https://www.codebuddy.cn/v2/billing/meter/get-user-resource"),
            ("www 域 · 倍率", "GET",
             "https://www.codebuddy.cn/v3/config"),
        ]
        for name, m, u in cands:
            body = b"{}" if m == "POST" else None
            s, b, e = hit(u, method=m, headers=hdrs, body=body)
            print("  %-26s %-6s %-8s %s" % (name, m, s,
                                            classify(s, b)))

    hdr("汇总")
    bad = [r for r in rows if r["verdict"] in ("不存在-404", "未定义", "连不上")]
    print("verify 端点可用    : %d" % sum(1 for r in rows
                                        if r["verdict"] in ("存在-要凭据",
                                                            "疑似免登录")))
    print("verify 端点有问题  : %d" % len(bad))
    if bad:
        print("\n需要修的：")
        for r in bad:
            print("   %-22s %-12s %s" % (r["id"], r["verdict"],
                                       (r.get("url") or "-")[:56]))
    out = os.path.join(HERE, "data", "verify_endpoint_audit.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(rows, f, ensure_ascii=False, indent=1)
    print("\n已存 %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
