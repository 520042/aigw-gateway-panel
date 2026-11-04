# -*- coding: utf-8 -*-
"""
服务端远程扫码登录子系统（无需本机浏览器 / CDP）。

思路：直接打各 AI 平台的「网页二维码登录」Web 接口
  init()   拿 ticket + 二维码（图片 data-uri 或待编码文本）
  poll()   轮询登录态，确认后抽取会话 Cookie / Token
框架负责起后台轮询线程、超时、把确认后的凭据交给 verify_cookie 校验并落库。

已落地适配器（有开源逆向 / 官方 Device Flow 支撑）：
  apk-doubao      豆包（字节 passport 二维码）
  apk-yuanbao     元宝（微信 qrconnect + joint/login）
  apk-kuku        百度文库 / 库库（百度 passport 二维码）
  apk-antigravity Google（OAuth2 Device Flow，需自备 client_id）

其余 cookie 类平台（WPS / 秘塔 / 阿里通义 / 360 / 讯飞 / 小浣熊）公开无可用
Web 扫码端点，自动降级到「手动粘贴 Cookie」或「本机 CDP 扫码」，不在此子系统内。

所有网络调用都包了异常，单平台失败只会影响它自己，不会拖垮面板。
"""

import base64
import http.cookiejar
import json
import os
import re
import ssl
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36")


def _ctx():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _http(method, url, headers=None, data=None, timeout=15, jar=None):
    """返回 (status, body_bytes, headers)。jar 非空则顺带收 Set-Cookie。"""
    hdrs = {"User-Agent": UA, "Accept": "*/*"}
    if headers:
        hdrs.update(headers)
    raw = None
    if data is not None:
        if isinstance(data, (dict, list)):
            raw = json.dumps(data).encode("utf-8")
            hdrs["Content-Type"] = "application/json"
        else:
            raw = data if isinstance(data, bytes) else str(data).encode("utf-8")
    req = urllib.request.Request(url, data=raw, headers=hdrs, method=method)
    resp = urllib.request.urlopen(req, timeout=timeout, context=_ctx())
    body = resp.read()
    if jar is not None:
        try:
            jar.extract_cookies(resp, req)
        except Exception:
            pass
    return resp.status, body, resp.headers, resp.geturl()


def _j(body):
    try:
        return json.loads(body.decode("utf-8", "replace"))
    except Exception:
        return {}


def _b64img(b):
    return "data:image/png;base64," + base64.b64encode(b).decode("ascii")


def _cookie_str(jar, *domains):
    out = []
    for c in jar:
        d = c.domain or ""
        if any(dm in d for dm in domains):
            out.append("%s=%s" % (c.name, c.value))
    return "; ".join(out)


def _follow(jar, url, headers, max_hop=8):
    """手动追 3xx（不自动跳），沿途把 Set-Cookie 收进 jar。"""
    loc = url
    for _ in range(max_hop):
        if not loc:
            break
        if loc.startswith("//"):
            loc = "https:" + loc
        elif loc.startswith("/"):
            from urllib.parse import urlparse
            p = urlparse(url)
            loc = "%s://%s%s" % (p.scheme, p.netloc, loc)
        try:
            req = urllib.request.Request(loc, headers=headers, method="GET")
            resp = urllib.request.urlopen(req, timeout=15, context=_ctx())
            if jar is not None:
                try:
                    jar.extract_cookies(resp, req)
                except Exception:
                    pass
            if resp.status in (301, 302, 303, 307, 308):
                loc = resp.headers.get("Location")
            else:
                break
        except urllib.error.HTTPError as e:
            if jar is not None:
                try:
                    jar.extract_cookies(e, e.req if hasattr(e, "req") else None)
                except Exception:
                    pass
            if e.code in (301, 302, 303, 307, 308):
                loc = e.headers.get("Location")
            else:
                break
        except Exception:
            break
    return _cookie_str(jar, *domains_of(url))


def domains_of(url):
    from urllib.parse import urlparse
    net = urlparse(url).netloc
    return (net,)


# ------------------------------------------------------------------ 适配器
class QRAdapter:
    platform = None

    def init(self):
        """返回 dict：qr(图片data-uri/url) / qr_text(待编码文本) 二选一，
        ticket, expire(秒), interval(轮询秒), state(任意透传)。"""
        raise NotImplementedError

    def poll(self, ctx):
        """返回 (status, payload)。
        status: waiting / scanned / expired / confirmed / error
        confirmed -> payload 为凭据 dict {cookie, bearer, account, type, refresh_token}"""
        raise NotImplementedError


