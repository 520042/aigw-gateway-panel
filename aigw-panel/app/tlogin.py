# -*- coding: utf-8 -*-
"""
腾讯 / CodeBuddy 登录与凭据获取（抓包逆向出的真实链路）
=====================================================
本文件里每一条路径、每一个参数都来自 2026-10-04 的 CDP 全量抓包实测，
不是猜的。链路如下：

  1) 本地生成 state（uuid4，格式不限，服务端不校验）
  2) 浏览器打开  https://copilot.tencent.com/login?platform=CLI&state=<state>
     —— 缺 state 会直接报「登录链接不完整」
  3) 302 → www.codebuddy.cn/login?platform=CLI&state=<state>
  4) 站内灰度探测 /v2/plugin/login/gray-decision?feature=...&platform=CLI
  5) 跳 Keycloak（realm=copilot，client_id=console，response_type=code）
     → 企业微信 IdP：/auth/realms/copilot/console/auth/wechat-url
       → open.weixin.qq.com/connect/qrconnect（appid=wxd1fa324ebb4cbed1）
     → 用户扫码 → 302 回 /console/accounts/.apisix/redirect
  6) **关键** 页面在浏览器里发一个同源请求：
         POST /console/login/enterprise?state=<state>
     → 直接返回 {"code":0,"data":{"accessToken":"<RS256 JWT>",
                                   "refreshToken":"<...>"}}
     这一步才是真正发 token 的地方。

为什么必须走第 6 步
  另一个入口 GET /console/auth/login?platform=CLI&state=<state>&domain=...
  是给浏览器页面跳转用的，它会 302 到 Keycloak 再 302 回来，
  跟着跳完再请求就变成 {"code":400,"msg":"state is invalid"}。
  逆向二进制里只有这个 auth/login，看不到 enterprise 这条路 —— 只有抓包能拿到。

token 拿到后**纯裸 HTTP 即可**，不再需要浏览器：
  Authorization: Bearer <accessToken>
  User-Agent: CLI/2.143.1 CodeBuddy/2.143.1     ← 少了这个 /v3/config 报 12403
  X-Ide-Type: production
  X-Ide-Name: workbuddy
  实测 200 的接口：/v3/config（倍率表）、/console/enterprises/personal/models
"""

import json
import os
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))

# 数据根目录必须和 main.base_dir() 完全一致，否则打包后会把凭据
# 写进 _MEIPASS（PyInstaller 的临时解包目录，进程退出就没了）。
#   源码运行：ROOT = <项目根>
#   打包运行：ROOT = exe 同级   ← 凭据落在 exe 旁边的 data/
if getattr(sys, "frozen", False):
    ROOT = os.path.dirname(sys.executable)
else:
    ROOT = os.path.dirname(HERE)

LOGIN_HOST = "https://copilot.tencent.com"
WEB_HOST = "https://www.codebuddy.cn"

# 网关伪装（实测缺 UA 时 /v3/config 返回 12403 "check ua"）
UA = "CLI/2.143.1 CodeBuddy/2.143.1"
HDRS = {
    "User-Agent": UA,
    "Accept": "application/json",
    "Content-Type": "application/json",
    "X-Ide-Type": "production",
    "X-Ide-Name": "workbuddy",
    "X-Ide-Version": "2.143.1",
}

CDP_PORT = 9333
PROFILE = os.path.join(ROOT, "data", "chrome-capture-profile")


def new_state():
    """state 本地生成即可，实测任意字符串（含 uuid）都能通过"""
    return str(uuid.uuid4())


def login_url(state, platform="CLI"):
    """平台名实测必须是 CLI，写错服务端不认"""
    return "%s/login?platform=%s&state=%s" % (LOGIN_HOST, platform, state)


# --------------------------------------------------------------- 浏览器侧

