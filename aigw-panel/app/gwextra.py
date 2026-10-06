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

# WPS 系三个域（2026-10-04 抓包实测）：
#   drive.kdocs.cn     → 账号信息 /api/v3/userinfo
#   lingxi.kdocs.cn    → WPS AI（灵犀）：智点、模型、套餐
#   vas.wps.cn         → 商城商品
#   tiance.wps.cn      → 活动
WPS_DRIVE = "https://drive.kdocs.cn"
WPS_LINGXI = "https://lingxi.kdocs.cn"
WPS_VAS = "https://vas.wps.cn"
WPS_TIANCE = "https://tiance.wps.cn"

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
    # ⚠ 2026-10-04 全面复测：**api.trae.cn 拆两台主机这件事今天已经不成立**。
    #   逐条实测（无凭据）：
    #     api.trae.cn  /trae/api/v2/ug/checkin_credits/status   200  ← 真 API
    #     api.trae.cn  /trae/api/v2/ug/checkin_credits/claim    200  ← 真 API
    #     api.trae.cn  /trae/api/v2/pay/user_current_entitlement_list  401 ← 真 API
    #     api.trae.cn  /trae/api/v2/pay/ide_user_ent_usage      401  ← 真 API
    #     www.trae.com.cn  所有上述路径                        全 <!DOCTYPE html> ← 是网页不是 API
    #     api.trae.cn  /cloudide/api/v3/trae/GetUserInfo       404
    #     api.trae.cn  /userinfo/query/baseInfo                HTML
    #     api.trae.cn  /buddy/quota                            HTML
    #     api.trae.cn  /login/phone/sendMsgCode                HTML
    #   结论：活的只有挂在 /trae 前缀下的 4 条；用户信息/配额/手机登录
    #        那一批已经**退化成网页路由或 404**，原表标 verified 是误判。
    #        已全部降级为 unverified，前端灰掉，别再拿它们当验活/签到依据。
    "apk-trae": [
        # —— 活着的一组（api.trae.cn，/trae 前缀）——
        {"id": "checkin_status", "name": "签到状态", "method": "GET",
         "path": "/trae/api/v2/ug/checkin_credits/status", "verified": True},
        {"id": "checkin_claim", "name": "领取签到积分", "method": "POST",
         "path": "/trae/api/v2/ug/checkin_credits/claim", "verified": True},
        {"id": "entitlement", "name": "当前权益套餐", "method": "GET",
         "path": "/trae/api/v2/pay/user_current_entitlement_list", "verified": True},
        {"id": "usage", "name": "用量账单", "method": "GET",
         "path": "/trae/api/v2/pay/ide_user_ent_usage", "verified": True},
        {"id": "usage_web", "name": "网页版用量", "method": "GET",
         "path": "/trae/api/v2/pay/web_user_ent_usage", "verified": True},
        # —— 已退化的一组（2026-10-04 实测返回 HTML 或 404）——
        # APP 本地路由（连公网 www.trae.ai 也 404），前端应灰掉
        {"id": "points_activation", "name": "积分激活", "method": "POST",
         "path": "/api/v1/points/activation", "base": TRAE_WEB,
         "local": True, "unverified": True},
        {"id": "redeem", "name": "兑换码兑换", "method": "POST",
         "path": "/api/v1/points/redemption-codes/redeem", "need": ["code"],
         "base": TRAE_WEB, "local": True, "unverified": True},
        {"id": "team_points", "name": "团队积分", "method": "GET",
         "path": "/api/v1/team-points/balance", "base": TRAE_WEB,
         "local": True, "unverified": True},
        # 以下 5 条实测全部退化成 HTML/404，只留作存档
        {"id": "points_balance", "name": "积分余额", "method": "GET",
         "path": "/v2/billing/meter/get-user-resource", "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 实测返回 HTML，不是 API"},
        {"id": "growth", "name": "成长活动", "method": "GET",
         "path": "/v2/activity/growth", "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 实测返回 HTML，不是 API"},
        {"id": "user_info", "name": "用户基础信息", "method": "GET",
         "path": "/userinfo/query/baseInfo", "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 实测返回 HTML"},
        {"id": "user_info_cloud", "name": "云端用户信息", "method": "GET",
         "path": "/cloudide/api/v3/trae/GetUserInfo", "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 www 域返 HTML / api 域 404"},
        {"id": "quota", "name": "剩余配额", "method": "GET",
         "path": "/buddy/quota", "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 实测返回 HTML"},
        {"id": "login_send_code", "name": "手机登录·发验证码", "method": "POST",
         "path": "/login/phone/sendMsgCode", "need": ["phone"], "base": TRAE_WEB,
         "unverified": True, "stale_note": "2026-10-04 实测返回 HTML"},
        {"id": "login_check_code", "name": "手机登录·校验码", "method": "POST",
         "path": "/login/phone/checkCode", "need": ["phone", "code"],
         "base": TRAE_WEB, "unverified": True,
         "stale_note": "2026-10-04 实测返回 HTML"},
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
        # 2026-10-04 实测：/model_catalog 与 /refresh 返回的是 **HTML 页面**
        # （SPA 前端路由兜底），不是 JSON API —— 拿它当模型目录会把 HTML
        # 当 JSON 解析直接报错。小浣熊真正的 JSON 接口都在 /api/web/... 下。
        {"id": "model_catalog", "name": "模型目录", "method": "GET",
         "path": "/model_catalog", "unverified": True,
         "stale_note": "2026-10-04 实测返回 HTML 页面（SPA 路由），不是 JSON API"},
        {"id": "refresh", "name": "刷新凭据", "method": "POST",
         "path": "/refresh", "unverified": True,
         "stale_note": "2026-10-04 实测返回 HTML 页面（SPA 路由），非接口"},
    ],
    "apk-codebuddy": [
        # —— 以下标记说明来自 2026-10-04 的端点审计（audit_endpoints.py）——
        # 方法：对同一站发一条「肯定不存在的随机路径」作基准，
        #       只有响应与基准**不同**才判定端点真实存在。
        # 标 unverified 的路径返回的 401 与基准**完全一致**（该站 Nginx 整站鉴权），
        # 因此无法证伪也无法证实 —— 保留但不要当成已验证。
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/console/enterprises/personal/models", "verified": True},
        # 2026-10-04 全量无凭据扫描：POST 返回 401（需登录），说明端点真实存在
        # → 从 unverified 改标 verified（之前标 unverified 是沿用早期猜测）。
        {"id": "checkin", "name": "每日签到", "method": "POST",
         "path": "/v2/billing/meter/daily-checkin", "verified": True},
        # ⚠ 2026-10-04 更正：get-user-resource **必须 POST**，GET 是 404
        #   （路由不匹配 GET 方法）。POST + body={} → 200 能拿到 47 条资源明细。
        {"id": "points_balance", "name": "积分余额", "method": "POST",
         "path": "/v2/billing/meter/get-user-resource", "verified": True},
        # ⚠ 已下线：逆向登记里它是 404，本轮扫描 www.codebuddy.ai 返 401（整站
        #   鉴权，无法证伪）。两种观测都说明**拿不到数据**，别当可用接口。
        {"id": "usage_summary", "name": "用量摘要", "method": "GET",
         "path": "/billing/meter/get-user-resource-summary", "unverified": True,
         "stale_note": "2026-10-04 逆向登记为 404 已下线；本轮扫到 401（整站鉴权），拿不到数据"},
        # 官方直连的二维码登录轮询口，实测 GET 200：
        #   {"code":11217,"msg":"11217:login ing..."} 表示已发起、待扫码
        # 同名 /v2/plugin/auth/state 已下线（404），不要再加回来。
        {"id": "auth_token", "name": "官方登录轮询", "method": "GET",
         "path": "/v2/plugin/auth/token", "base": "https://copilot.tencent.com",
         "verified": True},
        # ⚠ 2026-10-04 更正：这个端点**没有 404，之前是误判**（当时方法用错了）。
        #   正确用法：refreshToken 放 **header `X-Refresh-Token`**，
        #   body 传 {} 即可。放 body / query 都报 400「refreshToken is empty」。
        #   带真 token 实测 200 → 返回新的 accessToken + refreshToken。
        #   实现见 app/tlogin.py::refresh()（不要在 gwextra 里重写一遍）。
        {"id": "auth_refresh", "name": "刷新 token", "method": "POST",
         "path": "/v2/plugin/auth/token/refresh",
         "base": "https://copilot.tencent.com", "verified": True,
         "header_token": "X-Refresh-Token", "need": ["refreshToken"]},
        {"id": "login_account", "name": "账密登录", "method": "POST",
         "path": "/v2/plugin/login/account", "unverified": True,
         "need": ["username", "password"],
         "note": "2026-10-04 扫描返 401（需登录态）；面板主路走二维码，此口仅备用"},
        {"id": "invite_bind", "name": "邀请码绑定", "method": "POST",
         "path": "/activity/workbuddy/invitation/v2/bind", "unverified": True,
         "need": ["code"],
         "note": "2026-10-04 扫描返 401（需登录态）；需 body 带 invite_code"},
    ],
    "apk-doubao": [
        {"id": "user_info", "name": "账号信息", "method": "GET",
         "path": "/api/v1/user/info", "verified": True},
        # ⚠ 2026-10-04 更正：之前注释写「/samantha/chat/completion 已下线」是**误判**。
        # ★★ 2026-10-04 开浏览器实测补的模型清单接口。
        #   用户说得对：「网页版里面也是有模型选项的」—— 抓包确认页面确实加载了
        #   `s2-model-select-v2-entry.js`（模型选择组件）。豆包真实接口前缀是
        #   **/alice/**（不是 /api/v1/*），模型/启动配置就在 /alice/basic/launch。
        #   ⚠ 无登录态时它返 404（路由需带完整 query 与登录态），所以标
        #   unverified —— 登录后才能真正拿到模型数组。
        {"id": "models", "name": "模型清单", "method": "GET",
         "path": "/alice/basic/launch", "unverified": True,
         "query": {"version_code": "20800", "language": "zh",
                   "device_platform": "web", "doubao_device_platform": "web",
                   "aid": "497858", "real_aid": "497858"},
         "stale_note": "2026-10-04 抓包确认的真路径（/alice/*）；无登录态返 404，需登录后取"},
        {"id": "user_config", "name": "用户配置（验活）", "method": "GET",
         "path": "/alice/user/config/pull", "verified": True,
         "query": {"version_code": "20800", "language": "zh",
                   "device_platform": "web", "doubao_device_platform": "web",
                   "aid": "497858", "real_aid": "497858"}},
        #   实测两个路径行为完全不同：
        #     POST /samantha/chat/completion → 200 + 结构化业务错误
        #       event: gateway-error  {"code":"Internal",
        #        "message":"ErrorX:code=710012000 ..."}   ← 路由活着，报的是鉴权/参数
        #     POST /v1/chat/completions      → 200 + <!DOCTYPE html>…  ← 落到官网页面，**不是 API**
        #   结论：豆包对话真路径是 /samantha/chat/completion（与 verify2.txt 一致），
        #        /v1/chat/completions 是错的（返回 HTML，等于没命中后端）。
        #   710012000 = 缺凭据/参数不对；带上 Cookie 后应该能进正常对话。
        {"id": "chat", "name": "对话补全", "method": "POST",
         "path": "/samantha/chat/completion", "verified": True,
         "need": ["messages"]},
        # 备选：某些客户端版本走 OpenAI 兼容路径，保留但默认不验证
        # 2026-10-04 实测：www.doubao.com 对 /v1/* 返回 **HTML 页面**。
        # /v1/chat/completions、/v1/images/generations 是 dev.doubao2api
        # **APK 本地网关**自己的路由（base(1).apk 里扫到的就是这三个），
        # 不是豆包云端 API。云端真正的对话口是 /samantha/chat/completion。
        # 注意：image_gen 原先被标 verified=True，实测为 HTML，已降级。
        {"id": "chat_openai", "name": "对话补全（OpenAI 兼容）", "method": "POST",
         "path": "/v1/chat/completions", "need": ["messages"],
         "unverified": True,
         "stale_note": "云端无此路径（返 HTML）；/v1/* 是 APK 本地网关路由"},
        {"id": "image_gen", "name": "文生图", "method": "POST",
         "path": "/v1/images/generations", "need": ["prompt"],
         "unverified": True,
         "stale_note": "2026-10-04 实测返 HTML，非云端接口（原误标 verified）"},
    ],
    "apk-yuanbao": [
        # 实测 2026-10-04：/api/models 返回 401（存在，只是要鉴权），
        # 而 /v1/models 与 /health 返回的是 SPA 的 HTML —— 别被 200 骗了。
        # 之前漏了 models 这条，所以「已登录却拿不到模型」。
        # ★ 2026-10-04 开浏览器抓包实证：元宝真实的模型列表接口是
        #   /api/agent/model/list（浏览器自己发的就是这个），不是 /api/models。
        {"id": "models", "name": "模型列表", "method": "GET",
         "path": "/api/agent/model/list"},
        {"id": "models_legacy", "name": "模型列表（旧路径）", "method": "GET",
         "path": "/api/models", "verified": True,
         "note": "实测 401 存在；抓包显示现在走 /api/agent/model/list"},
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
         "path": "/v1internal:listModels", "needs_proxy": True,
         "unverified": True,
         "stale_note": "2026-10-04 实测 GET/POST 均 404，路径未证实"},
    ],
    # ================================================== CodeBuddy 国内站
    # 实测：www.codebuddy.cn/console/enterprises/personal/models → 400「未找到 cookie」
    #      www.codebuddy.cn/v2/billing/meter/get-user-resource → 401
    "apk-codebuddy-cn": [
        {"id": "models", "name": "模型目录", "method": "GET",
         "path": "/console/enterprises/personal/models"},
        {"id": "checkin", "name": "每日签到", "method": "POST",
         "path": "/v2/billing/meter/daily-checkin"},
        # ⚠ 同 apk-codebuddy：必须 POST。GET 在 www.codebuddy.cn 上是 404，
        #   POST 同路径 → 401（端点存在，只是要凭据）。
        {"id": "points_balance", "name": "积分余额", "method": "POST",
         "path": "/v2/billing/meter/get-user-resource", "verified": True},
        {"id": "usage_summary", "name": "用量摘要", "method": "GET",
         "path": "/billing/meter/get-user-resource-summary"},
        # refreshToken 走 header X-Refresh-Token，实现见 app/tlogin.py::refresh()
        {"id": "auth_refresh", "name": "刷新 token", "method": "POST",
         "path": "/v2/plugin/auth/token/refresh", "verified": True,
         "header_token": "X-Refresh-Token", "need": ["refreshToken"]},
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
    # ✓ 全部来自 2026-10-04 登录后 CDP 抓包（data/kuku_wps_capture.jsonl）。
    #   之前这里填的是 6 个**瞎猜的**路径（/api/v1/points、/api/v1/user/info…），
    #   各试 10 个候选全 404 —— 真接口长得完全不一样，是 `/api/genflowpro/*` 和
    #   `/wenchain/genflow*` 两套前缀。
    #
    # ★ 关键：库库的登录态标志**不是 HTTP 401**，而是响应体里的
    #     errno = -6 且 show_msg = "未登录" / "need login"。
    #     200 + errno:-6 = 没登录；errno:0 = 登录了。别按状态码判。
    #   凭据：kuku.baidu.com 域下的 Cookie（关键键 XFT / XFI / XFS），
    #        走 passport.baidu.com 统一登录（BDUSS 不下发给 kuku）。
    "apk-kuku": [
        # —— 用户信息（抓包实测：未登录时 200 + errno:-6 + 空 nickname）——
        {"id": "profile", "name": "用户资料", "method": "GET",
         "path": "/api/genflowpro/settings/profile", "verified": True,
         "ok_keys": ["nickname", "account", "avatar_url", "phone"]},
        {"id": "userreport", "name": "用户上报", "method": "GET",
         "path": "/api/genflowpro/common/userreport"},
        {"id": "entcert", "name": "企业认证", "method": "GET",
         "path": "/tobgfp/entcert/get", "verified": True,
         "note": "未登录时 show_msg=need login"},
        {"id": "device_list", "name": "设备列表", "method": "GET",
         "path": "/api/genflowpro/device/getdevicelist"},
        # —— 免费积分（签到/任务/首页活动）——
        # ⚠ taskstatus 和 homenew 都**要 query 参数**，无参返 400 params error；
        #    taskstatus 缺 task_key 会报 Go validator 错误（说明要带 task_key）
        {"id": "freepoint_taskstatus", "name": "积分任务状态", "method": "GET",
         "path": "/api/genflowpro/freepoint/taskstatus", "verified": True,
         "note": "需 query: task_key（缺失报 validator 错）"},
        {"id": "freepoint_homenew", "name": "积分首页活动", "method": "GET",
         "path": "/api/genflowpro/freepoint/homenew", "verified": True,
         "note": "需 query 参数，无参返 400 params error"},
        # ★ 会话参数（2026-10-04 从 JS bundle 反编译 + 实测确认）
        #   页面启动先打这个接口拿 bdstoken/uinfo/uk，所有 genflowpro 接口都带。
        #   GET/POST 都行，登录态必返 data:{bdstoken,uinfo,uk}（bdstoken 会话级会转）。
        {"id": "session_params", "name": "会话参数(取bdstoken)", "method": "GET",
         "path": "/api/genflowpro/common/userreport",
         "query": {"clienttype": "400", "app_id": "123971023", "web": "1",
                   "channel": "chunlei", "version": "1.6.8"},
         "verified": True, "ok_keys": ["bdstoken", "uinfo", "uk"],
         "note": "返回 bdstoken/uinfo/uk，签到/积分接口的必需 query"},
        # ★ 签到领取（2026-10-04 抓包实测真接口）
        #   POST /api/genflowpro/freepoint/taskComplete
        #     ?bdstoken=<必需，从 cookie XFT 派生>&clienttype=400
        #     &app_id=123971023&web=1&channel=chunlei&version=1.6.8&uinfo=<uinfo>
        #   body: task_type=LOGIN
        #   响应: {"complete_status":"SUCCESS","reward_point":50}
        # ⚠ 缺 bdstoken 会失败 —— 这是百度系的 CSRF token
        {"id": "checkin_claim", "name": "每日签到领积分", "method": "POST",
         "path": "/api/genflowpro/freepoint/taskComplete", "verified": True,
         "need_query": ["bdstoken", "uinfo", "clienttype", "app_id",
                        "web", "channel", "version"],
         "body_form": {"task_type": "LOGIN"},
         "note": "任务表: daily_login+50 / daily_chat+50 / "
                 "daily_invite_visit+150 / daily_invite_download+300"},
        {"id": "activity_task_complete", "name": "活动任务完成", "method": "POST",
         "path": "/api/genflowpro/activity/taskcomplete",
         "note": "从 JS bundle 挖到，未实测"},
        # —— 模型 / 配置 ——
        {"id": "model_list", "name": "模型清单", "method": "GET",
         "path": "/wenchain/genflowpro/model/list", "verified": True,
         "ok_keys": ["model_list"], "note": "免登录可读"},
        {"id": "model_list2", "name": "模型清单（生成链）", "method": "GET",
         "path": "/wenchain/genflow/model/list", "unverified": True,
         "stale_note": "2026-10-04 实测 404；正确路径是 /wenchain/genflowpro/model/list"},
        {"id": "skill_list", "name": "技能清单", "method": "POST",
         "path": "/wenchain/genflowpro/skill/list", "verified": True},
        {"id": "skill_category", "name": "技能分类", "method": "POST",
         "path": "/wenchain/genflowpro/skill/category/list", "verified": True},
        {"id": "skill_mine", "name": "我的技能", "method": "POST",
         "path": "/wenchain/genflowpro/skill/mine", "verified": True,
         "note": "未登录返 code:401 请登录后使用"},
        {"id": "chat_alloc", "name": "会话分配", "method": "POST",
         "path": "/wenchain/genflow/idallochstr", "verified": True},
        {"id": "layout", "name": "工作台布局", "method": "GET",
         "path": "/api/genflowpro/workspace/getlayout", "verified": True},
        {"id": "project_list", "name": "项目列表", "method": "GET",
         "path": "/api/genflowpro/workspace/getprojectnamelist"},
        {"id": "act_conf", "name": "活动配置", "method": "GET",
         "path": "/act/api/conf", "verified": True, "note": "免登录可读"},
        {"id": "ping", "name": "连通性", "method": "GET",
         "path": "/api/genflowpro/ping", "verified": True},
    ],
    # ✓ 全部来自 2026-10-04 登录后 CDP 抓包（data/wps_ai_api.json）
    # ⚠ WPS AI（灵犀）**不在 www.wps.cn**，独立域 lingxi.kdocs.cn。
    #   www.wps.cn 首页只有云文档接口（/api/v5/files|links|groups），
    #   AI 积分 / 模型 / 套餐全在 lingxi.kdocs.cn 下 —— 之前在 www.wps.cn 上
    #   试了 10 个候选全 404，就是因为找错了域。
    #   实测全部 200（本机账号「风归叶落」uid=357465770，赠送积分 800）。
    "apk-wps": [
        # —— 账号 ——
        {"id": "userinfo", "name": "账号信息", "method": "GET",
         "path": "/api/v3/userinfo", "base": WPS_DRIVE, "verified": True,
         "ok_keys": ["id", "name", "avatar", "status"]},
        # —— AI 智点（这是「积分/智点」那一轨）——
        {"id": "credits_balance", "name": "AI 智点余额", "method": "GET",
         "path": "/api/public/v1/credits/balance", "base": WPS_LINGXI,
         "verified": True, "ok_keys": ["balance", "bonus_credits", "enabled"],
         "note": "含赠送/消耗/过期时间（2026-10-04 实测 bonus_credits 800）"},
        # ★ 签到 / 任务（2026-10-04 实测：本机已签到，day1 claimed）
        #   GET  /api/public/v1/tasks → 全部任务 + 7 天连续签到表
        #   POST /api/public/v1/tasks/daily_check_in/complete → 领取
        #     （已签到时返 400 v7code:400000007「参数错误」，属幂等）
        #   任务实测：daily_check_in 100/天（第7天200）· try_expert_mode 200
        #            try_group_chat 400 · use_lingxi_client 800
        {"id": "tasks", "name": "任务与签到", "method": "GET",
         "path": "/api/public/v1/tasks", "base": WPS_LINGXI, "verified": True,
         "ok_keys": ["tasks", "consecutive_days", "today_day"]},
        {"id": "checkin_claim", "name": "每日签到领智点", "method": "POST",
         "path": "/api/public/v1/tasks/daily_check_in/complete",
         "base": WPS_LINGXI, "verified": True,
         "note": "已签到时返 400 v7code 400000007（幂等，非失败）"},
        {"id": "extra_grant_claim", "name": "额外额度领取", "method": "POST",
         "path": "/api/public/v1/credits/extra_grant/claim",
         "base": WPS_LINGXI, "verified": True,
         "note": "实测 200 {granted:false}（无可领）"},
        {"id": "sessions", "name": "会话", "method": "GET",
         "path": "/api/public/v1/sessions", "base": WPS_LINGXI, "verified": True},
        {"id": "cowork_profile", "name": "Cowork 资料", "method": "GET",
         "path": "/api/aioffice/v1/cowork/profile", "base": WPS_LINGXI,
         "verified": True},
        {"id": "unread_count", "name": "未读数", "method": "GET",
         "path": "/api/aioffice/v1/message_center/unread_count",
         "base": WPS_LINGXI, "verified": True},
        {"id": "subscriptions", "name": "订阅", "method": "GET",
         "path": "/api/public/v1/order/subscriptions", "base": WPS_LINGXI,
         "verified": True, "ok_keys": ["total", "list"]},
        {"id": "settings", "name": "AI 设置", "method": "GET",
         "path": "/api/public/v1/settings", "base": WPS_LINGXI, "verified": True},
        # —— AI 模型 / 套餐 ——
        {"id": "ai_models", "name": "AI 模型清单", "method": "GET",
         "path": "/api/aioffice/v1/sessions/models", "base": WPS_LINGXI,
         "verified": True, "ok_keys": ["models"],
         "note": "实测含 deepseek-v4-flash-0731 / auto 等"},
        {"id": "ai_plans", "name": "AI 套餐（Pro/Max）", "method": "GET",
         "path": "/api/aioffice/v1/sessions/plans", "base": WPS_LINGXI,
         "verified": True, "ok_keys": ["plans", "multiplier"]},
        {"id": "ai_identity", "name": "AI 助手身份", "method": "GET",
         "path": "/solution/api/aigc/v3/assistant/identity", "base": WPS_LINGXI,
         "verified": True, "ok_keys": ["corp"]},
        {"id": "ai_groups", "name": "用户组", "method": "POST",
         "path": "/api/aioffice/v1/user/groups", "base": WPS_LINGXI,
         "verified": True, "need": ["user_id"]},
        # —— 商城 / 活动（另一轨「积分」）——
        {"id": "merchandise", "name": "商品列表", "method": "GET",
         "path": "/query/api/v1/list_merchandise", "base": WPS_VAS, "verified": True},
        {"id": "market_activity", "name": "活动", "method": "POST",
         "path": "/dce/exec/api/market/activity", "base": WPS_TIANCE,
         "verified": True, "note": "需 channel_code，如 AIGW5001"},
    ],
}


