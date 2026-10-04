# -*- coding: utf-8 -*-
"""
7 个 APP 网关内部能力的补齐层
=============================
对比 APK 里扒出来的接口清单，面板原先只实现了 wb-gateway 那一份，
其余 6 个的功能（签到领积分、做任务、兑换码、积分流水、登录送积分、
文生图、会话创建）全都缺。这里按平台把接口补齐，凭据统一从账号池取。

调用方式统一：
    call(accounts, platform, action, params) -> (ok:bool, data|error:str)

所有请求走标准库 urllib；网络不可达时返回明确的 unreachable，
不把"连不上"伪装成"接口不存在"。
"""

import json
import ssl
import urllib.error
import urllib.parse
import urllib.request

UA = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
      "(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")

# ------------------------------------------------------------------ 平台基址

# 平台默认基址
BASE = {
    "apk-trae": "https://api.trae.cn",
    "apk-codebuddy": "https://www.codebuddy.ai",
    "apk-codebuddy-cn": "https://www.codebuddy.cn",
    "apk-doubao": "https://www.doubao.com",
    "apk-yuanbao": "https://yuanbao.tencent.com",
    "apk-raccoon": "https://xiaohuanxiong.com",
    "apk-go": "https://copilot.tencent.com",
    # ↓ 下面两个是从 base.apk（aigw.app）里挖出来的多供应商反代
    "apk-loomy": "https://loomyad.xunfei.cn",          # 讯飞 Loomy
    "apk-antigravity": "https://cloudcode-pa.googleapis.com",  # Google Antigravity
    "apk-coze": "https://api.coze.cn",           # 扣子（字节）官方开放 API
    # ↓ 办公 AI：这几家是**整站 /api 前缀统一鉴权**，404 探测法失效
    #   （实测豆包 www.doubao.com/api/v1/* 与千问 www.qoder.com/api/v1/*
    #    全部返回 401，包括我瞎编的路径）。必须登录后用 probe_endpoints() 动态扫。
    "apk-qwenwork": "https://qwenwork.cn",       # 千问办公（阿里）
    "apk-kuku": "https://kuku.baidu.com",        # 库库 AI（百度文库+网盘）
    "apk-wps": "https://www.wps.cn",             # WPS AI（金山）
    "apk-nano": "https://www.n.cn",              # 纳米 AI（360）
    "apk-metaso": "https://metaso.cn",           # 秘塔 AI（有官方 Open API）
    "apk-qoder": "https://www.qoder.com",        # Qoder（阿里，CLIProxyAPI 已支持其 OAuth）
    # ↓ 国内网页版 / 客户端对话式 AI（整站鉴权，必须登录后探测端点）
    "apk-chatglm": "https://chatglm.cn",         # 智谱清言（官方 API 是 open.bigmodel.cn）
    "apk-qwen": "https://chat.qwen.ai",         # 通义千问网页版（官方 API 是 dashscope）
    "apk-kimi": "https://www.kimi.com",          # Kimi 网页版（官方 API 是 api.moonshot.cn）
    "apk-ernie": "https://yiyan.baidu.com",      # 文心一言（官方 API 是 qianfan.baidubce.com）
    "apk-nanobot": "https://www.n.cn",           # 纳米 AI
}

# Trae 一个 APP 拆在两台主机上（实测 200/401 才确认）：
#   api.trae.cn        → 签到、权益、用量（挂 /trae 前缀下）
#   www.trae.com.cn    → 积分、用户、配额、手机登录、成长活动
TRAE_WEB = "https://www.trae.com.cn"

# 各供应商的固定请求头（从 APK 的 UA / header 常量串里扒出来的）
VENDOR_HEADERS = {
    # APK 里的 UA 串：Loomy|Desktop|Electron|macOS
    "apk-loomy": {"User-Agent": "Loomy|Desktop|Electron|macOS",
                  "loomy-version": "1.0.0"},
    # APK 里的 UA 串：antigravity/1.15.8 windows/amd64
    "apk-antigravity": {
        "User-Agent": "antigravity/1.15.8 windows/amd64",
        # APK 里的配置串
        "X-IDE-Type": "ANTIGRAVITY",
        "X-Plugin-Type": "GEMINI",
    },
}