def open_login(state, port=CDP_PORT, user_data_dir=PROFILE, wait=6):
    """
    起一个带调试端口的 Chrome 打开登录页，返回 Browser 对象。
    已经有浏览器在跑就直接复用（避免 profile 锁冲突）。
    """
    from app.cdp import Browser, find_browser
    br = Browser(port=port, exe=find_browser(), user_data_dir=user_data_dir)
    if not br.is_up():
        br.start(login_url(state))
        time.sleep(wait)
    else:
        br.open_tab(login_url(state))
        time.sleep(3)
    return br


def exchange_in_page(port=CDP_PORT, state=None, timeout=45):
    """
    在**浏览器页面上下文**里跑 fetch 换 token。

    为什么非得在页面里跑：
      - 要同源，浏览器自动带上 Keycloak 的 HttpOnly 会话 Cookie
        （AUTH_SESSION_ID / KEYCLOAK_IDENTITY / session）
      - 手搓 Cookie 请求这些接口会 401（APISIX 校验），试过
      - 页面里跑就绕开了全部 Cookie 域/路径匹配的坑

    返回 dict：{state, accessToken, refreshToken, expire} 或 {} 表示失败
    """
    from app.cdp import Session, Browser
    state = state or new_state()
    br = Browser(port=port)
    tabs = [t for t in br.targets() if t.get("type") == "page"]
    if not tabs:
        return {}
    # 优先挑 codebuddy 域的标签
    tab = None
    for t in tabs:
        if "codebuddy" in (t.get("url") or "") or "tencent" in (t.get("url") or ""):
            tab = t
            break
    tab = tab or tabs[0]
    s = Session(tab["webSocketDebuggerUrl"])
    try:
        js = r"""
(async function(){
  var st = %s;
  var post = {
    method:'POST', credentials:'include',
    headers:{'Content-Type':'application/json'}
  };
  var r = await fetch('/console/login/enterprise?state='+st, post);
  var t = await r.text();
  if (r.status !== 200) {
    return JSON.stringify({_err:r.status, _body:t.slice(0,400), state:st});
  }
  var d = JSON.parse(t);
  var a = (d && d.data && (d.data.accessToken || d.data.access_token)) || '';
  var f = (d && d.data && (d.data.refreshToken || d.data.refresh_token)) || '';
  return JSON.stringify({state:st, accessToken:a, refreshToken:f,
                         expire:(d&&d.data&&(d.data.expireIn||d.data.expiresIn))||'',
                         userId:(d&&d.data&&d.data.userId)||''});
})()
""" % json.dumps(state)
        r = s.call("Runtime.evaluate",
                   {"expression": js, "awaitPromise": True,
                    "returnByValue": True}, timeout=timeout)
        val = (r.get("result") or {}).get("value")
        if not val:
            return {}
        return json.loads(val)
    finally:
        s.close()


def poll_token(port=CDP_PORT, state=None, timeout=300, interval=2):
    """
    等用户扫码。轮询 enterprise 端点 —— 一旦登录完成它就直接给 token。
    浏览器端那条 /v2/plugin/auth/token?state= 只能告诉你「登录中」，
    真正发 token 的是 enterprise。
    """
    state = state or new_state()
    t0 = time.time()
    last = None
    while time.time() - t0 < timeout:
        d = exchange_in_page(port=port, state=state, timeout=20)
        if d.get("accessToken"):
            return d
        # 顺手也问一下官方那个轮询端点，便于对照
        code = None
        try:
            from app.tencent import Tencent
            ok, r = Tencent("").call("oauth_token", state=state)
            if isinstance(r, dict):
                code = r.get("code")
        except Exception:
            pass
        sig = (d.get("_err"), code)
        if sig != last:
            last = sig
        time.sleep(interval)
    return {}


# --------------------------------------------------------------- 直连侧

def _req(url, method="GET", token="", body=None, timeout=20, extra=None):
    import urllib.request
    import urllib.error
    h = dict(HDRS)
    if token:
        h["Authorization"] = "Bearer " + token
    if extra:
        h.update(extra)
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    r = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as x:
            return x.status, x.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:
        return 0, str(e)


