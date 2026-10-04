# -*- coding: utf-8 -*-
"""
7 个本地网关的「账号登录 / 获取凭据」驱动
=========================================
这是面板真正完成「集成」的核心：以前只能让用户去命令行跑
`workbuddy-gateway login` 或者手工导入文件，现在面板自己把登录跑完。

三类登录方式
------------
qrcode   网关二维码登录。实测 v1.29.6 契约：
           POST /admin/api/login/start {"edition":"cn"|"intl"}
             → {id, edition, siteLabel, qr(data:image/png;base64,...),
                authUrl, status, secondsLeft, expiresAt}
           GET  /admin/api/login/poll?id=<id>   （**仅 GET**，POST 会 405）
             → 同上结构，status: pending → success / expired
         面板负责：展示二维码 + 提供 authUrl 一键跳转 + 轮询到 success。

cookie   Cookie 类平台（豆包 / 元宝 / 小浣熊）。两条取 Cookie 的路：
           - CDP：面板开一个带调试端口的浏览器，用户登录后由浏览器
                  把**已解密**的 Cookie 交出来（Chrome 127+ 磁盘加密已封死）
           - 直读：老版本 Chromium（v10/v11）直接解磁盘 Cookie
           - 手动粘贴：永远可用的兜底

file     凭据文件导入（Go 原生网关等）。保留，不作为唯一手段。

会话模型
--------
LoginSession 是状态机，前端用 id 轮询：
  pending → waiting（等用户操作）→ success / error / expired
"""

import json
import os
import threading
import time
import urllib.parse
import uuid

# ------------------------------------------------------------------ 平台规格