# 每个平台支持的动作（面板 UI 与自测都按这张表走）
#
# 路径来源：APK dex 字符串实测抽取（见 scan_missing_apis.py），不是猜的。
# 注意 Trae 的接口挂在 /trae 前缀下（APK 内是 /trae/api/v2/...），
# 早期版本漏了这个前缀 → 全部 404，这里已修正。
ACTIONS = {
    "apk-trae": [
        {"id": "checkin_status", "name": "签到状态", "method": "GET",
         "path": "/trae/api/v2/ug/checkin_credits/status", "verified": True},
        {"id": "checkin_claim", "name": "领取签到积分", "method": "POST",
         "path": "/trae/api/v2/ug/checkin_credits/claim", "verified": True},
        {"id": "points_activation", "name": "积分激活", "method": "POST",
         "path": "/api/v1/points/activation", "base": TRAE_WEB, "local": True},
        {"id": "points_balance", "name": "积分余额", "method": "GET",
         "path": "/v2/billing/meter/get-user-resource", "verified": True, "base": TRAE_WEB},
        {"id": "redeem", "name": "兑换码兑换", "method": "POST",
         "path": "/api/v1/points/redemption-codes/redeem", "need": ["code"],
         "base": TRAE_WEB, "local": True},
        {"id": "team_points", "name": "团队积分", "method": "GET",
         "path": "/api/v1/team-points/balance", "base": TRAE_WEB, "local": True},
        {"id": "entitlement", "name": "当前权益套餐", "method": "GET",
         "path": "/trae/api/v2/pay/user_current_entitlement_list", "verified": True},
        {"id": "usage", "name": "用量账单", "method": "GET",
         "path": "/trae/api/v2/pay/ide_user_ent_usage", "verified": True},
        {"id": "usage_web", "name": "网页版用量", "method": "GET",
         "path": "/trae/api/v2/pay/web_user_ent_usage", "verified": True},
        {"id": "growth", "name": "成长活动", "method": "GET",
         "path": "/v2/activity/growth", "verified": True, "base": TRAE_WEB},
        {"id": "user_info", "name": "用户基础信息", "method": "GET",
         "path": "/userinfo/query/baseInfo", "verified": True, "base": TRAE_WEB},
        {"id": "user_info_cloud", "name": "云端用户信息", "method": "GET",
         "path": "/cloudide/api/v3/trae/GetUserInfo", "verified": True, "base": TRAE_WEB},
        {"id": "quota", "name": "剩余配额", "method": "GET",
         "path": "/buddy/quota", "verified": True, "base": TRAE_WEB},
        {"id": "login_send_code", "name": "手机登录·发验证码", "method": "POST",
         "path": "/login/phone/sendMsgCode", "verified": True, "need": ["phone"], "base": TRAE_WEB},
        {"id": "login_check_code", "name": "手机登录·校验码", "method": "POST",
         "path": "/login/phone/checkCode", "verified": True, "need": ["phone", "code"],
         "base": TRAE_WEB},
    ],
    "apk-raccoon": [
        {"id": "login_bonus", "name": "登录送积分", "method": "POST",
         "path": "/api/web/desktop/v1/login/points/grant"},
        {"id": "points_balance", "name": "积分余额", "method": "GET",
         "path": "/api/web/points/v1/balance", "verified": True},
        {"id": "points_bills", "name": "积分流水", "method": "GET",
         "path": "/api/web/points/v1/bills"},
        {"id": "entitlement", "name": "权益信息", "method": "GET",
         "path": "/api/web/auth/v1/entitlement_info", "verified": True},
        {"id": "setting_info", "name": "站点设置", "method": "GET",
         "path": "/api/web/office/v3/setting_info"},
        {"id": "model_catalog", "name": "模型目录", "method": "GET",
         "path": "/model_catalog"},
        {"id": "refresh", "name": "刷新凭据", "method": "POST",
         "path": "/refresh"},
    ],
    "apk-codebuddy": [
        # —— 以下标记说明来自 2026-10-04 的端点审计（audit_endpoints.py）——
        # 方法：对同一站发一条「肯定不存在的随机路径」作基准，
        #       只有响应与基准**不同**才判定端点真实存在。
        # 标 unverified 的路径返回的 401 与基准**完全一致**（该站 Nginx 整站鉴权），
        # 因此无法证伪也无法证实 —— 保留但不要当成已验证。
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/console/enterprises/personal/models", "verified": True},
        {"id": "checkin", "name": "每日签到", "method": "POST",
         "path": "/v2/billing/meter/daily-checkin", "unverified": True},
        {"id": "points_balance", "name": "积分余额", "method": "GET",
         "path": "/v2/billing/meter/get-user-resource", "verified": True, "unverified": True},
        {"id": "usage_summary", "name": "用量摘要", "method": "GET",
         "path": "/billing/meter/get-user-resource-summary", "unverified": True},
        # 官方直连的二维码登录轮询口，实测 GET 200：
        #   {"code":11217,"msg":"11217:login ing..."} 表示已发起、待扫码
        # 同名 /v2/plugin/auth/state 已下线（404），不要再加回来。
        {"id": "auth_token", "name": "官方登录轮询", "method": "GET",
         "path": "/v2/plugin/auth/token", "base": "https://copilot.tencent.com",
         "verified": True},
        {"id": "login_account", "name": "账密登录", "method": "POST",
         "path": "/v2/plugin/login/account", "unverified": True,
         "need": ["username", "password"]},
        {"id": "invite_bind", "name": "邀请码绑定", "method": "POST",
         "path": "/activity/workbuddy/invitation/v2/bind", "unverified": True,
         "need": ["code"]},
        # /v2/plugin/auth/token/refresh 实测 **404**（与基准 401 不同 → 真的不存在），
        # 2026-10-04 从本表移除。
    ],
    "apk-doubao": [
        {"id": "user_info", "name": "账号信息", "method": "GET",
         "path": "/api/v1/user/info", "verified": True, "verified": True},
        # 2026-10-04 审计：/samantha/chat/completion 已下线（404 与基准不同），
        # 对话统一走 /v1/chat/completions。
        {"id": "chat", "name": "对话补全", "method": "POST",
         "path": "/v1/chat/completions", "need": ["messages"]},
        {"id": "image_gen", "name": "文生图", "method": "POST",
         "path": "/v1/images/generations", "verified": True,
         "need": ["prompt"]},
    ],
    "apk-yuanbao": [
        # 实测 2026-10-04：/api/models 返回 401（存在，只是要鉴权），
        # 而 /v1/models 与 /health 返回的是 SPA 的 HTML —— 别被 200 骗了。
        # 之前漏了 models 这条，所以「已登录却拿不到模型」。
        {"id": "models", "name": "模型列表", "method": "GET",
         "path": "/api/models"},
        {"id": "user_info", "name": "账号信息", "method": "GET",
         "path": "/api/getuserinfo", "verified": True},
        {"id": "conversation", "name": "创建会话", "method": "POST",
         "path": "/api/user/agent/conversation/create"},
        {"id": "chat", "name": "对话", "method": "POST",
         "path": "/api/chat/completions", "need": ["messages"]},
    ],
    "apk-go": [
        # copilot.tencent.com 是官网不是 API，/v1/* 实测 404。
        # 这款 Go 版 APP 走本地网关，不暴露公开 REST，这里留空并在 UI 标注。
    ],
    # ============================================================ 讯飞 Loomy
    # 证据：base.apk 顶部串 "Trae / Loomy / WorkBuddy / Antigravity"
    # 实测：GET /api/v1/models → 200 {"code":"100002","desc":"缺少 token"}
    "apk-loomy": [
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/api/v1/models"},
        {"id": "chat", "name": "对话补全", "method": "POST",
         "path": "/api/v1/chat/completions", "need": ["messages"]},
    ],
    # ====================================================== Google Antigravity
    # 证据：UA "antigravity/1.15.8 windows/amd64"、
    #      配置串 {"ideType":"ANTIGRAVITY","platform":"MACOS","pluginType":"GEMINI"}、
    #      OAuth client_id GOCSPX-...、本地回调 127.0.0.1:51120/authorize
    "apk-antigravity": [
        {"id": "load_code_assist", "name": "Cloud Code 会话", "method": "POST",
         "path": "/v1internal:loadCodeAssist", "needs_proxy": True},
        {"id": "onboard_user", "name": "用户引导", "method": "POST",
         "path": "/v1internal:onboardUser", "needs_proxy": True},
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/v1internal:listModels", "needs_proxy": True},
    ],
    # ================================================== CodeBuddy 国内站
    # 实测：www.codebuddy.cn/console/enterprises/personal/models → 400「未找到 cookie」
    #      www.codebuddy.cn/v2/billing/meter/get-user-resource → 401
    "apk-codebuddy-cn": [
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/console/enterprises/personal/models"},
        {"id": "checkin", "name": "每日签到", "method": "POST",
         "path": "/v2/billing/meter/daily-checkin"},
        {"id": "points_balance", "name": "积分余额", "method": "GET",
         "path": "/v2/billing/meter/get-user-resource", "verified": True},
        {"id": "usage_summary", "name": "用量摘要", "method": "GET",
         "path": "/billing/meter/get-user-resource-summary"},
        {"id": "login_account", "name": "账密登录", "method": "POST",
         "path": "/v2/plugin/login/account", "need": ["username", "password"]},
    ],
    # ================================================== 扣子 Coze（官方 API）
    # 唯一一个「官方开放 API 完整可用」的国内办公/Agent 平台。
    # 实测 2026-10-04：api.coze.cn 的 404 带明确错误码 {code:4000}，
    # 所以这里的 404 是可信的（不像某些平台整站 401 那样无法区分）。
    #   POST /v3/chat              → 200 {"code":4101,"msg":"The token you entered is incorrect"}
    #   GET  /v3/chat/retrieve     → 401 {"code":700012006,"msg":"access token invalid"}
    #   POST /v3/chat/cancel       → 401
    #   GET  /v3/chat/message/list → 401
    # 注意：开放 API 里**没有签到端点**（每日 1500 活动积分只在网页端），
    #       所以 checkin 标 probe 走动态探测。
    "apk-coze": [
        {"id": "chat", "name": "对话补全", "method": "POST",
         "path": "/v3/chat", "need": ["bot_id", "user_id", "content"]},
        {"id": "chat_retrieve", "name": "查询会话状态", "method": "GET",
         "path": "/v3/chat/retrieve", "need": ["conversation_id", "chat_id"]},
        {"id": "chat_cancel", "name": "停止生成", "method": "POST",
         "path": "/v3/chat/cancel", "need": ["conversation_id", "chat_id"]},
        {"id": "message_list", "name": "消息列表", "method": "GET",
         "path": "/v3/chat/message/list", "need": ["conversation_id", "chat_id"]},
    ],
}


