# -*- coding: utf-8 -*-
"""
腾讯 CodeBuddy / WorkBuddy 直连层
=================================
**这是「把 workbuddy-gateway 的能力移植进面板」的落地模块。**

从 workbuddy-gateway 二进制里逆向出的规格（2026-10-04，详见
data/gateway_spec.txt / data/gateway_endpoints.txt）：

  User-Agent : CLI/2.143.1 CodeBuddy/2.143.1     ← 它伪装成 CodeBuddy CLI
  X-IDE-Type : production
  X-IDE-Name : workbuddy
  上游基址    : https://copilot.tencent.com

  ┌ 能力              端点                                    实测(未登录)
  ├─────────────────────────────────────────────────────────────────
  │ OAuth 登录       GET  /v2/plugin/auth/token?state=      200 「login ing」
  │ 刷新 token       POST /v2/plugin/auth/token/refresh     400 「refreshToken is empty」
  │ 官方倍率         GET  /v3/config                        200 OK
  │ 额度             GET  /v2/billing/meter/get-user-resource  401
  │ 领礼             POST /v2/billing/meter/claim-gift      404（该走别的站）
  │ 成长抽奖         POST /activity/growth/lottery/draw     401
  │ 成长首签         POST /activity/growth/buddy/first      401
  │ 成长热力         GET  /activity/growth/heatmap          401
  │ 模型目录         GET  /console/enterprises/personal/models  400「未找到 cookie」

**结论：所有腾讯接口都活着且直连可达，401 只表示缺凭据。**
所以面板不需要 workbuddy-gateway 进程 —— 只要拿到 OAuth token，
上面的能力都能直接调。这就是「移植」的完整含义。

用法（拿到 token 后）：
    from app.tencent import Tencent
    tc = Tencent(token)
    tc.rates()        # 官方倍率
    tc.quota()         # 额度
    tc.growth_lottery()
"""
import json
import os
import ssl
import urllib.error
import urllib.parse
import urllib.request

# ── 从二进制逆向出的规格（改动前请先看 data/gateway_spec.txt）─────────────
UA = "CLI/2.143.1 CodeBuddy/2.143.1"
IDE_TYPE = "production"
IDE_NAME = "workbuddy"
BASE_CN = "https://copilot.tencent.com"
BASE_INTL = "https://www.codebuddy.ai"
BASE_CN2 = "https://www.codebuddy.cn"

_ctx = ssl.create_default_context()
_ctx.check_hostname = False
_ctx.verify_mode = ssl.CERT_NONE
_op = urllib.request.build_opener(
    urllib.request.ProxyHandler({}),        # 面板走直连，不吃系统代理
    urllib.request.HTTPSHandler(context=_ctx))

# 网关暴露的端点 → 对应的腾讯上游调用
ENDPOINTS = {
    "oauth_state":   ("GET",  "/v2/plugin/auth/state?platform={platform}"),
    "oauth_token":   ("GET",  "/v2/plugin/auth/token?state={state}"),
    "oauth_refresh": ("POST", "/v2/plugin/auth/token/refresh"),
    "login_account": ("POST", "/v2/plugin/login/account?state={state}"),
    "rates":         ("GET",  "/v3/config"),
    "quota":         ("GET",  "/v2/billing/meter/get-user-resource"),
    "usage_summary": ("GET",  "/billing/meter/get-user-resource-summary"),
    "daily_checkin": ("POST", "/v2/billing/meter/daily-checkin"),
    "claim_gift":    ("POST", "/v2/billing/meter/claim-gift"),
    "models":        ("GET",  "/console/enterprises/personal/models"),
    "growth_lottery": ("POST", "/activity/growth/lottery/draw"),
    "growth_buddy":  ("POST", "/activity/growth/buddy/first"),
    "growth_heatmap": ("GET",  "/activity/growth/heatmap"),
    "growth_makeup": ("POST", "/activity/growth/makeup-cards/use"),
    "invite_bind":   ("POST", "/activity/workbuddy/invitation/v2/bind"),
}