# platform 与 upstreams.GATEWAYS 的 id 对齐，另加若干「纯上游平台」
PLATFORMS = {
    "wb-gateway": {
        "name": "WorkBuddy 网关 · 国内站",
        "method": "qrcode", "edition": "cn",
        "gateway": True,
        "upstream": "copilot.tencent.com",
        "hint": "扫码或点开授权链接，登录 WorkBuddy / CodeBuddy 国内账号",
    },
    "wb-gateway-intl": {
        "name": "WorkBuddy 网关 · 国际站",
        "method": "qrcode", "edition": "intl",
        "gateway": True,
        "upstream": "copilot.tencent.com",
        "hint": "国际站账号，走 copilot.tencent.com 国际授权",
    },
    "apk-trae": {
        "name": "Trae / aigw.app",
        "method": "qrcode", "edition": "cn",
        "gateway": False,
        "upstream": "api.trae.cn",
        "hint": "Trae 走同一套 copilot 授权，登录后可直连 api.trae.cn",
        "verify": {"url": "https://api.trae.cn/api/v3/trae/GetUserInfo",
                   "type": "bearer", "ok_keys": ["user_id", "user", "data"]},
    },
    "apk-codebuddy": {
        "name": "CodeBuddy 国际版",
        "method": "qrcode", "edition": "intl",
        "gateway": False,
        "upstream": "www.codebuddy.ai",
        "hint": "CodeBuddy 国际站账号（OAuthWebActivity 对应的网页授权）",
    },
    "apk-doubao": {
        "name": "豆包 dev.doubao2api",
        "method": "cookie",
        "hosts": ["doubao.com"],
        "login_url": "https://www.doubao.com/",
        "gateway": False,
        "upstream": "www.doubao.com",
        "hint": "在打开的浏览器里登录豆包，面板自动取 Cookie（也支持手动粘贴）",
        "verify": {"url": "https://www.doubao.com/api/v1/user/info",
                   "type": "cookie"},
    },
    "apk-yuanbao": {
        "name": "元宝 dev.yuanbao2api",
        "method": "cookie",
        "hosts": ["yuanbao.tencent.com"],
        "login_url": "https://yuanbao.tencent.com/",
        "gateway": False,
        "upstream": "yuanbao.tencent.com",
        "hint": "⚠ 2026-10-04 审计：/api/models 端点**确实存在**"
                "（同站基准路径返回 404，它返回 401）。但元宝要的是 hy_token / hy_user / "
                "uskey 这类 token，CDP 通常只扫到 qimei 开头的**设备 ID** —— "
                "光有设备 ID 取不到数据。模型列表会自动退回 APK 内置的 12 个。",
        "verify": {"url": "https://yuanbao.tencent.com/api/getuserinfo",
                   "type": "cookie", "ok_keys": ["user", "data", "userId"]},
    },
    "apk-raccoon": {
        "name": "小浣熊 dev.raccoon2api",
        "method": "cookie",
        "hosts": ["xiaohuanxiong.com"],
        "login_url": "https://xiaohuanxiong.com/",
        "gateway": False,
        "upstream": "xiaohuanxiong.com",
        "hint": "登录小浣熊；登录后可调「登录送积分」接口领积分",
        "bonus": {"url": "https://xiaohuanxiong.com/api/web/desktop/v1/login/points/grant",
                  "method": "POST"},
    },
    "apk-go": {
        "name": "Go 原生网关 com.joy4fire.wb2apimobile",
        "method": "file",
        "gateway": False,
        "upstream": "copilot.tencent.com",
        "hint": "Go 版网关未提供网页授权入口，支持导入凭据文件 / 直接填 API Key",
    },
    # ↓ 以下三个是从 base.apk（aigw.app）里挖出来的多供应商反代
    # APK 顶部串明写："Trae / Loomy / WorkBuddy / Antigravity"
    "apk-loomy": {
        "name": "Loomy 讯飞（loomyad.xunfei.cn）",
        "method": "cookie",
        "hosts": ["xunfei.cn", "xfinfr.com"],
        "login_url": "https://account.xfinfr.com/",
        "gateway": False,
        "upstream": "loomyad.xunfei.cn",
        "hint": "讯飞 Loomy。登录 account.xfinfr.com，面板取 Cookie 后调 "
                "/api/v1/models（实测返回「缺少 token」说明接口通）",
        "verify": {"url": "https://loomyad.xunfei.cn/api/v1/models",
                   "type": "cookie"},
    },
    "apk-antigravity": {
        "name": "Antigravity / Google Cloud Code",
        "method": "cookie",
        "hosts": ["google.com", "googleapis.com"],
        "login_url": "https://accounts.google.com/",
        "gateway": False,
        "upstream": "cloudcode-pa.googleapis.com",
        "hint": "Google 官方 Antigravity（APK 内 UA antigravity/1.15.8，"
                "pluginType=GEMINI）。需 Google 账号，取 oauth2 token 或 Cookie",
        "verify": {"url": "https://cloudcode-pa.googleapis.com/v1internal:listModels",
                   "type": "bearer"},
    },
    "apk-codebuddy-cn": {
        "name": "CodeBuddy 国内站（www.codebuddy.cn）",
        "method": "qrcode", "edition": "cn",
        "gateway": False,
        "upstream": "www.codebuddy.cn",
        "hint": "CodeBuddy 国内站，与国际站 www.codebuddy.ai 是两套"
                "（实测国内站的模型目录与积分接口都通）",
        "verify": {"url": "https://www.codebuddy.cn/v2/billing/meter/get-user-resource",
                   "type": "bearer"},
    },
    # ==================== 办公 AI / Agent 平台（2026-10-04 调研） ====================
    "apk-coze": {
        "name": "扣子 Coze（api.coze.cn 官方 API）",
        # ★ 2026-10-04 审计修正：官方 API 用的是**控制台签发的 PAT**（pat_ 开头），
        #   不是浏览器 Cookie。之前配成 cookie 方式，扫出来的浏览器 Cookie 打过去
        #   必然 401。PAT 在 https://www.coze.cn → 个人中心 → 访问令牌 生成。
        "method": "file",
        "type": "api_key",
        "placeholder": "pat_xxxxxxxxxxxx",
        "hosts": ["coze.cn"],
        "login_url": "https://www.coze.cn/",
        "gateway": False,
        "upstream": "api.coze.cn",
        "hint": "唯一有完整开放 API 的国内 Agent 平台。在扣子控制台「个人访问令牌」"
                "生成 pat_ 开头的 PAT，粘到下面即可（浏览器 Cookie 会被 401 拒绝）",
        "verify": {"url": "https://api.coze.cn/v3/chat/retrieve",
                   "type": "bearer"},
    },
    "apk-qwenwork": {
        "name": "千问办公 QwenWork（阿里）",
        "method": "cookie",
        "hosts": ["qwenwork.cn", "aliyun.com"],
        "login_url": "https://qwenwork.cn/",
        "gateway": False,
        "upstream": "qwenwork.cn",
        "hint": "每日登录奖 100 积分（新用户 2000，有效期 3 个月）。"
                "接口是整站鉴权，存好凭据后用「探测端点」扫真实路径。"
                "注意：没有第三方反代，别找反代项目浪费时间",
        "verify": None,
    },
    "apk-kuku": {
        "name": "库库 AI（百度文库+网盘）",
        "method": "cookie",
        "hosts": ["baidu.com", "kuku.baidu.com"],
        "login_url": "https://kuku.baidu.com/",
        "gateway": False,
        "upstream": "kuku.baidu.com",
        "hint": "每日登录 +50、完成一次对话 +50、邀请新用户最高 +450；"
                "连续签到 3/5/7 天各再送百度网盘 SVIP。1 积分 ≈ 1 万 Token。"
                "活动期制，规则可能变",
        "verify": None,
    },
    "apk-wps": {
        "name": "WPS AI（金山办公）",
        "method": "cookie",
        "hosts": ["wps.cn", "kdocs.cn"],
        "login_url": "https://www.wps.cn/",
        "gateway": False,
        "upstream": "www.wps.cn",
        "hint": "⚠ WPS 是「积分 / 灵犀智点」双轨制，别把两者当同一个。"
                "端点需登录后探测",
        "verify": None,
    },
    "apk-nano": {
        "name": "纳米 AI（360）",
        "method": "cookie",
        "hosts": ["n.cn", "360.cn"],
        "login_url": "https://www.n.cn/",
        "gateway": False,
        "upstream": "www.n.cn",
        "hint": "360 旗下 AI 搜索/办公。端点需登录后探测",
        "verify": None,
    },
    "apk-qoder": {
        "name": "Qoder（阿里，原通义灵码国际版）",
        "method": "cookie",
        "hosts": ["qoder.com", "lingma.aliyun.com"],
        "login_url": "https://www.qoder.com/",
        "gateway": False,
        "upstream": "www.qoder.com",
        "hint": "★ 每日 10:00 领 100 Credits（30 天有效、错过不可补）；"
                "新邮箱+邀请码 600 Credits。官方文档写明「仅桌面端主动领取」，"
                "未公布 API —— 用「探测端点」看能否脚本化。"
                "CLIProxyAPI 已原生支持 Qoder OAuth",
        "verify": None,
    },
    "apk-metaso": {
        "name": "秘塔 AI（metaso.cn）",
        "method": "cookie",
        "hosts": ["metaso.cn"],
        "login_url": "https://metaso.cn/",
        "gateway": False,
        "upstream": "metaso.cn",
        "hint": "唯一有官方 Open API + MCP 的国内办公 AI（¥0.03/次）。"
                "它的积分是「每日自动刷新到账」，**不需要签到** —— "
                "别给它做签到按钮",
        "verify": None,
    },
}