def actions_of(platform):
    return ACTIONS.get(platform, [])


# ---------------------------------------------------------------- 网易 Lobster AI
# 2026-10-05 新增：协议逆向自社区项目 lobsterai2api（Go，纯 stdlib）。
# 上游：{server}/api/proxy/v1/chat/completions（OpenAI 兼容透传，Bearer accessToken）
#       {server}/api/models/available（动态模型）；{server}/api/user/profile-summary
# server 不在仓库硬编码 —— 由 设置→Lobster 上游地址 提供（动作基址 "@lobster"）。
ACTIONS["web-lobster"] = [
    {"id": "models", "name": "动态模型清单", "method": "GET",
     "path": "/api/models/available", "base": "@lobster",
     "note": "Bearer accessToken；动态清单，随官方上下线变化"},
    {"id": "profile", "name": "账号概要/积分", "method": "GET",
     "path": "/api/user/profile-summary", "base": "@lobster",
     "note": "Bearer accessToken；credits 在这里"},
]

# ================================================================ 2026-10-05 全项目复查补齐
# (1) 官方 API 平台（api-*）：15 家全是 OpenAI 兼容端点，用账号池里的 API Key
#     调 /models 就能拉实时模型清单。端点真实性已由 2026-10-04 审计证实
#     （假 Key → 401，见 app/upstreams.py OFFICIAL 注释）。
#     hint = 未配置 Key 时给用户的「官方常见模型」提示（来自 OFFICIAL.models_hint）。
_API_OFFICIAL = {
    "api-deepseek":    {"base": "https://api.deepseek.com", "name": "DeepSeek 官方",
                        "hint": ["deepseek-chat", "deepseek-reasoner"]},
    "api-moonshot":    {"base": "https://api.moonshot.cn/v1", "name": "Kimi 月之暗面",
                        "hint": ["moonshot-v1-8k", "moonshot-v1-32k", "kimi-k2-turbo-preview"]},
    "api-zhipu":       {"base": "https://open.bigmodel.cn/api/paas/v4", "name": "智谱 BigModel",
                        "hint": ["glm-4.7-flash", "glm-4-flash", "glm-4.6v-flash"]},
    "api-dashscope":   {"base": "https://dashscope.aliyuncs.com/compatible-mode/v1",
                        "name": "阿里云百炼",
                        "hint": ["qwen3-max", "qwen3-plus", "qwen3-flash", "qwen3-coder-plus"]},
    "api-bailian":     {"base": "https://dashscope.aliyuncs.com/compatible-mode/v1",
                        "name": "阿里百炼",
                        "hint": ["qwen3-max", "qwen3-plus", "qwen3-flash", "qwen3-coder-plus"]},
    "api-volcengine":  {"base": "https://ark.cn-beijing.volces.com/api/v3", "name": "火山方舟",
                        "hint": ["doubao-pro", "doubao-lite", "doubao-seed"]},
    "api-minimax":     {"base": "https://api.minimax.chat/v1", "name": "MiniMax",
                        "hint": ["abab6.5s-chat", "abab5.5s-chat"]},
    "api-stepfun":     {"base": "https://api.stepfun.com/v1", "name": "阶跃星辰",
                        "hint": ["step-2-16k", "step-1x-medium"]},
    "api-baichuan":    {"base": "https://api.baichuan-ai.com/v1", "name": "百川智能",
                        "hint": ["Baichuan4", "Baichuan4-Air"]},
    "api-siliconflow": {"base": "https://api.siliconflow.cn/v1", "name": "硅基流动",
                        "hint": ["deepseek-ai/DeepSeek-V3", "Qwen/Qwen3-32B"]},
    "api-groq":        {"base": "https://api.groq.com/openai/v1", "name": "Groq",
                        "hint": ["moonshotai/kimi-k2-instruct", "qwen/qwen3-32b"]},
    "api-openrouter":  {"base": "https://openrouter.ai/api/v1", "name": "OpenRouter",
                        "hint": ["deepseek/deepseek-chat-v3", "google/gemini-2.5-flash"]},
    "api-modelscope":  {"base": "https://api-inference.modelscope.cn/v1", "name": "魔搭 ModelScope",
                        "hint": ["Qwen/Qwen3-32B"]},
    # 讯飞两家：spark-api.xf-yun.com 是 HMAC 签名鉴权（Bearer 打不通），
    # 这里用 OpenAI 兼容、API Key 直用的星辰 MaaS。
    "api-xfyun":       {"base": "https://spark-api-open.xf-yun.com/v1", "name": "讯飞星辰 MaaS",
                        "hint": ["qwen3-coder-next", "qwen3-1.7b"]},
    "api-qianfan":     {"base": "https://qianfan.baidubce.com/v2", "name": "百度千帆",
                        "hint": ["ernie-speed-8k", "ernie-lite-8k"]},
}
for _pid, _info in _API_OFFICIAL.items():
    BASE.setdefault(_pid, _info["base"])
    ACTIONS[_pid] = [{
        "id": "models", "name": "模型清单（官方 /models）",
        "method": "GET", "path": "/models", "verified": True,
        "note": "OpenAI 兼容官方 API；账号池配 API Key 后 Bearer 拉全量，"
                "未配置时面板回退官方常见模型提示",
    }]