def actions_of(platform):
    return ACTIONS.get(platform, [])


def action_spec(platform, action):
    for a in ACTIONS.get(platform, []):
        if a["id"] == action:
            return a
    return None


# ------------------------------------------------------------------ HTTP

def _ssl_ctx():
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    return ctx


def _request(base, spec, secret, params, timeout=20, vendor=None):
    path = spec["path"]
    method = spec.get("method", "GET")
    url = base.rstrip("/") + path
    body = None
    if method == "GET" and params:
        url += ("&" if "?" in url else "?") + urllib.parse.urlencode(params)
    headers = {"User-Agent": UA, "Accept": "application/json"}
    # 供应商专属 UA / header（Loomy、Antigravity 的 UA 是服务端校验的，不能用通用 UA）
    for k, v in (VENDOR_HEADERS.get(vendor) or {}).items():
        headers[k] = v
    if secret:
        # Trae / CodeBuddy 用 Bearer；Cookie 类平台用 Cookie 头
        if secret.lstrip().startswith("{") or "=" not in secret[:40]:
            headers["Authorization"] = "Bearer " + secret.strip()
        else:
            headers["Cookie"] = secret
    if method != "GET":
        body = json.dumps(params or {}).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=_ssl_ctx()) as r:
            raw = r.read().decode("utf-8", "replace")
            status = r.status
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        status = e.code
    except urllib.error.URLError as e:
        return False, "unreachable: %s" % e.reason
    except Exception as e:
        return False, "%s: %s" % (type(e).__name__, e)

    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        return status < 400, {"status": status, "raw": raw[:2000]}
    if status >= 400:
        return False, {"status": status, "body": data}
    return True, data


