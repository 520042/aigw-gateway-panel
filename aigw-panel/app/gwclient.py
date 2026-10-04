# -*- coding: utf-8 -*-
"""
WorkBuddy Gateway 客户端
=========================
对接 workbuddy-gateway v1.29.x 的 admin API。

鉴权契约（实测 v1.29.6）：
  1. 首次使用需 POST /admin/api/setup  {"username","password"}
  2. 登录 POST /admin/api/login/key {"username","password"}  → 下发 Cookie 会话
  3. 之后所有 /admin/api/* 带 Cookie 即可；未登录返回 {"error":{"code":"login_required"}}
  4. /v1/* 用 -api-key 的 Bearer 鉴权（默认 admin）

全部走标准库 urllib，不引入第三方依赖，保证 PyInstaller 单文件打包干净。
"""

import json
import ssl
import threading
import urllib.error
import urllib.parse
import urllib.request
from http.cookiejar import CookieJar

TIMEOUT = 15


class GatewayError(Exception):
    def __init__(self, code, message, payload=None):
        super().__init__("%s: %s" % (code, message))
        self.code = code
        self.message = message
        self.payload = payload or {}


class GatewayClient:
    def __init__(self, addr="127.0.0.1", port=8317, api_key="admin",
                 admin_user="admin", admin_password=""):
        self.addr = addr
        self.port = int(port)
        self.api_key = api_key or "admin"
        self.admin_user = admin_user or "admin"
        self.admin_password = admin_password or ""
        self._lock = threading.RLock()
        self._jar = CookieJar()
        self._opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self._jar),
            urllib.request.HTTPSHandler(context=ssl.create_default_context()),
        )
        self._logged_in = False

    # ------------------------------------------------------------ 基础
    @property
    def base(self):
        return "http://%s:%d" % (self.addr, self.port)

    def reconfigure(self, **kw):
        with self._lock:
            for k in ("addr", "port", "api_key", "admin_user", "admin_password"):
                if k in kw and kw[k] not in (None, ""):
                    setattr(self, k, kw[k])
            self._logged_in = False

    def _request(self, path, method="GET", payload=None, auth="cookie",
                 retries=1, timeout=None):
        url = self.base + path
        body = None
        headers = {"Accept": "application/json"}
        if payload is not None:
            body = json.dumps(payload).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if auth == "bearer":
            headers["Authorization"] = "Bearer " + self.api_key
        req = urllib.request.Request(url, data=body, headers=headers, method=method)
        tmo = timeout or TIMEOUT

        last_err = None
        for attempt in range(retries + 1):
            try:
                with self._opener.open(req, timeout=tmo) as resp:
                    raw = resp.read().decode("utf-8", "replace")
                    return self._parse(raw, resp.status)
            except urllib.error.HTTPError as e:
                raw = e.read().decode("utf-8", "replace")
                try:
                    data = self._parse(raw, e.code)
                except GatewayError:
                    raise
                # 会话过期 → 自动重登一次再试
                err = (data.get("error") or {}) if isinstance(data, dict) else {}
                if (err.get("code") == "login_required" and auth == "cookie"
                        and self.admin_password and attempt < retries):
                    if self.login():
                        req = urllib.request.Request(url, data=body, headers=headers, method=method)
                        continue
                if err.get("code"):
                    raise GatewayError(err.get("code"), err.get("message"), data)
                return data
            except urllib.error.URLError as e:
                last_err = GatewayError("unreachable",
                                        "无法连接网关 %s（%s）" % (self.base, e.reason))
            except Exception as e:
                last_err = GatewayError("internal", str(e))
            if attempt < retries:
                continue
        raise last_err

    @staticmethod
    def _parse(raw, status):
        if not raw.strip():
            return {"ok": True, "status": status}
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return {"ok": False, "status": status, "raw": raw[:2000]}

    # ------------------------------------------------------------ 鉴权
    def setup(self, username, password):
        return self._request("/admin/api/setup", "POST",
                             {"username": username, "password": password})

    def login(self):
        with self._lock:
            if not self.admin_password:
                return False
            self._jar.clear()
            r = self._request("/admin/api/login/key", "POST",
                              {"username": self.admin_user, "password": self.admin_password},
                              retries=0)
            self._logged_in = bool(r.get("ok"))
            return self._logged_in

    def login_info(self):
        return self._request("/admin/api/login/info", auth="none" if False else "cookie")

    # ---------------------------------------------------- 账号登录（二维码）
    def start_login(self, edition="cn"):
        """
        发起二维码登录会话。实测 v1.29.6 返回：
          {id, edition, siteLabel, status, message,
           qr(data:image/png;base64,...), authUrl, startedAt, expiresAt, secondsLeft}
        """
        return self._request("/admin/api/login/start", "POST",
                             {"edition": edition or "cn"})

    def poll_login(self, session_id):
        """轮询登录状态。**仅支持 GET**，POST 会返回 405"""
        return self._request(
            "/admin/api/login/poll?id=" + urllib.parse.quote(str(session_id)),
            "GET")

    def import_login(self, payload, name=None):
        """导入凭据文件内容（保留入口，不作唯一手段）"""
        body = {"content": payload}
        if name:
            body["name"] = name
        return self._request("/admin/api/login/import", "POST", body)

    def active_login_sessions(self):
        """列出网关上仍在进行中的登录会话"""
        try:
            return self._request("/admin/api/login/sessions")
        except GatewayError:
            return {"sessions": []}

    def logout(self):
        with self._lock:
            self._logged_in = False
            self._jar.clear()
            return {"ok": True}

    def needs_setup(self):
        try:
            r = self._request("/admin/api/status", retries=0)
            err = (r.get("error") or {}) if isinstance(r, dict) else {}
            return err.get("code") == "setup_required"
        except GatewayError as e:
            return e.code == "setup_required"

    def ping(self):
        """轻量存活探测：/v1/models 用 Bearer，不受控制台会话影响"""
        try:
            r = self._request("/v1/models", auth="bearer", retries=0)
            return {"alive": True, "models": len(r.get("data") or [])}
        except GatewayError as e:
            return {"alive": False, "error": e.message, "code": e.code}

    # ------------------------------------------------------------ 读接口
    def status(self):
        return self._request("/admin/api/status")

    def settings(self):
        return self._request("/admin/api/settings")

    def models(self):
        return self._request("/admin/api/models")

    def v1_models(self):
        return self._request("/v1/models", auth="bearer")

    def raw_chat(self, payload):
        """
        裸转发 /v1/chat/completions（不过滤字段）。
        工具调用测试台要用：tools / tool_choice / parallel_tool_calls
        这些参数必须原样送到上游。
        """
        return self._request("/v1/chat/completions", "POST", payload,
                             auth="bearer", retries=0, timeout=120)

    def credentials(self):
        return self._request("/admin/api/credentials")

    def checkins(self):
        return self._request("/admin/api/checkins")

    def growth(self):
        return self._request("/admin/api/growth")

    def growth_report(self):
        return self._request("/admin/api/growth/report")

    def usage(self, rng="all"):
        return self._request("/admin/api/usage?range=" + urllib.parse.quote(rng))

    def usage_series(self, rng="24h"):
        return self._request("/admin/api/usage/series?range=" + urllib.parse.quote(rng))

    def apikeys(self):
        return self._request("/admin/api/apikeys")

    def webhooks(self):
        return self._request("/admin/api/webhooks")

    def logs(self, limit=200):
        return self._request("/admin/api/logs?limit=%d" % int(limit))

    def pricing(self):
        """
        模型价格表。实测 v1.29.6 **没有** /admin/api/pricing（返回 404），
        成本信息实际挂在 /admin/api/models 的 cost 字段（调用后才观测）。
        这里保留方法但返回空表，不再让前端吃到 404。
        """
        return {"pricing": {}, "note": "网关未提供 pricing 端点；"
                "成本请看 models 接口的 cost 字段（需先产生调用）"}

    # ------------------------------------------------------------ 写接口
    def do_checkin(self):
        """手动触发签到（网关自身账号池）"""
        return self._request("/admin/api/checkins/run", "POST", {})

    def run_growth(self):
        """手动触发成长任务全量执行"""
        return self._request("/admin/api/growth/run", "POST", {})

    def lottery(self):
        return self._request("/admin/api/growth/lottery", "POST", {})

    def redeem(self, code):
        return self._request("/admin/api/growth/redeem", "POST", {"code": code})

    def bonus(self):
        return self._request("/admin/api/growth/bonus", "POST", {})

    def makeup(self):
        return self._request("/admin/api/growth/makeup", "POST", {})

    def travel(self):
        return self._request("/admin/api/growth/travel", "POST", {})

    def probe_models(self, models=None, limit=5):
        payload = {"limit": limit}
        if models:
            payload["models"] = models
        return self._request("/admin/api/models/probe", "POST", payload)

    def delete_credential(self, name):
        return self._request("/admin/api/credentials/delete", "POST", {"name": name})

    def update_settings(self, patch):
        return self._request("/admin/api/settings", "POST", patch)