def api_hint_models(pid):
    """官方 API 平台未配 Key 时的兜底清单（OFFICIAL.models_hint）"""
    info = _API_OFFICIAL.get(pid)
    if not info:
        return []
    return [{"id": m, "name": m, "key": m, "desc": "官方常见模型（未验证可用性）"}
            for m in info.get("hint", [])]


def api_official_name(pid):
    return (_API_OFFICIAL.get(pid) or {}).get("name", pid)


# (2) 动作 id 别名：platform_models 只认 "models"，有些平台把真接口
#     登记成了别的 id，导致「有接口却查无模型」。
def _alias_models(pid, name, spec_template):
    """给平台补一个 id=models 的动作（指向已验证的真实路径）"""
    if any(a.get("id") == "models" for a in ACTIONS.get(pid, [])):
        return
    alias = dict(spec_template)
    alias["id"] = "models"
    alias["name"] = name
    ACTIONS[pid].append(alias)


_alias_models("apk-kuku", "模型清单",
              next(a for a in ACTIONS["apk-kuku"] if a.get("id") == "model_list"))
_alias_models("apk-wps", "AI 模型清单",
              next(a for a in ACTIONS["apk-wps"] if a.get("id") == "ai_models"))
# 库库模型清单免登录可读（实测 2026-10-04）：无凭据也照发
_m = next((a for a in ACTIONS["apk-kuku"] if a.get("id") == "models"), None)
if _m:
    _m["no_auth"] = True


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