def _json(url, method="GET", token="", body=None, timeout=20, extra=None):
    s, b = _req(url, method, token, body, timeout, extra)
    try:
        return s, json.loads(b)
    except Exception:
        return s, {"_raw": b[:400]}


def verify(token, base=LOGIN_HOST):
    """
    拿 token 验活：返回各端点状态。
    实测：/v3/config 和 /console/.../models 都要 Bearer + 伪装 UA 才 200。
    """
    out = {}
    for name, path in (("rates", "/v3/config"),
                       ("models", "/console/enterprises/personal/models")):
        s, d = _json(base + path, token=token)
        out[name] = (s, d if s == 200 else str(d)[:200])
    return out


def models_merged(token, base=LOGIN_HOST):
    """
    合并两个原生端点的模型清单（去重）—— ★ 单拉一个都不全。

    实测（2026-10-06）：
      /console/enterprises/personal/models → 31 个（含 auto、hy3、hy4-preview…）
      /v3/config                           → 32 个（含 deepseek-v4-pro、minimax-m2.5…）
      两者集合不同，合并去重 = 34 个。
    WorkBuddy 客户端的模型列表跨这两个源，所以面板也必须合并才对得上。
    字段冲突时以 /console 的为准（它带 credits/vendor/上下文），缺失项用 /v3/config 补。
    """
    out = {}
    # 先 /v3/config（补全用），再 /console（字段优先）
    for path in ("/v3/config", "/console/enterprises/personal/models"):
        try:
            s, d = _json(base + path, token=token)
        except Exception:
            continue
        if s != 200:
            continue
        for m in parse_models(d):
            mid = m.get("id")
            if not mid:
                continue
            old = out.get(mid)
            if not old:
                out[mid] = m
                continue
            for k in ("credits", "vendor", "maxInputTokens",
                      "maxOutputTokens", "maxAllowedSize"):
                if not old.get(k) and m.get(k):
                    old[k] = m[k]
            for k in ("supportsImages", "supportsToolCall",
                      "supportsReasoning", "onlyReasoning"):
                old[k] = bool(old.get(k)) or bool(m.get(k))
    return [out[k] for k in sorted(out)]


def models_with_credits(token, base=LOGIN_HOST):
    """
    在线模型目录 —— 带 credits（倍率）。
    这是倍率三源里优先级最高的「在线目录」源。
    ⚠ 只拉 /console 一个端点（31 个）；要全量清单请用 models_merged()。
    """
    s, d = _json(base + "/console/enterprises/personal/models", token=token)
    if s != 200:
        return []
    return parse_models(d)


def parse_models(d):
    """把 /console/.../models 或 /v3/config 的返回归一成统一结构"""
    data = d.get("data") if isinstance(d, dict) else None
    if not isinstance(data, dict):
        return []
    src = data.get("models")
    if not isinstance(src, list):
        src = []
    out = []
    for m in src:
        if not isinstance(m, dict):
            continue
        out.append({
            "id": m.get("id") or m.get("name") or "",
            "name": m.get("name") or m.get("id") or "",
            "credits": m.get("credits") or "",
            "vendor": m.get("vendor") or "",
            "maxInputTokens": m.get("maxInputTokens") or 0,
            "maxOutputTokens": m.get("maxOutputTokens") or 0,
            "maxAllowedSize": m.get("maxAllowedSize") or 0,
            "supportsImages": bool(m.get("supportsImages")),
            "supportsToolCall": bool(m.get("supportsToolCall")),
            "supportsReasoning": bool(m.get("supportsReasoning")),
            "onlyReasoning": bool(m.get("onlyReasoning")),
            "relatedModels": m.get("relatedModels") or {},
            "source": "online",
        })
    return out


def agents(d):
    """从 /v3/config 里取 agent 定义（含工具清单、可用模型）"""
    data = (d or {}).get("data") if isinstance(d, dict) else None
    if not isinstance(data, dict):
        return []
    return [a for a in (data.get("agents") or []) if isinstance(a, dict)]


# --------------------------------------------------------------- token 刷新