# 二维码登录的终态
_QR_DONE = {"success", "failed", "expired", "error"}
_QR_FAIL = {"failed", "expired", "error"}


def platform_list():
    out = []
    for pid, p in PLATFORMS.items():
        out.append({
            "id": pid, "name": p["name"], "method": p["method"],
            "edition": p.get("edition", ""),
            "hosts": p.get("hosts", []),
            "login_url": p.get("login_url", ""),
            "upstream": p.get("upstream", ""),
            "hint": p.get("hint", ""),
            "gateway": bool(p.get("gateway")),
        })
    return out


# ------------------------------------------------------------------ 会话

class LoginSession:
    def __init__(self, platform, method, **opts):
        self.id = uuid.uuid4().hex[:16]
        self.platform = platform
        self.method = method
        self.opts = opts or {}
        self.status = "pending"        # pending/waiting/success/error/expired
        self.message = ""
        self.error = ""
        self.qr = ""                   # data:image/png;base64,...
        self.auth_url = ""
        self.expires_at = 0
        self.result = None             # {"type":..., "secret":..., "account":...}
        self.created_at = time.time()
        self._browser = None
        self._stop = False
        self.lock = threading.RLock()

    def to_dict(self):
        with self.lock:
            return {
                "id": self.id, "platform": self.platform,
                "platform_name": PLATFORMS.get(self.platform, {}).get("name", self.platform),
                "method": self.method, "status": self.status,
                "message": self.message, "error": self.error,
                "qr": self.qr, "auth_url": self.auth_url,
                "expires_at": self.expires_at,
                "seconds_left": max(0, int(self.expires_at - time.time())) if self.expires_at else 0,
                "result": _mask(self.result), "created_at": self.created_at,
            }

    def finish(self, status, message="", error="", result=None):
        with self.lock:
            self.status = status
            self.message = message
            self.error = error
            if result is not None:
                self.result = result