def _request(base, spec, secret, params, timeout=20, vendor=None,
             query=None, form=False):
    """
    base+path 请求。
    - query: 追加到 URL 的静态/动态查询参数（GET/POST 都支持），用于 kuku 的
      clienttype/app_id/bdstoken/uinfo/uk 等必须走 query 的接口。
    - form:  True 时 body 用 application/x-www-form-urlencoded（kuku taskComplete
      的 task_type=LOGIN 是 form，不是 JSON）；否则 JSON。
    - spec.get("query")：动作里声明的静态 query（如 kuku 固定 clienttype/app_id）。
    """
    path = spec["path"]
    method = spec.get("method", "GET")
    url = base.rstrip("/") + path
    # 合并三层 query：spec 静态 > 动态 query 参数 > GET 的 params
    q = {}
    if isinstance(spec.get("query"), dict):
        q.update(spec["query"])
    if isinstance(query, dict):
        q.update(query)
    if method == "GET" and isinstance(params, dict):
        q.update(params)
    if q:
        url += ("&" if "?" in url else "?") + urllib.parse.urlencode(q)
    body = None
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
        if form or spec.get("body_form"):
            fb = dict(spec.get("body_form") or {})
            if isinstance(params, dict):
                fb.update(params)
            body = urllib.parse.urlencode(fb).encode("utf-8")
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
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