class Tencent(object):
    """腾讯 CodeBuddy / WorkBuddy 直连客户端（免 workbuddy-gateway 进程）"""

    def __init__(self, token="", base=BASE_CN, timeout=20):
        self.token = (token or "").strip()
        self.base = base.rstrip("/")
        self.timeout = timeout

    # ------------------------------------------------------------------ 底层
    def _headers(self):
        h = {
            "User-Agent": UA,
            "Accept": "application/json",
            "X-IDE-Type": IDE_TYPE,
            "X-IDE-Name": IDE_NAME,
        }
        if self.token:
            # 网关用 Bearer；部分接口同时认 Cookie，双写提高命中率
            h["Authorization"] = "Bearer " + self.token
            if "=" in self.token[:60] and " " not in self.token[:60]:
                h["Cookie"] = self.token
        return h

    def call(self, name, method=None, **kw):
        """
        调一个已登记的端点。
        返回 (ok, data|错误信息)
        """
        spec = ENDPOINTS.get(name)
        if not spec:
            return False, "未知端点 %s" % name
        m, tpl = spec
        m = method or m
        path = tpl.format(
            platform=kw.get("platform", "CLI"),
            state=urllib.parse.quote(str(kw.get("state", ""))),
        )
        url = self.base + path
        body = None
        if m != "GET":
            payload = {k: v for k, v in kw.items()
                       if k not in ("platform", "state", "token")}
            body = json.dumps(payload or {}).encode("utf-8")
        req = urllib.request.Request(url, data=body, method=m)
        for k, v in self._headers().items():
            req.add_header(k, v)
        if body is not None:
            req.add_header("Content-Type", "application/json")
        for k in list(os.environ):
            if k.upper().endswith("_PROXY"):
                os.environ.pop(k, None)
        try:
            with _op.open(req, timeout=self.timeout) as r:
                raw = r.read().decode("utf-8", "replace")
                return True, _parse(raw, r.status)
        except urllib.error.HTTPError as e:
            raw = e.read().decode("utf-8", "replace")
            return False, {"http": e.code, "body": raw[:300]}
        except Exception as e:
            return False, "%s: %s" % (type(e).__name__, str(e)[:120])

    # ------------------------------------------------------------------ 能力
    def rates(self):
        """官方倍率（/v3/config 的 data.models）"""
        ok, d = self.call("rates")
        if not ok:
            return False, d
        return True, (d.get("data") or {}) if isinstance(d, dict) else d

    def quota(self):
        ok, d = self.call("quota")
        return ok, d

    def models(self):
        ok, d = self.call("models")
        return ok, d

    def daily_checkin(self):
        return self.call("daily_checkin")

    def growth_lottery(self):
        return self.call("growth_lottery")

    def growth_buddy(self):
        return self.call("growth_buddy")

    def refresh_token(self, refresh_token):
        return self.call("oauth_refresh", token=refresh_token)

    def oauth_state(self, platform="CLI"):
        return self.call("oauth_state", platform=platform)


def _parse(raw, status=200):
    s = (raw or "").strip()
    if s.startswith("{") or s.startswith("["):
        try:
            return json.loads(s)
        except Exception:
            pass
    if "<html" in s[:200].lower():
        # 401/403 的 Nginx HTML 页
        code = status
        m = re_search_code(s)
        return {"_html": True, "http": code, "m": m,
                "note": "返回 HTML，多半是网关级鉴权拦截（Nginx）"}
    return {"_raw": s[:500], "http": status}


def re_search_code(s):
    import re
    m = re.search(r"<h1>(\d{3})", s) or re.search(r">(\d{3})<", s)
    return int(m.group(1)) if m else None


def probe_all(token="", base=BASE_CN, timeout=8):
    """
    把登记的端点全探一遍，返回每条的状态。
    用来验证「不依赖网关进程也能拿到哪些能力」。
    """
    tc = Tencent(token, base, timeout=timeout)
    out = []
    for name in ENDPOINTS:
        ok, d = tc.call(name)
        if isinstance(d, dict):
            code = d.get("http") or (0 if d.get("code") is None else d["code"])
            note = d.get("msg") or d.get("note") or d.get("_raw", "")[:60]
        else:
            code, note = 0, str(d)[:60]
        out.append({"name": name, "ok": ok, "code": code,
                    "note": str(note)[:90],
                    "method": ENDPOINTS[name][0]})
    return out


if __name__ == "__main__":
    import sys
    tok = sys.argv[1] if len(sys.argv) > 1 else ""
    print("目标 %s   token=%s" % (BASE_CN, "有" if tok else "无"))
    print("=" * 74)
    for r in probe_all(tok):
        flag = "OK " if r["ok"] else "-- "
        print("%s%-16s %-5s %-5s %s" % (flag, r["name"], r["method"],
                                          r["code"], r["note"]))