# ------------------------------------------------------------------ 入口

def call(accounts, platform, action, params=None, account_id=None):
    """
    执行某平台的一个动作。
    凭据来源优先级：指定 account_id > 该平台第一个可用账号 > 网关本地凭据
    """
    spec = action_spec(platform, action)
    if not spec:
        return False, "平台 %s 不支持动作 %s" % (platform, action)
    # 个别动作挂在不同主机上（Trae 签到在 api.trae.cn，其余在 www.trae.com.cn）
    base = spec.get("base") or BASE.get(platform)
    if not base:
        return False, "平台 %s 未配置基址" % platform

    for need in spec.get("need", []):
        if not (params or {}).get(need):
            return False, "缺少必填参数：%s" % need

    secret = ""
    acct_name = ""
    if account_id and accounts:
        a = accounts.get(account_id)
        if a:
            secret, acct_name = a.get("secret", ""), a.get("name", "")
    if not secret and accounts:
        usable = accounts.usable(platform)
        if usable:
            secret, acct_name = usable[0].get("secret", ""), usable[0].get("name", "")
    if not secret:
        return False, ("账号池里没有 %s 的可用凭据，请先在「账号登录」页完成登录"
                       % platform)

    okk, data = _request(base, spec, secret, params or {}, vendor=platform)
    if okk and accounts and account_id:
        try:
            accounts.update(account_id, {"last_used": __import__(
                "app.store", fromlist=["now_str"]).now_str()})
        except Exception:
            pass
    if okk:
        return True, {"platform": platform, "action": action,
                      "account": acct_name, "data": data}
    return False, data


def trae_checkin_flow(accounts, account_id=None):
    """Trae 完整签到：先看状态，未签则领"""
    okk, d = call(accounts, "apk-trae", "checkin_status", {}, account_id)
    if not okk:
        return False, d
    body = (d or {}).get("data") or {}
    if isinstance(body, dict) and body.get("checked_in_today"):
        return True, {"already": True, "data": body}
    ok2, d2 = call(accounts, "apk-trae", "checkin_claim", {}, account_id)
    return ok2, d2


def raccoon_login_bonus(accounts, account_id=None):
    """小浣熊登录送积分"""
    return call(accounts, "apk-raccoon", "login_bonus", {}, account_id)


def codebuddy_checkin(accounts, account_id=None):
    """CodeBuddy 每日签到（/v2/billing/meter/daily-checkin，实测 401=需登录）"""
    return call(accounts, "apk-codebuddy", "checkin", {}, account_id)