def call(accounts, platform, action, params=None, account_id=None,
          query=None, form=False, base_override=None):
    """
    执行某平台的一个动作。
    凭据来源优先级：指定 account_id > 该平台第一个可用账号 > 网关本地凭据
    query / form 透传给 _request（kuku 签到等需要 query+form 的接口用）。
    base_override：动作基址可由设置动态提供（web-lobster 的上游地址
    来自 设置→Lobster 上游地址，即 lobsterai2api 的 LB2A_UPSTREAM_BASE）。
    """
    spec = action_spec(platform, action)
    if not spec:
        return False, "平台 %s 不支持动作 %s" % (platform, action)
    # 个别动作挂在不同主机上（Trae 签到在 api.trae.cn，其余在 www.trae.com.cn）
    base = spec.get("base") or BASE.get(platform)
    if base == "@lobster":
        base = (base_override or "").rstrip("/")
        if not base:
            return False, ("未配置 Lobster 上游地址：设置→「Lobster 上游地址」"
                           "（即 lobsterai2api 的 LB2A_UPSTREAM_BASE，"
                           "登录页 https://lobsterai.youdao.com 登录后抓包可得）")
    elif not base:
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
        # ⚠ 二维码登录（wb-gateway / apk-trae / apk-codebuddy / apk-codebuddy-cn）
        # 存进账号池的那条 **secret 是空的**（凭据在网关里，面板只记个壳），
        # 而 usable() 会过滤掉空 secret → 这里 usable 为空 → 所有接口都报
        # 「账号池里没有可用凭据」，用户明明登录成功却什么都用不了。
        # 所以：先把「该平台有没有登录记录」单独判一次，允许空 secret 的
        # gateway 型记录通过，再走网关凭据兜底。
        rows = [a for a in (accounts.list(platform, mask=False) or [])
                if a.get("enabled", True)]
        g_acct = next((a for a in rows if a.get("type") == "gateway"), None)
        if g_acct:
            acct_name = acct_name or g_acct.get("name", "")
        usable = accounts.usable(platform)
        if usable:
            secret, acct_name = usable[0].get("secret", ""), usable[0].get("name", "")
    if not secret:
        # ★ 第三优先级「网关本地凭据」——函数名一直在这，但 call() 从没调过，
        #   文档写的优先级等于没实现。二维码登录后网关会把 token 写到
        #   workbuddy*.json，这里兜底取出来。
        try:
            g = _secret_from_gateway_files()
        except Exception:
            g = ""
        if g:
            secret, acct_name = g, (acct_name or "网关本地凭据")
    if not secret and not spec.get("no_auth"):
        # ★ no_auth 动作（如库库 model_list 实测免登录可读）没有凭据也照发，
        #   不能被凭据门挡在本地——那会把「免登录就能拿到的数据」变成报错。
        return False, ("账号池里没有 %s 的可用凭据，请先在「账号登录」页完成登录"
                       % platform)

    okk, data = _request(base, spec, secret, params or {}, vendor=platform,
                         query=query, form=form)
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
    """
    Trae 完整签到：先看状态，未签则领。

    2026-10-04 实测修正（两个坑，都会造成「没登录却显示签到成功」）：
      · 该站**无凭据时也返 HTTP 200**，只在 body 里给 code=1001 +
        message "not able to authenticate you"。只按状态码判会把「未登录」
        当成成功 —— 必须先判业务码。
      · 字段名是 **checked_in**（不是 checked_in_today），且它在**顶层**，
        未必包在 data 里。旧代码 d["data"].get("checked_in_today") 恒为
        None，于是每次都去领一次，再把 200+code:1001 的领奖响应当成功。
    """
    okk, d = call(accounts, "apk-trae", "checkin_status", {}, account_id)
    if not okk:
        return False, d
    d = d or {}
    body = d.get("data") if isinstance(d.get("data"), dict) else d
    code = body.get("code", d.get("code"))
    if code not in (0, None):
        return False, {"error": "Trae 接口拒绝：code=%s %s" % (
            code, (body.get("message") or d.get("message") or "")[:80]),
            "code": code}
    if isinstance(body, dict) and (body.get("checked_in")
                                   or body.get("checked_in_today")):
        return True, {"already": True, "data": body}

    ok2, d2 = call(accounts, "apk-trae", "checkin_claim", {}, account_id)
    if not ok2:
        return False, d2
    d2 = d2 or {}
    b2 = d2.get("data") if isinstance(d2.get("data"), dict) else d2
    c2 = b2.get("code", d2.get("code"))
    if c2 not in (0, None):
        return False, {"error": "Trae 签到未生效：code=%s %s" % (
            c2, (b2.get("message") or d2.get("message") or "")[:80]),
            "code": c2}
    return True, d2