class DoubaoQR(QRAdapter):
    """豆包：字节 passport 二维码。参考 doubao2api（qr_login）。"""
    platform = "apk-doubao"

    def init(self):
        jar = http.cookiejar.CookieJar()
        _http("GET", "https://www.doubao.com/", jar=jar)
        csrf = _j(_http("GET",
                        "https://www.doubao.com/passport/safe/csrf_token/?aid=497858",
                        jar=jar)[1])
        csrf = (csrf.get("data") or {}).get("passport_csrf_token", "")
        h = {"x-tt-passport-csrf-token": csrf,
             "Referer": "https://www.doubao.com/chat/login",
             "Origin": "https://www.doubao.com"}
        nxt = urllib.parse.quote("https://www.doubao.com")
        d = _j(_http("GET",
                     "https://www.doubao.com/passport/web/get_qrcode/"
                     "?next=%s&aid=497858" % nxt, headers=h, jar=jar)[1])
        d = d.get("data") or {}
        token = d.get("token", "")
        qrimg = d.get("qrcode", "")
        if not token or not qrimg:
            raise RuntimeError("豆包二维码接口未返回 token/qrcode（可能需 a_bogus 签名）")
        # 接口直接返回 data:image/png;base64,...（已含前缀），不要再解一次
        img = qrimg if qrimg.startswith("data:image") else _b64img(base64.b64decode(qrimg))
        return {"qr": img, "ticket": token, "expire": 120, "interval": 2,
                "message": "请用【抖音 App】扫此二维码（豆包要求抖音扫码验证）",
                "state": {"jar": jar, "token": token, "csrf": csrf, "h": h}}

    def poll(self, c):
        h = c["state"]["h"]
        token = c["state"]["token"]
        jar = c["state"]["jar"]
        nxt = urllib.parse.quote("https://www.doubao.com")
        d = _j(_http("GET",
                     "https://www.doubao.com/passport/web/check_qrconnect/"
                     "?next=%s&token=%s&aid=497858" % (nxt, token),
                     headers=h, jar=jar)[1])
        d = d.get("data") or {}
        st = d.get("status")
        if st == "confirmed":
            url = d.get("redirect_url") or ""
            if url:
                _follow(jar, url, h)
            ck = _cookie_str(jar, "doubao.com")
            return "confirmed", {"cookie": ck, "account": "", "type": "cookie"}
        if st == "expired":
            return "expired", None
        return ("scanned" if st == "scanned" else "waiting"), None


class YuanbaoQR(QRAdapter):
    """元宝：微信 qrconnect + /api/joint/login。参考 yuanbao-free-api。"""
    platform = "apk-yuanbao"
    APPID = "wx12b75947931a04ec"

    def init(self):
        cb = ("https://yuanbao.tencent.com/desktop-redirect.html?"
              "&&bindType=wechat_login&login_type=jssdk&self_redirect=true")
        url = ("https://open.weixin.qq.com/connect/qrconnect?"
               "appid=%s&scope=snsapi_login&redirect_uri=%s" %
               (self.APPID, urllib.parse.quote(cb)))
        html = _http("GET", url)[1].decode("utf-8", "replace")
        m = re.search(r'img[^>]*js_qrcode_img[^>]*src="([^"]+)"', html)
        if not m:
            raise RuntimeError("未在微信 qrconnect 页面找到二维码（页面结构可能变了）")
        src = m.group(1)
        qr_url = "https://open.weixin.qq.com" + src if src.startswith("/") else src
        img = _b64img(_http("GET", qr_url)[1])
        # uuid 在图片地址末段
        uuid = src.rstrip("/").split("/")[-1].split("?")[0]
        return {"qr": img, "ticket": uuid, "expire": 180, "interval": 1.5,
                "state": {"uuid": uuid}}

    def poll(self, c):
        uuid = c["state"]["uuid"]
        ts = int(time.time() * 1000)
        txt = _http("GET",
                    "https://lp.open.weixin.qq.com/connect/l/qrconnect"
                    "?uuid=%s&_=%d" % (uuid, ts))[1].decode("utf-8", "replace")
        m = re.search(r"wx_errcode=(\d*);window\.wx_code='([^']*)';", txt)
        if not m:
            return "waiting", None
        code, wx = m.groups()
        if wx:
            # 换凭证
            jar = http.cookiejar.CookieJar()
            h = {"x-a3": "0", "x-source": "web",
                 "User-Agent": UA + " app/tencent_yuanbao",
                 "Referer": "https://yuanbao.tencent.com/"}
            _http("POST", "https://yuanbao.tencent.com/api/joint/login",
                  headers=h, data={"type": "wx", "jsCode": wx, "appid": self.APPID},
                  jar=jar)
            ck = _cookie_str(jar, "yuanbao.tencent.com", "tencent.com")
            return "confirmed", {"cookie": ck, "account": "", "type": "cookie"}
        code = int(code or 0)
        if code == 402:
            return "expired", None
        if code == 404:
            return "scanned", None
        if code in (403,):
            return "error", "微信拒绝该二维码（403）"
        return "waiting", None