def codebuddy_points(accounts, account_id=None):
    """CodeBuddy 积分余额（/v2/billing/meter/get-user-resource）"""
    return call(accounts, "apk-codebuddy", "points_balance", {}, account_id)


# ================================================================= 模型目录
# 倍率的权威来源是登录后的个人模型目录接口（实测存在、需要登录态）：
#   CodeBuddy 国际: https://www.codebuddy.ai/console/enterprises/personal/models
#     未登录时返回 400 {"error":"Client not found."}
#   copilot 国内:   https://copilot.tencent.com/console/enterprises/personal/models
#     未登录时返回 400 {"error":"未找到 cookie。请确保浏览器已启用 cookie。"}
MODEL_CATALOGS = {
    "apk-codebuddy": [
        "https://www.codebuddy.ai/console/enterprises/personal/models",
        "https://copilot.tencent.com/console/enterprises/personal/models",
    ],
    "apk-trae": [
        "https://copilot.tencent.com/console/enterprises/personal/models",
    ],
    "wb-gateway": [
        "https://copilot.tencent.com/console/enterprises/personal/models",
    ],
    "wb-gateway-intl": [
        "https://www.codebuddy.ai/console/enterprises/personal/models",
    ],
    # 小浣熊 /model_catalog 实测 GET 200（无需登录即可通，但内容是 SPA 壳时
    # 会解析不出模型；登录后才返回结构化目录）
    "apk-raccoon": [
        "https://xiaohuanxiong.com/model_catalog",
    ],
    "apk-yuanbao": [
        "https://yuanbao.tencent.com/api/models",
    ],
}

# 倍率字段名候选（不同版本接口字段不一样，全都试）
_RATE_KEYS = ("credits", "credit", "credits_per_token", "creditRate",
              "credit_rate", "rate", "multiplier", "ratio",
              "creditsMultiplier", "credits_multiplier", "creditCost",
              "price", "pricing", "cost")
_NAME_KEYS = ("name", "displayName", "display_name", "title", "label")
_CTX_KEYS = ("contextLength", "context_length", "context", "ctx",
             "maxContext", "max_context", "contextWindow")
_OUT_KEYS = ("maxOutput", "max_output", "outputLimit", "output",
             "maxTokens", "max_tokens")
_RATE_NAMES = {"credits", "credit", "credits_per_token", "creditrate",
               "credit_rate", "rate", "multiplier", "ratio",
               "creditsmultiplier", "credits_multiplier", "creditcost",
               "price", "pricing", "cost"}


def _walk_models(node, out, depth=0):
    """递归找模型数组（接口可能包在 data.models / models / list 里）"""
    if depth > 5 or not isinstance(node, (dict, list)):
        return
    if isinstance(node, list):
        for x in node:
            if isinstance(x, dict) and ("id" in x or "model" in x or "modelId" in x):
                out.append(x)
            else:
                _walk_models(x, out, depth + 1)
        return
    for k, v in node.items():
        if k.lower() in ("models", "model_list", "modellist", "list",
                         "items", "entries", "data") and isinstance(v, list):
            for x in v:
                if isinstance(x, dict):
                    out.append(x)
        else:
            _walk_models(v, out, depth + 1)


def _find_rate(item):
    """在模型条目里找倍率：直接字段 → 嵌套 dict → 递归"""
    def key_hit(d):
        for k, v in d.items():
            if k.lower() in _RATE_NAMES and isinstance(v, (int, float, str)):
                return v
        return None
    hit = key_hit(item)
    if hit is not None:
        return hit
    for v in item.values():
        if isinstance(v, dict):
            hit = key_hit(v)
            if hit is not None:
                return hit
    return None


def _first(item, keys, default=""):
    for k in keys:
        if k in item and item[k] not in (None, ""):
            return item[k]
    return default