def raccoon_login_bonus(accounts, account_id=None):
    """小浣熊登录送积分"""
    return call(accounts, "apk-raccoon", "login_bonus", {}, account_id)


def codebuddy_checkin(accounts, account_id=None):
    """CodeBuddy 每日签到（/v2/billing/meter/daily-checkin，实测 401=需登录）"""
    return call(accounts, "apk-codebuddy", "checkin", {}, account_id)


def codebuddy_points(accounts, account_id=None):
    """CodeBuddy 积分余额（/v2/billing/meter/get-user-resource）"""
    return call(accounts, "apk-codebuddy", "points_balance", {}, account_id)


# ------------------------------------------------------------------ 库库签到
# 库库签到是两段式：先打 userreport 拿会话参数（bdstoken/uinfo/uk），
# 再带这些参数打 taskComplete（form body: task_type=LOGIN）。bdstoken 是会话级、
# 会轮换，每次现取，不能存死。2026-10-04 实测 GET userreport 200 且 data 里有三者。
KUKU_FIXED = {"clienttype": "400", "app_id": "123971023", "web": "1",
              "channel": "chunlei", "version": "1.6.8"}


def kuku_session_params(accounts, account_id=None):
    """拿 bdstoken/uinfo/uk（每次现取，会话级）"""
    okk, d = call(accounts, "apk-kuku", "session_params", {}, account_id)
    if not okk:
        return False, d
    data = (d or {}).get("data") or {}
    sp = data.get("data") or {}
    bdstoken = sp.get("bdstoken")
    uinfo = sp.get("uinfo")
    uk = sp.get("uk")
    if not (bdstoken and uinfo and uk):
        return False, "userreport 未返回完整会话参数：%r" % sp
    return True, {"bdstoken": bdstoken, "uinfo": uinfo, "uk": uk}


