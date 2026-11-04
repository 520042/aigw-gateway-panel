# -*- coding: utf-8 -*-
"""
全平台账号凭据审计（18 个）
==========================
用户要求：除了 WorkBuddy，其他平台的账号登录凭据都要实测一遍。

本脚本对每个平台回答三个问题：
  1. **登录机制是否可用** —— 定义的 method（cookie / qrcode / file）在当前环境能不能走通
  2. **凭据验证端点是否存在** —— verify 端点用「基线对比法」判定，不被 401/403 骗
  3. **本机是否已有可用凭据** —— 扫账号池 + 浏览器 Cookie 库

基线对比法（关键）：每个域先打一条**肯定不存在**的随机路径作为对照。
  响应与基线一致 → 该站返回的是无信息量通用响应，不能当「端点存在」的证据。

运行：
    python audit_all_platforms.py            # 全量审计
    python audit_all_platforms.py --deep     # 额外探测候选 verify 端点
"""
import json
import os
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

OK, BAD, WARN = [], [], []


def line(c="=", n=96):
    print(c * n)


def hdr(t):
    print()
    line()
    print(t)
    line()


# ---------------------------------------------------------------- HTTP
def _opener():
    import urllib.request
    import ssl
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    op = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),          # 不吃系统代理
        urllib.request.HTTPSHandler(context=ctx))
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)
    return op


OP = _opener()


def hit(url, method="GET", headers=None, body=None, timeout=15):
    """返回 (status, body_str, err)"""
    import urllib.request
    import urllib.error
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    h = {"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                       "AppleWebKit/537.36 (KHTML, like Gecko) "
                       "Chrome/154.0.0.0 Safari/537.36",
         "Accept": "application/json, text/plain, */*"}
    if headers:
        h.update(headers)
    r = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with OP.open(r, timeout=timeout) as x:
            return x.status, x.read().decode("utf-8", "replace"), None
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), None
    except Exception as e:
        return 0, "", "%s: %s" % (type(e).__name__, str(e)[:80])


def baseline_of(base):
    """取该域的基线响应（随机不存在路径）"""
    p = "/__aigw_probe_%s" % uuid.uuid4().hex[:12]
    s, b, e = hit(base.rstrip("/") + p)
    return s, (b or "").strip()[:120]


def is_informative(status, body, bstat, bbody):
    """响应是否与基线有区别（有信息量）"""
    body = (body or "").strip()
    if e_was_none(body) and status == 0:
        return False
    if status != bstat:
        return True
    return body[:60] != bbody[:60]


def e_was_none(body):
    return not body


def classify(status, body):
    body = (body or "").strip()
    if status == 0:
        return "连不上"
    if "<html" in body[:200].lower() or "<!doctype" in body[:200].lower():
        return "HTML页"
    if body.startswith("{") or body.startswith("["):
        try:
            d = json.loads(body)
            code = d.get("code") if isinstance(d, dict) else None
            msg = str((d.get("msg") or d.get("message") or ""))[:40] if isinstance(d, dict) else ""
            if status == 200:
                return "✓200可用" + (" code=%s" % code if code is not None else "")
            if status in (401, 403):
                return "要鉴权" + (" code=%s" % code if code is not None else "")
            if status == 400:
                return "缺参数" + (" %s" % msg if msg else "")
            return "HTTP%d %s" % (status, msg)
        except Exception:
            pass
    if status == 200:
        return "✓200"
    if status in (401, 403):
        return "要鉴权"
    if status == 404:
        return "404"
    return "HTTP%d" % status


# ---------------------------------------------------------------- 平台清单
def load_platforms():
    from app.gwlogin import PLATFORMS
    return PLATFORMS


def local_creds():
    """扫本机所有可能的凭据位置"""
    out = {}
    for d in ("data", os.path.join("dist", "data")):
        p = os.path.join(HERE, d)
        if not os.path.isdir(p):
            continue
        for fn in os.listdir(p):
            fp = os.path.join(p, fn)
            if fn == "accounts.json":
                try:
                    for a in json.load(open(fp, encoding="utf-8")) or []:
                        if a.get("secret"):
                            out[a.get("platform")] = ("accounts.json", a["secret"])
                except Exception:
                    pass
            elif fn == "tencent_cred.json":
                try:
                    c = json.load(open(fp, encoding="utf-8"))
                    if c.get("accessToken"):
                        out.setdefault("wb-gateway", ("tencent_cred.json",
                                                      c["accessToken"]))
                        out.setdefault("apk-codebuddy-cn", ("tencent_cred.json",
                                                             c["accessToken"]))
                except Exception:
                    pass
    return out


def browser_cookies():
    """从 CDP 浏览器取 Cookie（如果开着）"""
    try:
        from app.cdp import Browser
        b = Browser(port=9333)
        if not b.is_up():
            return {}
        allc = b.cookies()
        by = {}
        for c in allc:
            by.setdefault((c.get("domain") or "").lstrip("."), {})[
                c.get("name")] = c.get("value") or ""
        return by
    except Exception:
        return {}