def fetch_model_catalog(accounts, platform, account_id=None):
    """
    登录后拉取模型目录（含倍率）。
    凭据来源：账号池 → 网关凭据目录（workbuddy.json / cn/ / intl/）。
    返回 (ok, {"models":[{id,name,rate,ctx,out,source}], "raw_auth_error":...})
    """
    import os
    base_urls = MODEL_CATALOGS.get(platform)
    if not base_urls:
        return False, "平台 %s 未配置模型目录接口" % platform

    secret, acct_name = "", ""
    if accounts:
        if account_id:
            a = accounts.get(account_id)
            if a:
                secret, acct_name = a.get("secret", ""), a.get("name", "")
        if not secret:
            usable = accounts.usable(platform)
            if usable:
                secret, acct_name = usable[0].get("secret", ""), usable[0].get("name", "")
        # 网关二维码登录的凭据在网关自己的文件里，账号池可能没有
        if not secret:
            secret = _secret_from_gateway_files()

    headers = {"User-Agent": UA, "Accept": "application/json"}
    if secret:
        # Cookie 形态（含 =）用 Cookie 头，否则 Bearer
        if "=" in secret[:60] and " " not in secret[:60]:
            headers["Cookie"] = secret
        else:
            headers["Authorization"] = "Bearer " + secret
        headers["X-Client"] = "aigw-panel"

    last = ""
    for base in base_urls:
        req = urllib.request.Request(base, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=15, context=_ssl_ctx()) as r:
                raw = r.read().decode("utf-8", "replace")
                code = r.status
        except urllib.error.HTTPError as e:
            body = e.read().decode("utf-8", "replace")[:200]
            last = "HTTP %s：%s" % (e.code, body)
            if e.code in (400, 401, 403):
                continue   # 换下一个 URL / 记录原因
            continue
        except Exception as e:
            last = "%s: %s" % (type(e).__name__, e)
            continue
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            last = "返回非 JSON"
            continue
        found = []
        _walk_models(data, found)
        models = []
        for it in found:
            mid = _first(it, ("id", "modelId", "model", "model_id"), "")
            if not mid:
                continue
            models.append({
                "id": str(mid),
                "name": str(_first(it, _NAME_KEYS, mid)),
                "rate": _find_rate(it),
                "ctx": _first(it, _CTX_KEYS, ""),
                "out": _first(it, _OUT_KEYS, ""),
                "source": "线上目录",
            })
        if models:
            return True, {"platform": platform, "account": acct_name,
                          "endpoint": base, "models": models, "total": len(models)}
        last = "接口可达但未解析出模型（结构可能变化）"

    return False, ("拉取失败（可能未登录或凭据无效）：%s" % (last or "无可用端点"))


def _secret_from_gateway_files():
    """从网关凭据目录找 token/cookie（二维码登录成功后网关会写文件）"""
    import os
    import glob
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    root = os.path.dirname(here)          # aigw-panel 的上级（工作区根）
    cands = []
    for d in (root, os.path.join(root, "cn"), os.path.join(root, "intl")):
        cands += glob.glob(os.path.join(d, "workbuddy*.json"))
        cands += glob.glob(os.path.join(d, "*.json"))
    token_keys = ("token", "access_token", "accessToken", "apiKey", "api_key",
                  "credential", "cookie", "session")
    for p in cands:
        if "settings" in os.path.basename(p) or "status" in os.path.basename(p) \
                or "models-cache" in os.path.basename(p) or "apikeys" in os.path.basename(p):
            continue
        try:
            with open(p, "r", encoding="utf-8", errors="replace") as f:
                d = json.load(f)
        except Exception:
            continue
        stack = [d]
        while stack:
            cur = stack.pop()
            if isinstance(cur, dict):
                for k, v in cur.items():
                    if k in token_keys and isinstance(v, str) and len(v) > 16:
                        return v
                    if isinstance(v, (dict, list)):
                        stack.append(v)
            elif isinstance(cur, list):
                stack.extend(x for x in cur if isinstance(x, (dict, list)))
    return ""


# ================================================================ 端点动态探测
# 为什么需要这个：
#   有些平台是「整站 /api 前缀统一鉴权中间件」，所有路径都返回 401，
#   包括你随便编的路径。实测样本：
#     www.doubao.com/api/v1/{models,chat/completions,zzz}  → 全 401
#     www.qoder.com/api/v1/{credits,user,zzz}             → 全 401
#   这种情况下 404 探测法完全失效，**401 不能当作「接口存在」的证据**。
#
# 解法：带登录凭据后再扫一遍，并额外发一个「基准请求」
#   （一个肯定不存在的随机路径）拿到该站的「整站 401 指纹」。
#   只有当某路径的响应与基准**不同**时，才认为它是真实端点。