def kuku_checkin_flow(accounts, account_id=None):
    """库库 AI 每日签到（两段式，完全自主，只需登录态 Cookie）"""
    okk, sp = kuku_session_params(accounts, account_id)
    if not okk:
        return False, sp
    q = dict(KUKU_FIXED)
    q.update({"bdstoken": sp["bdstoken"], "uinfo": sp["uinfo"], "uk": sp["uk"]})
    ok2, d2 = call(accounts, "apk-kuku", "checkin_claim",
                   {"task_type": "LOGIN"}, account_id, query=q, form=True)
    if ok2:
        data = (d2 or {}).get("data") or {}
        return True, {"summary": "库库签到成功",
                      "reward_point": data.get("reward_point"),
                      "complete_status": data.get("complete_status"),
                      "data": data}
    # 失败：可能是「今天已签」或参数过期
    body = (d2 or {}).get("body") or (d2 or {})
    return False, d2


# ------------------------------------------------------------------ WPS 签到
def wps_checkin_flow(accounts, account_id=None):
    """WPS AI 每日签到：先看 tasks 状态，未签则领（已签 400 视为幂等成功）"""
    okk, d = call(accounts, "apk-wps", "tasks", {}, account_id)
    if not okk:
        return False, d
    data = (d or {}).get("data") or {}
    tasks = data.get("tasks") or (data.get("data") or {}).get("tasks") or []
    dc = next((t for t in tasks
               if (t.get("task_key") or t.get("type")) == "daily_check_in"), None)
    if dc and str(dc.get("status")) == "claimed":
        return True, {"already": True, "data": dc}
    ok2, d2 = call(accounts, "apk-wps", "checkin_claim", {}, account_id)
    if ok2:
        return True, {"summary": "WPS 签到成功", "data": (d2 or {}).get("data")}
    # 已签到的幂等响应：400 + v7code 400000007
    err = (d2 or {}).get("body") or d2 or {}
    s = json.dumps(err, ensure_ascii=False) if isinstance(err, dict) else str(err)
    if "400000007" in s or (isinstance(err, dict)
                             and (err.get("v7code") == 400000007
                                  or err.get("code") == 400000007)):
        return True, {"already": True, "data": err}
    return False, d2


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
    # ⚠ apk-raccoon 的 /model_catalog **已从目录表移除**（2026-10-04）：
    #   它返回的是 HTML 页面（SPA 路由兜底），不是 JSON。留着会让
    #   fetch_model_catalog 把 HTML 当 JSON 解析直接抛错，小浣熊的模型
    #   获取等于废的。小浣熊真实的 JSON 接口都在 /api/web/... 下。
    # "apk-raccoon": ["https://xiaohuanxiong.com/model_catalog"],   ← 已移除
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