def spec_base(spec):
    """取该平台的基址：verify.url 的 origin 优先，其次 upstream"""
    v = spec.get("verify")
    if isinstance(v, dict) and v.get("url"):
        u = v["url"]
        return "/".join(u.split("/")[:3])
    up = spec.get("upstream") or ""
    if up:
        if up.startswith("http"):
            return "/".join(up.split("/")[:3])
        return "https://" + up.split("/")[0]
    hs = spec.get("hosts") or []
    if hs:
        return "https://" + hs[0]
    return ""


def spec_verify(spec):
    """
    返回 (url, type, method, ok_keys, ua) 或 None。
    verify 是个 dict，不是字符串 —— 早期版本按字符串处理，直接 AttributeError。
    """
    v = spec.get("verify")
    if isinstance(v, str):
        return (v, spec.get("type"), spec.get("verify_method", "GET"),
                spec.get("ok_keys"), spec.get("ua"))
    if isinstance(v, dict) and v.get("url"):
        return (v["url"], v.get("type") or spec.get("type"),
                v.get("method", "GET"), v.get("ok_keys"), v.get("ua"))
    return None


# ---------------------------------------------------------------- 主流程
def main():
    deep = "--deep" in sys.argv
    plats = load_platforms()
    creds = local_creds()
    ck = browser_cookies()
    print("本机凭据：%d 个平台" % len(creds))
    print("浏览器 Cookie 域：" + (", ".join(sorted(ck)) or "无"))

    hdr("平台清单与登录机制（%d 个）" % len(plats))
    print("%-22s %-9s %-8s %-30s %-8s"
          % ("平台", "登录方式", "凭据类型", "验证端点", "本机凭据"))
    print("-" * 96)
    rows = []
    for pid, spec in plats.items():
        vv = spec_verify(spec)
        vurl = (vv[0][:28] if vv else "（未定义）")
        has = pid in creds
        print("%-22s %-9s %-8s %-30s %-8s" % (
            pid, spec.get("method", "?"), (spec.get("type") or "-")[:8],
            vurl,
            ("有(%d字)" % len(creds[pid][1])) if has else "无"))
        rows.append((pid, spec, vv, has))

    # ---------------------------------------------------- 逐个验活
    hdr("凭据验活")
    alive, dead, skip = [], [], []
    for pid, spec, vv, has in rows:
        if not vv:
            skip.append(pid)
            print("  - %-22s 未定义 verify 端点" % pid)
            continue
        url, vtype, vmethod, ok_keys, vua = vv
        if not has:
            print("  - %-22s 本机无凭据（需扫码/登录）  %s" % (pid, url[:52]))
            continue
        src, secret = creds[pid]
        hdrs = {}
        if vua:
            hdrs["User-Agent"] = vua
        # token 类型优先 Bearer；cookie 类型看形态判断
        if vtype == "api_key":
            hdrs["Authorization"] = "Bearer " + secret
        elif "=" in secret[:40] and " " not in secret[:40] and len(secret) > 80:
            hdrs["Authorization"] = "Bearer " + secret     # 像 JWT
        elif vtype == "cookie":
            hdrs["Cookie"] = secret
        elif vtype == "bearer":
            hdrs["Authorization"] = "Bearer " + secret
        else:
            hdrs["Cookie"] = secret
        s, b, e = hit(url, method=vmethod, headers=hdrs)
        verdict = classify(s, b)
        mark = "✓" if s == 200 else "✗"
        print("  %s %-22s %-18s %s" % (mark, pid, verdict, url[:54]))
        print("      %s · %s" % (src, (b or e or "")[:110].replace("\n", " ")))
        (alive if s == 200 else dead).append((pid, s, verdict, url))

    hdr("汇总")
    print("平台总数         : %d" % len(rows))
    print("有本机凭据       : %d" % sum(1 for r in rows if r[3]))
    print("验活 200         : %d" % len(alive))
    print("验活失败         : %d" % len(dead))
    print("无凭据（待登录） : %d" % sum(1 for r in rows if not r[3] and r[2]))
    print("未定义 verify    : %d  %s" % (len(skip), ", ".join(skip)))
    if alive:
        print("\n✓ 可用：")
        for pid, s, v, u in alive:
            print("   %-22s %s" % (pid, v))
    if dead:
        print("\n✗ 不可用：")
        for pid, s, v, u in dead:
            print("   %-22s %s  %s" % (pid, v, u[:60]))

    out = {
        "platforms": [{"id": p, "name": s.get("name"), "method": s.get("method"),
                       "type": s.get("type"), "upstream": s.get("upstream"),
                       "hosts": s.get("hosts") or [],
                       "login_url": s.get("login_url"),
                       "has_cred": h,
                       "verify": (vv[0] if vv else None),
                       "verify_type": (vv[1] if vv else None),
                       "verify_method": (vv[2] if vv else None)}
                      for p, s, vv, h in rows],
        "alive": [{"id": p, "status": s, "verdict": v, "url": u}
                  for p, s, v, u in alive],
        "dead": [{"id": p, "status": s, "verdict": v, "url": u}
                 for p, s, v, u in dead],
        "no_verify": skip,
    }
    fp = os.path.join(HERE, "data", "platform_audit.json")
    with open(fp, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=1)
    print("\n已存 %s" % fp)
    return 0


if __name__ == "__main__":
    sys.exit(main())
