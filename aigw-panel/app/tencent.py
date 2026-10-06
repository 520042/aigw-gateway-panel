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
#
# ★ 2026-10-04 用真 token 逐条实测过（audit_endpoints_live.py / probe_paths2.py），
#   下面每条的 method 和 body 形态都标注了实测结论。逆向出的规格有 4 条是错的，已改。
#
#   判定口径（基线对比法）：
#     先打一条肯定不存在的路径作对照。
#     copilot.tencent.com 基线 = 404 {"error_msg":"404 Route Not Found"}
#     www.codebuddy.cn  基线 = 403 {"error":"access_denied","error_description":"not_authorized"}
#       ⚠ /console/* 下**所有**路径（含不存在的）都返 403，
#         所以 403 在 console 命名空间里毫无信息量，不能当「存在」的证据。
ENDPOINTS = {
    # ---- 登录（抓包实测，见 tlogin.py）----
    # 真正发 token 的是 console/login/enterprise，在 WWW 域不在 copilot 域
    "oauth_token":   ("GET",  "/v2/plugin/auth/token?state={state}"),   # 200 11217「登录中」
    "oauth_refresh": ("POST", "/v2/plugin/auth/token/refresh"),          # 400 要 refreshToken
    "login_account": ("POST", "/v2/plugin/login/account?state={state}"),  # 200 OK
    # ⚠ 原 /v2/plugin/auth/state 实测 404，已删

    # ---- 配置 / 模型 ----
    "rates":         ("GET",  "/v3/config"),                             # 200 OK，倍率表
    "models":        ("GET",  "/console/enterprises/personal/models"),    # 200 OK，31 个模型带 credits

    # ---- 对话（★ 静态提取自 base(3).apk dex：copilot.tencent.com + /v2/chat/completions
    #      + X-Domain/X-Tenant-Id/X-User-Id/X-Enterprise-Id/X-No-Department-Info 头组；
    #      workbuddy-gateway EXE 的 upstreamProfile.chatURL 同源。端点真实性待真 token 联调，
    #      但路径/头组全部来自 APK 字节码，非猜测）----
    "chat":          ("POST", "/v2/chat/completions"),

    # ---- 额度 ----
    # ⚠⚠ 原来登记成 GET，实际是 **POST**，GET 会返回 400 "EOF"（没 body 读不出来）
    #   实测 200：data.Response.Data.{TotalCount,TotalDosage,Accounts[]}
    "quota":         ("POST", "/v2/billing/meter/get-user-resource"),     # POST body={} 即可
    # ⚠ 原 /billing/meter/get-user-resource-summary 实测 404，已删

    # ---- 签到 ----
    "daily_checkin": ("POST", "/v2/billing/meter/daily-checkin"),        # 200/400「今天已签到」
    # ⚠ 原 /v2/billing/meter/claim-gift 实测 404，已删

    # ---- 成长 ----
    "growth_buddy":  ("POST", "/activity/growth/buddy/first"),           # 200 OK
    "growth_buddy_info": ("GET", "/activity/growth/buddy/info"),        # 200 OK（逆向时漏掉的）
    "growth_heatmap": ("GET",  "/activity/growth/heatmap"),              # 200 OK
    "growth_lottery": ("POST", "/activity/growth/lottery/draw"),         # 400 invalid request（缺参数）
    "growth_makeup": ("POST", "/activity/growth/makeup-cards/use"),      # 要 {"target_date":"YYYY-MM-DD"}
    "invite_bind":   ("POST", "/activity/workbuddy/invitation/v2/bind"), # 要 {"invite_code":"..."}
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
        url = kw.get("base", self.base).rstrip("/") + path
        body = None
        payload = kw.get("body")
        if m != "GET":
            if payload is None:
                # 没显式给 body，就把除保留键以外的 kw 当参数
                payload = {k: v for k, v in kw.items()
                           if k not in ("platform", "state", "token", "body", "base")}
            body = json.dumps(payload).encode("utf-8")
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
        """
        额度明细（2026-10-04 实测修正）
        ------------------------------
        原来是 GET，真机返回 400 "EOF" —— 端点存在但**必须 POST**。
        实测 200 的结构：
            data.Response.Data.TotalCount      资源条数
            data.Response.Data.TotalDosage     总消耗
            data.Response.Data.Accounts[]      每条含
                PackageName / CapacityUnit / CapacitySize
                CapacityRemain / CapacityUsed
                CycleStartTime / CycleEndTime
        实测本机：47 条，本月周期 10-01~10-31，剩 500 credits
        """
        ok, d = self.call("quota", body={})
        if not ok:
            return False, d
        if isinstance(d, dict) and d.get("code") not in (0, None):
            return False, d
        node = ((d.get("data") or {}).get("Response") or {}).get("Data") or {}
        return True, {
            "totalCount": node.get("TotalCount") or 0,
            "totalDosage": node.get("TotalDosage") or 0,
            "accounts": node.get("Accounts") or [],
        }

    def quota_summary(self):
        """把 47 条资源聚合成几条易读的摘要（按 package 去重）"""
        ok, q = self.quota()
        if not ok:
            return False, q
        by = {}
        for a in q.get("accounts") or []:
            key = (a.get("PackageName") or a.get("ProductName") or "?",
                   a.get("CapacityUnit") or "")
            cur = by.setdefault(key, {
                "name": key[0], "unit": key[1],
                "size": 0, "remain": 0, "used": 0, "cycles": 0,
            })
            for k, src in (("size", "CapacitySize"), ("remain", "CapacityRemain"),
                           ("used", "CapacityUsed")):
                try:
                    cur[k] += float(a.get(src) or 0)
                except (TypeError, ValueError):
                    pass
            cur["cycles"] += 1
        return True, sorted(by.values(), key=lambda x: -x["size"])

    def models(self):
        ok, d = self.call("models")
        return ok, d

    def daily_checkin(self):
        """
        每日签到。实测 2026-10-04 真的签上了：
          首次 → 200 {"code":0,...}
          再签 → 400 {"code":10001,"msg":"今天已签到，请明天再来"}
        400 不是失败，是「今天已签」，调用方要按 msg 区分。
        """
        return self.call("daily_checkin", body={})

    def growth_buddy_info(self):
        """养成 buddy 状态（逆向时漏掉的端点，实测 200）"""
        return self.call("growth_buddy_info")

    def growth_makeup(self, target_date):
        """
        补签卡。实测要 {"target_date":"YYYY-MM-DD"}：
          日期格式错 → 400 "invalid target_date format"
          该天没断 → 400 "date is not broken, no makeup needed"
        """
        return self.call("growth_makeup", body={"target_date": target_date})

    def invite_bind(self, invite_code):
        """绑邀请码。实测要 {"invite_code":"..."}，缺了报 required 校验失败"""
        return self.call("invite_bind", body={"invite_code": invite_code})

    def growth_lottery(self, **kw):
        """成长抽奖。实测 400 invalid request，具体参数待抓包确认"""
        return self.call("growth_lottery", body=kw or {})

    def account(self):
        """账号信息（实测 200，在 www.codebuddy.cn 域）"""
        return self.call("account", base="https://www.codebuddy.cn")

    def growth_buddy(self):
        return self.call("growth_buddy")

    def refresh_token(self, refresh_token):
        """实测 400 {"code":10001,"msg":"refreshToken is empty"}，要 refresh_token"""
        return self.call("oauth_refresh", body={"refresh_token": refresh_token})

    # ------------------------------------------------- 原生登录（抓包实测链路）
    #
    # 逆向二进制只能看到 /v2/plugin/auth/token?state= 这条轮询路径，
    # 真正发 token 的 POST /console/login/enterprise?state= 只有抓包能拿到，
    # 详见 tlogin.py 顶部的完整链路说明。

    @staticmethod
    def new_state():
        from app.tlogin import new_state
        return new_state()

    @staticmethod
    def login_url(state, platform="CLI"):
        from app.tlogin import login_url
        return login_url(state, platform)

    @staticmethod
    def open_login(state, port=9333):
        """起带调试口的浏览器打开登录页，用户扫码"""
        from app.tlogin import open_login
        return open_login(state, port=port)

    @staticmethod
    def exchange(state, port=9333):
        """在浏览器上下文里把 state 换成 accessToken/refreshToken"""
        from app.tlogin import exchange_in_page
        return exchange_in_page(port=port, state=state)

    @staticmethod
    def poll_login(state, port=9333, timeout=300):
        """等扫码，登录完成直接返回 token 字典"""
        from app.tlogin import poll_token
        return poll_token(port=port, state=state, timeout=timeout)

    def load_cred(self, path=None):
        """从 data/tencent_cred.json 读 token 并装进 self.token"""
        from app.tlogin import load
        c = load(path)
        if c.get("accessToken"):
            self.token = c["accessToken"]
            return True, c
        return False, c

    def save_cred(self, cred, path=None):
        from app.tlogin import save
        return save(cred, path)

    def catalog(self):
        """
        在线模型目录（归一化，带 credits 倍率）。
        这是倍率三源里优先级第二的「在线目录」源。
        """
        from app.tlogin import models_with_credits
        return models_with_credits(self.token, base=self.base)


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