class BaiduQR(QRAdapter):
    """百度文库 / 库库：百度 passport 二维码。"""
    platform = "apk-kuku"

    def init(self):
        gid = uuid4_hex()
        cb = "tangram_guid_%d" % int(time.time() * 1000)
        url = ("https://passport.baidu.com/v2/api/?getqrcode&lp=pc"
               "&apiver=v3&tpl=pp&gid=%s&callback=%s" % (gid, cb))
        d = _j(_http("GET", url)[1])
        sign = d.get("sign", "")
        imgurl = d.get("imgurl", "")
        if not sign:
            raise RuntimeError("百度二维码接口未返回 sign（可能参数被拦）")
        return {"qr": imgurl, "ticket": sign, "expire": 120, "interval": 2,
                "state": {"sign": sign, "gid": gid, "cb": cb}}

    def poll(self, c):
        sign = c["state"]["sign"]
        d = _j(_http("GET",
                     "https://passport.baidu.com/v2/api/checkqrcode"
                     "?sign=%s&apiver=v3&tpl=pp" % sign)[1])
        errno = d.get("errno")
        if errno == 1:
            return "waiting", None
        if errno == 0 and d.get("status") == 1:
            return "scanned", None
        code = d.get("code")
        if code:
            jar = http.cookiejar.CookieJar()
            u = ("https://passport.baidu.com/v2/api/exchange_token"
                 "?sign=%s&code=%s&u=https://kuku.baidu.com/&apiver=v3&tpl=pp" % (sign, code))
            _follow(jar, u, {"User-Agent": UA})
            ck = _cookie_str(jar, "baidu.com")
            return "confirmed", {"cookie": ck, "account": "", "type": "cookie"}
        if errno in (0,) and not code:
            return "waiting", None
        return "error", "百度扫码状态未知：%s" % str(d)[:200]


class GoogleQR(QRAdapter):
    """Google / Antigravity：OAuth2 Device Flow（官方，需自备 client_id）。"""
    platform = "apk-antigravity"

    def _conf(self):
        cid = os.environ.get("GOOGLE_OAUTH_CLIENT_ID", "")
        csec = os.environ.get("GOOGLE_OAUTH_CLIENT_SECRET", "")
        try:
            root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
            with open(os.path.join(root, "data", "settings.json"),
                      encoding="utf-8") as f:
                s = json.load(f) or {}
            cid = s.get("google_oauth_client_id") or cid
            csec = s.get("google_oauth_client_secret") or csec
        except Exception:
            pass
        return cid, csec

    def init(self):
        cid, csec = self._conf()
        if not cid:
            raise RuntimeError(
                "Google 远程扫码需配置 OAuth client_id（类型：TV/有限输入设备）。"
                "请在 data/settings.json 写 google_oauth_client_id / "
                "google_oauth_client_secret，或设置环境变量 GOOGLE_OAUTH_CLIENT_ID")
        d = _j(_http("POST", "https://oauth2.googleapis.com/device/code",
                     data={"client_id": cid,
                           "scope": "email profile openid"})[1])
        if "device_code" not in d:
            raise RuntimeError("Google device/code 未返回 device_code：%s" % str(d)[:200])
        return {"qr_text": d.get("verification_url", ""),
                "ticket": d.get("device_code", ""),
                "expire": d.get("expires_in", 1800),
                "interval": max(1, int(d.get("interval", 5))),
                "state": {"cid": cid, "csec": csec,
                          "user_code": d.get("user_code", "")}}

    def poll(self, c):
        cid = c["state"]["cid"]
        csec = c["state"]["csec"]
        device = c["ticket"]
        d = _j(_http("POST", "https://oauth2.googleapis.com/token",
                     data={"client_id": cid, "client_secret": csec,
                           "code": device,
                           "grant_type": "urn:ietf:params:oauth:grant-type:device_code"})[1])
        if "access_token" in d:
            return "confirmed", {"bearer": d["access_token"],
                                "refresh_token": d.get("refresh_token", ""),
                                "account": d.get("id_token", "")[:40],
                                "type": "bearer"}
        err = d.get("error", "")
        if err in ("authorization_pending", "slow_down"):
            return "waiting", None
        if err == "expired_token":
            return "expired", None
        if err == "access_denied":
            return "error", "用户在手机上拒绝了授权"
        return "error", "Google token 返回：%s" % str(d)[:200]