def _mask(res):
    """对外隐藏完整密钥，只留可辨识的指纹"""
    if not isinstance(res, dict):
        return res
    out = dict(res)
    for k in ("secret", "token", "cookie", "api_key", "password"):
        v = out.get(k)
        if isinstance(v, str) and len(v) > 12:
            out[k] = v[:6] + "…" + v[-4:]
            out[k + "_full_len"] = len(v)
    return out


# ------------------------------------------------------------------ 管理器

class LoginManager:
    """
    驱动各平台登录。client_factory 返回已登录控制台的 GatewayClient。
    """
    # 会话保留时间
    TTL = 1800

    def __init__(self, client_factory, accounts=None):
        self._client_factory = client_factory
        self.accounts = accounts
        self.sessions = {}
        self.lock = threading.RLock()

    # ---------------------------------------------------------- 生命周期
    def start(self, platform, **opts):
        spec = PLATFORMS.get(platform)
        if not spec:
            raise KeyError("未知平台：%s" % platform)
        sess = LoginSession(platform, spec["method"], **opts)
        with self.lock:
            self.sessions[sess.id] = sess
        t = threading.Thread(target=self._run, args=(sess, spec), daemon=True)
        t.start()
        # 给线程一点时间把二维码/URL 填好
        for _ in range(40):
            time.sleep(0.05)
            if sess.status in ("waiting", "success", "error", "expired") or sess.qr or sess.auth_url:
                break
        return sess

    def get(self, sid):
        with self.lock:
            return self.sessions.get(sid)

    def poll(self, sid):
        """前端轮询入口：二维码类在这里真正向网关查询状态"""
        sess = self.get(sid)
        if not sess:
            return None
        if sess.method == "qrcode" and sess.status in ("pending", "waiting"):
            self._poll_qrcode(sess)
        elif sess.method == "cookie" and sess.status == "waiting":
            self._poll_cookie(sess, PLATFORMS.get(sess.platform, {}))
        self._gc()
        return sess

    def cancel(self, sid):
        sess = self.get(sid)
        if not sess:
            return False
        sess._stop = True
        if sess._browser:
            try:
                sess._browser.close()
            except Exception:
                pass
        sess.finish("error", "已取消", "cancelled")
        return True

    def _gc(self):
        now = time.time()
        with self.lock:
            dead = [k for k, v in self.sessions.items()
                    if now - v.created_at > self.TTL or v._stop]
            for k in dead:
                v = self.sessions.pop(k, None)
                if v and v._browser:
                    try:
                        v._browser.close()
                    except Exception:
                        pass

    # ---------------------------------------------------------- 分派
    def _run(self, sess, spec):
        try:
            if sess.method == "qrcode":
                self._start_qrcode(sess, spec)
            elif sess.method == "cookie":
                self._start_cookie(sess, spec)
            else:
                self._start_file(sess, spec)
        except Exception as e:
            sess.finish("error", "启动登录失败", "%s: %s" % (type(e).__name__, e))

    # ---------------------------------------------------------- 二维码
    def _start_qrcode(self, sess, spec):
        c = self._client_factory()
        edition = sess.opts.get("edition") or spec.get("edition") or "cn"
        r = c.start_login(edition)
        if not isinstance(r, dict) or not r.get("id"):
            sess.finish("error", "网关未返回登录会话",
                        "响应=%s" % json.dumps(r, ensure_ascii=False)[:300])
            return
        with sess.lock:
            sess.opts["gw_session"] = r["id"]
            sess.qr = r.get("qr") or ""
            sess.auth_url = r.get("authUrl") or ""
            sess.expires_at = r.get("expiresAt") or (time.time() + (r.get("secondsLeft") or 300))
        sess.finish("waiting",
                    "请用 %s App 扫码，或点击「打开授权页」在浏览器完成登录" % r.get("siteLabel", "官方"),
                    result=None)
        self._poll_qrcode(sess)

    def _poll_qrcode(self, sess):
        c = self._client_factory()
        sid = sess.opts.get("gw_session")
        if not sid:
            sess.finish("error", "缺少登录会话 id", "no_session")
            return
        try:
            r = c.poll_login(sid)
        except Exception as e:
            sess.finish("error", "轮询登录状态失败", "%s: %s" % (type(e).__name__, e))
            return
        if not isinstance(r, dict):
            sess.finish("error", "网关返回了非预期的响应", str(r)[:200])
            return
        st = (r.get("status") or "").lower()
        with sess.lock:
            if r.get("qr"):
                sess.qr = r["qr"]
            if r.get("authUrl"):
                sess.auth_url = r["authUrl"]
            if r.get("expiresAt"):
                sess.expires_at = r["expiresAt"]
        if st in ("success", "ok", "done"):
            acct = (r.get("account") or r.get("user") or r.get("email")
                    or r.get("username") or "")
            sess.finish("success", "登录成功，凭据已写入网关",
                        result={"type": "gateway", "secret": "", "account": acct,
                                "edition": r.get("edition", "")})
            self._save(sess)
        elif st in _QR_FAIL:
            sess.finish("error", r.get("message") or "登录失败", st)
        else:
            with sess.lock:
                left = ""
                if r.get("secondsLeft"):
                    left = "（剩余 %d 秒）" % r["secondsLeft"]
                sess.status = "waiting"
                sess.message = (r.get("message") or "等待授权") + left
            if sess.expires_at and time.time() > sess.expires_at:
                sess.finish("expired", "二维码已过期，请重新开始", "expired")

    # ---------------------------------------------------------- Cookie
    def _start_cookie(self, sess, spec):
        hosts = sess.opts.get("hosts") or spec.get("hosts") or []
        if sess.opts.get("manual_cookie"):
            self._finish_cookie(sess, spec, sess.opts["manual_cookie"], "手动粘贴")
            return
        # 先试直读本机浏览器（老版本 Chromium 才有效）
        try:
            from . import browser_cookie
            got = browser_cookie.fetch(hosts)
            if got.get("ok") and got.get("cookie"):
                self._finish_cookie(sess, spec, got["cookie"],
                                    "本机浏览器（%s）" % got.get("source", ""))
                return
        except Exception as e:
            sess.message = "直读浏览器失败：%s" % e
        # 再走 CDP：开一个调试浏览器让用户在里面登录
        try:
            from . import cdp
        except Exception as e:
            sess.finish("error", "CDP 模块不可用", str(e))
            return
        port = int(sess.opts.get("port") or 9333)
        b = cdp.Browser(port=port)
        try:
            b.start(url=spec.get("login_url") or ("https://" + (hosts[0] if hosts else "")))
        except Exception as e:
            sess.finish("error", "启动浏览器失败",
                        "%s。可改用「手动粘贴 Cookie」" % e)
            return
        sess._browser = b
        sess.finish("waiting",
                    "已打开浏览器，请在其中完成登录；登录后点「我已登录，立即获取」",
                    result=None)

    def _poll_cookie(self, sess, spec):
        hosts = sess.opts.get("hosts") or spec.get("hosts") or []
        b = sess._browser
        if not b:
            sess.finish("error", "浏览器会话已丢失", "no_browser")
            return
        try:
            header, rows = b.cookie_header(hosts)
        except Exception as e:
            sess.finish("error", "读取 Cookie 失败", str(e))
            return
        if not header:
            with sess.lock:
                sess.message = "未检测到 %s 的登录 Cookie，请先在浏览器里登录" % "/".join(hosts)
            return
        self._finish_cookie(sess, spec, header, "CDP 浏览器")

    def _finish_cookie(self, sess, spec, cookie, source=""):
        okk, info = verify_cookie(spec, cookie)
        if not okk:
            sess.finish("error", "Cookie 已取得但未通过登录校验", info)
            return
        sess.finish("success", "登录成功（来源：%s）" % (source or "未知"),
                    result={"type": "cookie", "secret": cookie, "account": info,
                            "source": source})
        self._save(sess)

    # ---------------------------------------------------------- 文件 / Key
    def _start_file(self, sess, spec):
        payload = sess.opts.get("secret") or sess.opts.get("file_content") or ""
        if not payload:
            sess.finish("waiting",
                        "请粘贴凭据文件内容或 API Key，然后提交",
                        result=None)
            return
        sess.finish("success", "已保存凭据",
                    result={"type": sess.opts.get("type", "api_key"),
                            "secret": payload, "account": sess.opts.get("account", "")})
        self._save(sess)

    def submit(self, sid, **kv):
        """Cookie 手动提交 / 文件类提交后触发"""
        sess = self.get(sid)
        if not sess:
            return None
        spec = PLATFORMS.get(sess.platform, {})
        if sess.method == "cookie":
            ck = kv.get("cookie") or ""
            if not ck:
                sess.finish("error", "Cookie 不能为空", "empty")
                return sess
            self._finish_cookie(sess, spec, ck, "手动粘贴")
        else:
            secret = kv.get("secret") or kv.get("api_key") or ""
            if not secret:
                sess.finish("error", "内容不能为空", "empty")
                return sess
            sess.finish("success", "已保存凭据",
                        result={"type": kv.get("type", "api_key"),
                                "secret": secret, "account": kv.get("account", "")})
            self._save(sess)
        return sess

    # ---------------------------------------------------------- 落库
    def _save(self, sess):
        if not self.accounts or not sess.result:
            return
        try:
            self.accounts.add({
                "platform": sess.platform,
                "name": sess.result.get("account") or PLATFORMS.get(
                    sess.platform, {}).get("name", sess.platform),
                "type": sess.result.get("type", ""),
                "secret": sess.result.get("secret", ""),
                "source": sess.result.get("source", "面板登录"),
                "edition": sess.result.get("edition", ""),
            })
        except Exception:
            pass