PROBE_CANDIDATES = {
    "apk-coze": [
        "/v3/chat", "/v3/chat/retrieve", "/v3/chat/cancel",
        "/v3/chat/message/list", "/v3/workflow/run", "/v3/file/upload",
    ],
    "apk-doubao": [
        "/api/v1/user/info", "/api/v1/checkin", "/api/v1/sign_in",
        "/api/v1/points", "/api/v1/user/credit", "/api/v1/credit/balance",
        "/api/v1/user/sign", "/api/v1/task/list", "/api/v1/user/task",
        "/api/v1/models", "/api/v1/chat/completions",
    ],
    "apk-qwenwork": [
        "/api/v1/points", "/api/v1/credit", "/api/v1/user/credits",
        "/api/v1/checkin", "/api/v1/sign_in", "/api/v1/user/info",
        "/api/v1/task/list", "/api/v1/models",
    ],
    "apk-qoder": [
        "/api/v1/credits", "/api/v1/user", "/api/v1/usage", "/api/v1/models",
        "/api/v1/checkin", "/api/v1/events/100credits", "/api/v1/claim",
    ],
    "apk-kuku": [
        "/api/v1/points", "/api/v1/credit", "/api/v1/checkin",
        "/api/v1/user/info", "/api/v1/sign_in", "/api/v1/task/list",
    ],
    "apk-wps": [
        "/api/v1/points", "/api/v1/ai/points", "/api/v1/checkin",
        "/api/v1/user/info", "/api/v1/ai/user",
    ],
    "apk-nano": [
        "/api/v1/points", "/api/v1/credit", "/api/v1/checkin",
        "/api/v1/user/info", "/api/v1/sign_in",
    ],
    "apk-metaso": [
        "/api/v1/points", "/api/v1/user/info", "/api/v1/checkin",
    ],
    # ---------- 国内网页版 / 客户端对话式 AI ----------
    # 这些站大多整站鉴权（实测整段 /api 全部 401），必须登录后用探测器扫。
    # 候选表按「官方文档 + 常见命名」给出，扫到真端点后再固化进 ACTIONS。
    "apk-chatglm": [
        "/api/v1/user/info", "/api/v1/chat", "/api/v1/models",
        "/api/v1/points", "/api/v1/checkin", "/api/chat",
        "/api/user/info", "/api/chat/stream", "/backend-api/chat",
    ],
    "apk-qwen": [
        "/api/v1/chat", "/api/v1/models", "/api/v1/user/info",
        "/api/v1/points", "/api/v1/checkin", "/api/chat",
        "/api/user/info", "/api/v1/modes", "/api/v1/conversation",
    ],
    "apk-kimi": [
        "/api/v1/chat", "/api/v1/models", "/api/v1/user/info",
        "/api/v1/points", "/api/v1/checkin", "/api/chat",
        "/api/user/info", "/api/v1/agents", "/api/v1/file",
    ],
    "apk-ernie": [
        "/api/v1/chat", "/api/v1/models", "/api/v1/user/info",
        "/api/v1/points", "/api/v1/checkin", "/api/chat",
        "/api/user/info", "/api/conversation",
    ],
    "apk-nanobot": [
        "/api/v1/chat", "/api/v1/models", "/api/v1/user/info",
        "/api/v1/points", "/api/v1/checkin", "/api/chat",
    ],
}


def _http_probe(url, method, secret, timeout=12):
    """只回 (status, body_head)，不抛异常"""
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
        with urllib.request.urlopen(req, timeout=timeout,
                                    context=_ssl_ctx()) as r:
            return r.status, r.read().decode("utf-8", "replace")[:200]
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")[:200]
    except Exception as e:
        return 0, "%s: %s" % (type(e).__name__, str(e)[:80])


def probe_endpoints(accounts, platform, account_id=None, paths=None):
    """
    登录凭据后动态扫端点。
    返回 {
       platform, base, gated: bool,     # gated=整站 401（探测法失效）
       baseline: {...},                  # 基准请求（随机不存在路径）的响应
       found: [{path, method, code, body}],
       scanned: N, note: str
    }
    """
    base = BASE.get(platform)
    if not base:
        return {"ok": False, "message": "平台 %s 未配置基址" % platform}

    cands = paths or PROBE_CANDIDATES.get(platform) or []
    if not cands:
        return {"ok": False,
                "message": "平台 %s 没有候选端点表" % platform}

    secret = ""
    if accounts:
        a = accounts.get(account_id) if account_id else None
        if a:
            secret = a.get("secret", "")
        if not secret:
            u = accounts.usable(platform)
            if u:
                secret = u[0].get("secret", "")
    if not secret:
        return {"ok": False, "code": "no_credential",
                "message": "先在「账号登录」里给 %s 存一份凭据，"
                           "否则整站 401 的平台无法区分真实端点" % platform}

    # 基准：故意用一条几乎不可能存在的路径
    import time as _t
    base_path = ""
    for c in cands:
        seg = c.rsplit("/", 1)[0]
        if len(seg) > len(base_path):
            base_path = seg
    if not base_path:
        base_path = ""
    bcode, bbody = _http_probe(
        base.rstrip("/") + base_path + "/__aigw_probe_%d__" % int(_t.time()),
        "GET", secret)
    baseline = {"path": base_path + "/__aigw_probe__", "code": bcode,
                "body": bbody[:120]}
    # 整站鉴权指纹：基准也是 401/403
    gated = bcode in (401, 403)

    found = []
    for p in cands:
        methods = ["GET", "POST"] if p not in (
            "/api/v1/chat/completions", "/v3/chat") else ["POST"]
        for m in methods:
            code, body = _http_probe(base.rstrip("/") + p, m, secret)
            if code == 0:
                continue
            # 404 = 真不存在（对路径级路由的站点可信）
            if code == 404:
                continue
            # 与基准同码同文 = 整站拦截，不算命中
            if gated and code == bcode and body[:60] == bbody[:60]:
                continue
            found.append({"path": p, "method": m, "code": code,
                          "body": body[:160]})
            break          # 同一路径 GET 命中就不用再试 POST

    return {
        "ok": True, "platform": platform, "base": base, "gated": gated,
        "baseline": baseline, "found": found, "scanned": len(cands),
        "note": ("该站整站鉴权（基准路径也是 %d），已用「与基准不同」判定真实端点"
                 % bcode) if gated else "该站为路径级路由，404 可直接判定不存在",
    }