def uuid4_hex():
    import uuid as _u
    return _u.uuid4().hex


ADAPTERS = {
    "apk-doubao": DoubaoQR(),
    "apk-yuanbao": YuanbaoQR(),
    "apk-kuku": BaiduQR(),
    "apk-antigravity": GoogleQR(),
}


def supported_platforms():
    return list(ADAPTERS.keys())


# ------------------------------------------------------------------ 运行
def _finish_cred(lm, sess, spec, cred, source):
    """把确认后的凭据校验并落库（复用 gwlogin.verify_cookie）。"""
    if sess.status == "success":
        return
    from .gwlogin import verify_cookie
    secret = cred.get("bearer") or cred.get("cookie") or ""
    typ = cred.get("type") or ("bearer" if cred.get("bearer") else "cookie")
    try:
        okk, info = verify_cookie(spec, secret)
    except Exception as e:
        okk, info = False, "%s: %s" % (type(e).__name__, e)
    # 这些平台在线校验端点多需请求签名 / 业务码，易误判。
    # 拿到真凭据却校验失败时不阻断，存为「未校验」，留待实际对话验证。
    if not okk:
        sess.finish("success",
                    "远程扫码已取得凭据，但在线校验未通过"
                    "（该平台接口可能需签名，以实际对话验证为准）",
                    result={"type": typ, "secret": secret,
                            "account": cred.get("account") or info,
                            "source": source,
                            "refresh_token": cred.get("refresh_token", ""),
                            "verify_note": info})
        lm._save(sess)
        return
    sess.finish("success", "远程扫码登录成功（来源：%s）" % source,
                result={"type": typ, "secret": secret,
                        "account": cred.get("account") or info,
                        "source": source,
                        "refresh_token": cred.get("refresh_token", "")})
    lm._save(sess)


def start_remote_qr(lm, platform):
    """为某平台发起一次服务端远程扫码会话，返回 LoginSession。"""
    from .gwlogin import LoginSession, PLATFORMS
    spec = PLATFORMS.get(platform)
    adapter = ADAPTERS.get(platform)
    sess = LoginSession(platform, "remotescan")
    with lm.lock:
        lm.sessions[sess.id] = sess
    threading.Thread(target=_run_remote, args=(lm, sess, spec, adapter),
                     daemon=True).start()
    for _ in range(40):
        time.sleep(0.05)
        if sess.status in ("waiting", "success", "error", "expired") \
                or sess.qr or sess.qr_text:
            break
    return sess


def _run_remote(lm, sess, spec, adapter):
    if not adapter:
        sess.finish("error", "该平台暂不支持远程扫码",
                    "请用「手动粘贴 Cookie」或在本机运行面板用 Chrome 扫码")
        return
    try:
        info = adapter.init()
    except Exception as e:
        sess.finish("error", "生成远程二维码失败",
                    "%s: %s" % (type(e).__name__, e))
        return
    with sess.lock:
        sess.qr = info.get("qr", "")
        sess.qr_text = info.get("qr_text", "")
        sess.status = "waiting"
        sess.expires_at = time.time() + (info.get("expire") or 180)
        msg = info.get("message") or "请用手机扫描下方二维码完成登录；确认后自动捕捉凭据"
        if sess.qr_text:
            msg += "（用手机相机/微信扫后，在打开的页面输入验证码 %s）" % \
                   info.get("state", {}).get("user_code", "")
        sess.message = msg
    ctx = info
    deadline = sess.expires_at
    interval = info.get("interval", 3)
    while time.time() < deadline:
        if getattr(sess, "_stop", False) or sess.status in ("success", "error"):
            return
        try:
            st, cred = adapter.poll(ctx)
        except Exception as e:
            sess.finish("error", "轮询登录状态失败",
                        "%s: %s" % (type(e).__name__, e))
            return
        if st == "confirmed" and isinstance(cred, dict):
            _finish_cred(lm, sess, spec, cred, "远程扫码")
            return
        if st == "expired":
            sess.finish("expired", "二维码已过期，请取消后重新发起", "expired")
            return
        if st == "error":
            sess.finish("error", "登录失败", str(cred)[:300])
            return
        if st == "scanned":
            with sess.lock:
                if "已扫描" not in (sess.message or ""):
                    sess.message = (sess.message or "") + "（已扫描，请在手机上确认）"
        time.sleep(interval)
    if sess.status == "waiting":
        sess.finish("expired", "二维码超时未确认", "timeout")
