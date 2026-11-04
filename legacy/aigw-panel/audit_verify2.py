# -*- coding: utf-8 -*-
"""
全平台 verify 端点审计（并行版）
=================================
串行打 18 个域太慢（Google 域国内直接不通，一个能卡几分钟）。
改成 ThreadPoolExecutor 并发 + 8 秒超时，30 秒内出全部结果。
"""
import json
import os
import sys
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "app"))

from audit_all_platforms import (                      # noqa: E402
    hit, classify, load_platforms, spec_verify, local_creds)


def base_of(url):
    return "/".join(url.split("/")[:3])


def probe(args):
    pid, url, method, ua = args
    hdrs = {"User-Agent": ua} if ua else {}
    try:
        b = base_of(url)
        bs, bb, _ = hit(b + "/__probe_%s" % uuid.uuid4().hex[:12], timeout=8)
        s, body, err = hit(url, method=method, headers=hdrs, timeout=8)
    except Exception as e:
        return pid, url, "异常", str(e)[:60]
    if s == 0:
        return pid, url, "连不上", (err or "")[:50]
    if s == 404:
        return pid, url, "不存在-404", (body or "")[:44].replace("\n", " ")
    if s in (401, 403):
        return pid, url, "存在-要凭据", classify(s, body)
    if s == 200:
        if s == bs and (body or "")[:60] == (bb or "")[:60]:
            return pid, url, "与基线一致", (body or "")[:44].replace("\n", " ")
        return pid, url, "疑似免登录", classify(s, body)
    return pid, url, "HTTP%d" % s, (body or "")[:44].replace("\n", " ")


def probe_wild(pid, host, cands, method="GET", ua=None, token=""):
    """在给定 host 上试一批候选路径（带基线对比）"""
    hdrs = {"User-Agent": ua} if ua else {}
    if token:
        hdrs["Authorization"] = "Bearer " + token
    out = []
    b = "https://" + host
    try:
        bs, bb, _ = hit(b + "/__probe_%s" % uuid.uuid4().hex[:12], timeout=8)
    except Exception:
        bs, bb = -1, ""
    for c in cands:
        try:
            s, body, err = hit(b + c, method=method, headers=hdrs, timeout=8)
        except Exception as e:
            out.append((c, -1, str(e)[:40], ""))
            continue
        if s == 0:
            out.append((c, s, (err or "")[:40], "连不上"))
        elif s == 404:
            out.append((c, s, (body or "")[:40], "404"))
        elif s in (401, 403):
            out.append((c, s, classify(s, body), "存在-要凭据"))
        elif s == 200:
            if s == bs and (body or "")[:60] == (bb or "")[:60]:
                out.append((c, s, (body or "")[:30], "同基线"))
            else:
                out.append((c, s, classify(s, body), "★200"))
        else:
            out.append((c, s, classify(s, body), "?"))
    return pid, out


def main():
    plats = load_platforms()
    creds = local_creds()
    tok = ""
    try:
        from app import tlogin
        tok = (tlogin.load() or {}).get("accessToken") or ""
    except Exception:
        pass

    print("=" * 104)
    print("verify 端点审计（无凭据 · 基线对比法 · 8s 超时）")
    print("=" * 104)
    print("%-22s %-16s %-46s %s" % ("平台", "判定", "端点", "说明"))
    print("-" * 104)

    jobs, missing = [], []
    for pid, spec in plats.items():
        vv = spec_verify(spec)
        if not vv:
            missing.append(pid)
            print("%-22s %-16s %-46s %s" % (pid, "未定义", "-",
                                            "gwlogin 里没写 verify"))
            continue
        url, vtype, vmethod, ok_keys, vua = vv
        jobs.append((pid, url, vmethod, vua))

    results = {}
    with ThreadPoolExecutor(max_workers=10) as ex:
        futs = {ex.submit(probe, j): j[0] for j in jobs}
        for fu in as_completed(futs):
            pid, url, v, d = fu.result()
            results[pid] = (v, url, d)
    for pid, url, method, ua in jobs:
        v, u, d = results.get(pid, ("?", url, ""))
        print("%-22s %-16s %-46s %s" % (pid, v, u[:46], d[:26]))

    # 候选路径搜索：给缺 verify 的平台找
    if missing:
        print("\n" + "=" * 104)
        print("为缺 verify 的平台搜索候选路径（带 token 的先试）")
        print("=" * 104)
        CANDS = {
            "apk-raccoon": ("xiaohuanxiong.com", [
                "/api/web/points/v1/balance", "/api/web/user/info",
                "/api/web/auth/v1/entitlement_info", "/api/web/office/v3/setting_info",
                "/api/web/user/v1/profile", "/api/web/account/info"]),
            "apk-qwenwork": ("qwenwork.cn", [
                "/api/user/info", "/api/v1/user/info", "/api/user/profile",
                "/api/me", "/api/user/points", "/api/points"]),
            "apk-kuku": ("kuku.baidu.com", [
                "/api/user/info", "/api/v1/user/info", "/api/user/points",
                "/api/points/balance", "/api/me"]),
            "apk-wps": ("www.wps.cn", [
                "/api/user/info", "/api/v1/user/info", "/api/ai/points",
                "/api/points", "/api/user/points", "/api/v1/points/balance"]),
            "apk-nano": ("www.n.cn", [
                "/api/user/info", "/api/v1/user/info", "/api/points",
                "/api/user/points", "/api/nano/points"]),
            "apk-qoder": ("www.qoder.com", [
                "/api/user/info", "/api/v1/user/info", "/api/credits",
                "/api/user/credits", "/api/points"]),
            "apk-metaso": ("metaso.cn", [
                "/api/user/info", "/api/v1/user/info", "/api/agent/user",
                "/api/user/points", "/api/points"]),
            "wb-gateway": ("copilot.tencent.com", [
                "/v3/config", "/console/enterprises/personal/models",
                "/v2/billing/meter/get-user-resource"]),
            "apk-codebuddy": ("copilot.tencent.com", [
                "/v3/config", "/console/enterprises/personal/models"]),
            "apk-go": ("copilot.tencent.com", [
                "/v3/config", "/console/enterprises/personal/models"]),
        }
        hosts = {p: CANDS.get(p, (None, []))[0] for p in missing}
        with ThreadPoolExecutor(max_workers=8) as ex:
            futs = {ex.submit(probe_wild, p, hosts[p], CANDS.get(p, (None, []))[1],
                              "POST" if p == "wb-gateway" else "GET",
                              None, tok): p
                    for p in missing if hosts.get(p)}
            for fu in as_completed(futs):
                pid, out = fu.result()
                good = [c for c, s, d, v in out if v in ("存在-要凭据", "★200")]
                print("\n[%s]" % pid)
                for c, s, d, v in out:
                    if v in ("存在-要凭据", "★200"):
                        print("   ★ %-46s %s %s" % (c, s, d[:34]))
                if not good:
                    print("   （候选里没一个存在，需要抓包定位）")

    print("\n已存 %s"
          % os.path.join(HERE, "data", "verify_audit2.json"))
    with open(os.path.join(HERE, "data", "verify_audit2.json"), "w",
              encoding="utf-8") as f:
        json.dump({k: v for k, v in results.items()}, f,
                  ensure_ascii=False, indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