def catalog_sources():
    """
    「模型来源」下拉框的数据源。

    用户要的是「我登录了哪些平台，各有哪些模型」，
    所以这里列**所有可登录的平台**，并逐个标注能力：
      has_models  该平台有 models 接口，能直接列模型
      has_catalog 该平台能拉线上倍率目录
    两者都没有的标 can_list=False，前端会明确说「该平台不提供模型清单接口」，
    而不是让人点了以后才报错。
    """
    from .gwlogin import PLATFORMS
    out = []
    for pid, spec in PLATFORMS.items():
        m_act = action_spec(pid, "models")
        c_urls = list((MODEL_CATALOGS or {}).get(pid) or [])
        if not m_act and not c_urls:
            continue                       # 既没 models 也没目录，别列出来凑数
        out.append({
            "platform": pid,
            "name": spec.get("name") or pid,
            "has_models": bool(m_act),
            "has_catalog": bool(c_urls),
            "can_list": bool(m_act),
            "urls": c_urls,
            "has_account": False,
        })
    return out


def mark_catalog_accounts(items, accounts):
    """标出哪些平台在账号池里有可用凭据（决定能不能真拉到数据）"""
    for it in items or []:
        try:
            it["has_account"] = bool(accounts and accounts.usable(it["platform"]))
        except Exception:
            it["has_account"] = False
    return items


# ================================================================ 本机上游探活
# 用户会把调研到的反代项目（CLIProxyAPI / claude-code-router / Trae2api-cn …）
# 部署到本机。面板要能一眼看出「哪些真在跑、哪些只是纸面档案」，
# 否则档案页全是绿的，实际一个都没起，排查起来很浪费时间。
LOCAL_PROBES = {
    "cliproxyapi":      {"url": "http://127.0.0.1:8318/v1/models", "key": "aigw-local-key"},
    "claude-code-router": {"url": "http://127.0.0.1:3456/v1/models", "key": ""},
    "trae2api-cn":      {"url": "http://127.0.0.1:8000/v1/models", "key": ""},
    "workbuddy-manager": {"url": "http://127.0.0.1:8080/v1/models", "key": ""},
}


def probe_local_upstreams(timeout=6):
    """
    探活本机部署的反代上游。
    返回 [{id, url, online, code, models, ms, note}]
    online 的判据：拿到 2xx 且能解析出模型列表（空列表也算在线 —— 说明服务活着只是没挂账号）
    """
    import time as _t
    out = []
    for pid, cfg in LOCAL_PROBES.items():
        url = cfg["url"]
        item = {"id": pid, "url": url, "online": False, "code": 0,
                "models": 0, "ms": 0, "note": ""}
        headers = {"Accept": "application/json", "User-Agent": UA}
        if cfg.get("key"):
            headers["Authorization"] = "Bearer " + cfg["key"]
        req = urllib.request.Request(url, headers=headers)
        t0 = _t.time()
        try:
            with urllib.request.urlopen(req, timeout=timeout,
                                        context=_ssl_ctx()) as r:
                raw = r.read().decode("utf-8", "replace")
                item["code"] = r.status
                item["ms"] = int((_t.time() - t0) * 1000)
                try:
                    d = json.loads(raw)
                    data = d.get("data") if isinstance(d, dict) else None
                    if isinstance(data, list):
                        item["models"] = len(data)
                        item["online"] = True
                        item["note"] = ("在线，%d 个模型" % len(data)) if data \
                            else "在线，但还没登录任何账号（模型列表为空）"
                    else:
                        item["online"] = True
                        item["note"] = "在线（响应不是标准模型列表）"
                except json.JSONDecodeError:
                    item["online"] = True
                    item["note"] = "在线（非 JSON 响应）"
        except urllib.error.HTTPError as e:
            item["code"] = e.code
            item["ms"] = int((_t.time() - t0) * 1000)
            if e.code == 401:
                item["note"] = "服务在跑，但 API Key 不对（面板配的 key 与它不一致）"
            elif e.code == 404:
                item["note"] = "端口有响应但没有这个路径，可能不是这个服务"
            else:
                item["note"] = "HTTP %d" % e.code
        except Exception as e:
            item["ms"] = int((_t.time() - t0) * 1000)
            item["note"] = "未运行（%s）" % type(e).__name__
        out.append(item)
    return out