def refresh(refresh_token, base=LOGIN_HOST):
    """
    刷新 accessToken（2026-10-04 实测打通）。

    ★ 关键坑：refreshToken **不是放 body / query，而是放 header** `X-Refresh-Token`。
      实测过的形态（都报 400「refreshToken is empty」，说明没收到）：
        body {"refreshToken": "..."}      ✗
        body {"refresh_token": "..."}     ✗
        query ?refreshToken=...           ✗
        body 纯文本 / 裸 JSON 字符串        ✗
        header Authorization / X-Token     ✗
        header Cookie refreshToken=...     ✗
      只有这样才行：
        header X-Refresh-Token: <token>   ✓ → 200 {"code":0,"data":{accessToken,refreshToken}}

      参数位置对但 token 内容不对时，报的是
        401 {"code":12153,"msg":"12153:refresh token failed:10000:token format error"}
      —— 可以据此区分「没收到参数」和「token 无效」。

    返回 (ok, {"accessToken","refreshToken","expire"} 或错误信息)
    """
    if not refresh_token:
        return False, "没有 refreshToken"
    url = base.rstrip("/") + "/v2/plugin/auth/token/refresh"
    h = dict(HDRS)
    h["X-Refresh-Token"] = refresh_token
    s, b = _req(url, "POST", body={}, extra=h)
    if s != 200:
        return False, b[:300]
    try:
        d = json.loads(b)
    except Exception:
        return False, b[:300]
    if d.get("code") != 0:
        return False, d.get("msg") or str(d)[:200]
    data = d.get("data") or {}
    at = data.get("accessToken") or data.get("access_token") or ""
    rt = data.get("refreshToken") or data.get("refresh_token") or refresh_token
    if not at:
        return False, "刷新响应里没有 accessToken：" + str(d)[:200]
    return True, {
        "accessToken": at,
        "refreshToken": rt,
        "expire": data.get("expireIn") or data.get("expiresIn") or "",
    }


def refresh_saved(path=None):
    """读已保存凭据 → 刷新 → 覆盖落盘。返回 (ok, 说明)"""
    c = load(path)
    rt = c.get("refreshToken") or ""
    if not rt:
        return False, "没有 refreshToken，请先登录"
    ok, d = refresh(rt)
    if not ok:
        return False, str(d)[:200]
    c["accessToken"] = d["accessToken"]
    c["refreshToken"] = d["refreshToken"]
    c["expire"] = d.get("expire") or c.get("expire")
    c["refreshed_at"] = time.strftime("%Y-%m-%d %H:%M:%S")
    p = save(c, path)
    return True, "已刷新并落盘 %s（token %d 字节）" % (p, len(d["accessToken"]))


# --------------------------------------------------------------- 凭据落盘

def save(cred, path=None):
    p = path or os.path.join(ROOT, "data", "tencent_cred.json")
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "w", encoding="utf-8") as f:
        json.dump(cred, f, ensure_ascii=False, indent=1)
    return p


def load(path=None):
    p = path or os.path.join(ROOT, "data", "tencent_cred.json")
    if not os.path.exists(p):
        return {}
    with open(p, "r", encoding="utf-8") as f:
        return json.load(f)


if __name__ == "__main__":
    import sys
    if "--token" in sys.argv:
        # 从已登录的浏览器直接换 token
        d = exchange_in_page()
        print(json.dumps({k: (v[:40] + "..." if k.endswith("Token") and v else v)
                          for k, v in d.items()}, ensure_ascii=False, indent=1))
        if d.get("accessToken"):
            print("已存:", save(d))
            print("验活:", verify(d["accessToken"]))
            ms = models_with_credits(d["accessToken"])
            print("在线模型 %d 个，示例：" % len(ms))
            for m in ms[:6]:
                print("   %-24s %-14s in=%s out=%s img=%s tool=%s" % (
                    m["id"], m["credits"], m["maxInputTokens"],
                    m["maxOutputTokens"], m["supportsImages"],
                    m["supportsToolCall"]))
    else:
        print(__doc__)