# ------------------------------------------------------------------ 校验

def verify_cookie(spec, cookie):
    """
    用 Cookie 调一次用户信息接口，确认登录态有效。
    返回 (bool, 说明)。无法联网或平台未提供校验接口时返回 (True, "未校验")，
    避免因网络问题把可用的 Cookie 判死。
    """
    v = spec.get("verify")
    if not v or not cookie:
        return True, "未校验"
    import ssl
    import urllib.error
    import urllib.request
    url = v.get("url")
    if not url:
        return True, "未校验"
    req = urllib.request.Request(url, headers={
        "Cookie": cookie,
        "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                      "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
        "Accept": "application/json",
    })
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    try:
        with urllib.request.urlopen(req, timeout=12, context=ctx) as r:
            raw = r.read().decode("utf-8", "replace")
            code = r.status
    except urllib.error.HTTPError as e:
        # 401/403 说明 Cookie 无效；其它（404/405）视为接口形态变化，不判死
        if e.code in (401, 403):
            return False, "接口返回 %d，Cookie 无效或已过期" % e.code
        return True, "校验接口返回 %d（未判定为无效）" % e.code
    except Exception as e:
        return True, "校验请求未完成（%s），按可用处理" % type(e).__name__

    if code >= 400:
        return False, "接口返回 %d" % code
    try:
        d = json.loads(raw)
    except json.JSONDecodeError:
        return True, "接口返回非 JSON，按可用处理"
    keys = v.get("ok_keys") or []
    if keys and not any(k in d for k in keys):
        # 返回了合法 JSON 但不含用户字段 → 多半是未登录页
        return False, "响应中未找到用户字段，可能未登录"
    for k in ("nickname", "name", "user_name", "username", "email", "user_id", "userId"):
        val = _dig(d, k)
        if val:
            return True, str(val)
    return True, "已登录"


def _dig(d, key, depth=0):
    if depth > 4 or not isinstance(d, dict):
        return ""
    if key in d and isinstance(d[key], (str, int)):
        return str(d[key])
    for v in d.values():
        if isinstance(v, dict):
            r = _dig(v, key, depth + 1)
            if r:
                return r
    return ""
