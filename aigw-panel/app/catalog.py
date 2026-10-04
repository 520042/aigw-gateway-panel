# -*- coding: utf-8 -*-
"""
免费 AI 资源目录
================
数据来源：本地 APK 反解 + 公开导航站核验（核验日期见 VERIFIED_AT）

分四类：
  LOCAL   本地网关（用户已有的 APK / exe）
  RELAY   公益中转站（NewAPI/OneAPI 系，支持每日签到）
  OFFICIAL 官方免费额度平台
  TOOL    开源聚合 / 自建工具
"""

VERIFIED_AT = "2026-10-02"

# ---------------------------------------------------------------- 本地网关
# 从工作区 6 个 APK + gateway exe 反解得到
LOCAL_GATEWAYS = [
    {
        "id": "wb-gateway",
        "name": "WorkBuddy Local Gateway",
        "kind": "exe",
        "file": "workbuddy-gateway-windows-1.29.6.exe",
        "version": "1.29.6",
        "arch": "x86_64",
        "upstream": ["copilot.tencent.com", "www.codebuddy.cn", "api.trae.cn"],
        "auth_mode": "cookie-session",
        "default_port": 8317,
        "default_api_key": "admin",
        "free": "账号自带额度，每日 09:00 自动签到、10:00 成长任务",
        "checkin": True,
        "tasks": True,
        "usage": True,
        "endpoints": [
            "/v1/chat/completions", "/v1/models", "/v1/responses", "/v1/messages",
            "/admin/api/status", "/admin/api/checkins", "/admin/api/growth",
            "/admin/api/growth/lottery", "/admin/api/growth/redeem",
            "/admin/api/growth/bonus", "/admin/api/growth/makeup",
            "/admin/api/growth/travel", "/admin/api/growth/report",
            "/admin/api/usage", "/admin/api/usage/series",
            "/admin/api/models", "/admin/api/models/probe",
            "/admin/api/credentials", "/admin/api/credentials/delete",
            "/admin/api/login/start", "/admin/api/login/poll", "/admin/api/login/import",
            "/admin/api/apikeys", "/admin/api/webhooks", "/admin/api/settings",
            "/admin/api/logs", "/admin/api/setup", "/admin/api/probe",
        ],
        "note": "自带账号池轮询、429 冷却、用量统计、Webhook 通知、模型免费/收费探测",
    },
    {
        "id": "apk-codebuddy",
        "name": "CodeBuddy 国际版网关 (Android)",
        "kind": "apk",
        "file": "base(3).apk",
        "package": "com.joy4fire.workbuddy2api",
        "version": "1.1.0-native-international",
        "upstream": ["copilot.tencent.com", "www.codebuddy.ai"],
        "local_endpoint": "http://127.0.0.1:8788/v1",
        "free": "CodeBuddy 国际版账号额度",
        "checkin": False, "tasks": False, "usage": False,
        "endpoints": ["/v1/chat/completions", "/v1/models", "/v1/messages",
                      "/v1/messages/count_tokens", "/v1/responses"],
        "note": "含 codebuddy-international-models.json（22 个模型，含 gpt-5.6 系列）",
    },
    {
        "id": "apk-trae",
        "name": "Trae / WorkBuddy 国内网关 (Android)",
        "kind": "apk",
        "file": "base.apk",
        "package": "aigw.app",
        "version": "0.1.18",
        "upstream": ["api.trae.cn", "api.trae.com.cn", "www.codebuddy.cn",
                     "www.workbuddy.ai", "trae-api-cn.mchost.guru",
                     "cloudcode-pa.googleapis.com", "accounts.google.com"],
        "oauth_ports": [51120, 51121],
        "free": "Trae 国内站积分 + 签到积分",
        "checkin": True, "tasks": True, "usage": True,
        "endpoints": [
            "/api/v1/chat/completions", "/api/v1/models",
            "/api/v2/ug/checkin_credits/status", "/api/v2/ug/checkin_credits/claim",
            "/api/v1/onboarding/tasks", "/api/v1/onboarding/tasks/complete",
            "/api/v1/points/records", "/api/v1/points/activation",
            "/api/v1/points/redemption-codes/redeem", "/api/v1/team-points/balance",
            "/api/v2/pay/ide_user_ent_usage", "/api/v2/pay/web_user_ent_usage",
            "/api/v2/pay/user_current_entitlement_list",
            "/api/v3/trae/GetUserInfo", "/api/v3/trae/oauth/ExchangeToken",
            "/v2/billing/meter/daily-checkin", "/v2/billing/meter/get-user-resource",
        ],
        "note": "功能最全的 Android 网关：签到 / 成长任务 / 兑换码 / 积分明细 / 用量账单 / OAuth",
    },
    {
        "id": "apk-yuanbao",
        "name": "腾讯元宝网关 (Android)",
        "kind": "apk",
        "file": "base(5).apk",
        "package": "dev.yuanbao2api",
        "version": "1.2.0",
        "upstream": ["yuanbao.tencent.com"],
        "free": "元宝对话额度（消费 APP 内置广告换额度）",
        "checkin": False, "tasks": False, "usage": False,
        "endpoints": ["/v1/chat/completions", "/v1/models",
                      "/api/getuserinfo", "/api/chat/", "/api/user/agent/conversation/create"],
        "note": "把元宝 APP 变成 OpenAI 端点",
    },
    {
        "id": "apk-doubao",
        "name": "豆包网关 (Android)",
        "kind": "apk",
        "file": "base(1).apk",
        "package": "dev.doubao2api",
        "version": "1.0.6",
        "upstream": ["www.doubao.com"],
        "free": "豆包 APP 免费对话额度",
        "checkin": False, "tasks": False, "usage": False,
        "endpoints": ["/v1/chat/completions", "/v1/models", "/v1/images/generations",
                      "/alice/message/get_file_url", "/samantha/pages/upload_image"],
        "note": "支持图片上传 + 文生图，唯一带 /v1/images/generations 的",
    },
    {
        "id": "apk-xiaohuanxiong",
        "name": "小浣熊 AI 网关 (Android)",
        "kind": "apk",
        "file": "base(4).apk",
        "package": "dev.raccoon2api",
        "version": "1.15",
        "upstream": ["xiaohuanxiong.com"],
        "free": "登录送积分，每日登录赠送",
        "checkin": True, "tasks": False, "usage": True,
        "endpoints": ["/v1/chat/completions", "/v1/models",
                      "/api/web/llm/v2", "/api/web/auth/v1", "/api/web/auth/v1/entitlement_info",
                      "/api/web/points/v1/balance", "/api/web/points/v1/bills",
                      "/api/web/office/v3/setting_info", "/api/web/desktop/v1/login/points/grant"],
        "note": "唯一带「登录送积分 /grant」端点的，可当每日签到",
    },
    {
        "id": "apk-go-native",
        "name": "Go 原生网关 (Android)",
        "kind": "apk",
        "file": "base(2).apk",
        "package": "com.joy4fire.wb2apimobile",
        "version": "—",
        "upstream": ["—"],
        "free": "—",
        "checkin": False, "tasks": False, "usage": False,
        "endpoints": ["http://127.0.0.1:<port>"],
        "note": "libgojni.so × 4 架构（arm64/armv7/x86/x86_64），GoMobile 编译，体积 50MB",
    },
]

# ---------------------------------------------------------------- 公益中转站
# NewAPI/OneAPI 系，/api/user/checkin 通用签到
RELAY_SITES = [
    {"name": "AnyRouter",     "url": "https://anyrouter.top",       "signup": "https://anyrouter.top/register?aff=pG9m",   "bonus": "注册赠 $100，签到送 $25",      "models": "Claude 系 10+",       "checkin": True,  "limit": "Linux Do 信任等级≥2"},
    {"name": "AgentRouter",   "url": "https://agentrouter.org",     "signup": "https://agentrouter.org/register?aff=jnrM", "bonus": "注册赠 $100，签到送 $25",      "models": "Claude/GPT/GLM/DeepSeek", "checkin": True,  "limit": "GitHub 注册满 5 年"},
    {"name": "PM-API",        "url": "https://xn--wnup5g6so4wn.de5.net", "signup": "https://xn--wnup5g6so4wn.de5.net/sign-up?aff=9b8a", "bonus": "完全免费无签到",     "models": "240 个",              "checkin": False, "limit": "—"},
    {"name": "快跑 API",       "url": "https://kuaipao.ai",         "signup": "https://kuaipao.ai/register?aff=tOJg",       "bonus": "每日签到",              "models": "100+ 个",             "checkin": True,  "limit": "—"},
    {"name": "维云模型",       "url": "https://vsllm.cc",           "signup": "https://vsllm.cc/i/z2IN",                     "bonus": "每日任务换额度",        "models": "69 个",               "checkin": True,  "limit": "注册无限制"},
    {"name": "New API 福利站", "url": "https://new-api.abrdns.com", "signup": "https://new-api.abrdns.com/register?aff=klPR", "bonus": "每日福利签到",          "models": "69 个",               "checkin": True,  "limit": "Linux Do Lv1 + 注册满 1 天"},
    {"name": "CM-API 公益站",  "url": "https://api.chengmo.cc.cd",  "signup": "https://api.chengmo.cc.cd/sign-up?aff=lz0Q", "bonus": "每日签到",              "models": "46 个",               "checkin": True,  "limit": "注册无限制"},
    {"name": "ZeroCat",       "url": "https://zero.cat",           "signup": "https://zero.cat/sign-up?aff=esyf",         "bonus": "每日签到",              "models": "Claude/DS/xAI/Gemini 40", "checkin": True, "limit": "—"},
    {"name": "Huan API",      "url": "https://ai.huan666.de",      "signup": "https://ai.huan666.de/sign-up?aff=npLD",    "bonus": "每日签到",              "models": "17 个",               "checkin": True,  "limit": "—"},
    {"name": "可萌中转站",     "url": "https://api456.me",         "signup": "https://api456.me/register?aff=76S3",        "bonus": "每日签到",              "models": "Claude/GPT/GLM/DS/Gemini/qwen 29", "checkin": True, "limit": "注册无限制"},
    {"name": "Token 能量站",  "url": "https://factory.pub",        "signup": "https://factory.pub/sign-up?aff=6oZK",       "bonus": "每日签到",              "models": "20 个",               "checkin": True,  "limit": "仅 yaohuo/github 注册"},
    {"name": "Pomelo",        "url": "https://api.67.si",          "signup": "https://api.67.si/sign-up?aff=zpBO",        "bonus": "每日签到",              "models": "xAI/GPT/GLM/DS 17",    "checkin": True,  "limit": "—"},
    {"name": "BER分公益站",    "url": "https://ai.berf1.cn",        "signup": "https://ai.berf1.cn/sign-up?aff=RQvt",       "bonus": "每日签到",              "models": "4 个",                "checkin": True,  "limit": "—"},
    {"name": "hkai",          "url": "https://share.hkai25.top",   "signup": "https://share.hkai25.top/sign-up?aff=0qe8",  "bonus": "每日签到",              "models": "xAI/minimax/GLM/DS 10", "checkin": True, "limit": "—"},
    {"name": "糯喵喵 AI 驿站", "url": "https://ai.yangwj.me",       "signup": "https://ai.yangwj.me/sign-up?aff=gs6u",      "bonus": "每日签到",              "models": "47 个",               "checkin": True,  "limit": "—"},
    {"name": "onomeo",        "url": "https://onomeo.com",         "signup": "https://onomeo.com/?ref=AC-U4P69Q",         "bonus": "签到积分：首日 5000，第 7 天起 10000/日", "models": "35 免费 + 9 付费", "checkin": True, "limit": "免费账号 5h/20 次"},
    {"name": "奶昔 New API",  "url": "https://newapi.naixi.net",    "signup": "https://newapi.naixi.net/login",              "bonus": "开户送 $5，签到送 1-2 刀/日", "models": "模型广场自选",        "checkin": True,  "limit": "需奶昔 SSO 账号"},
]

# ---------------------------------------------------------------- 官方免费额度
OFFICIAL_PLATFORMS = [
    {"name": "智谱 BigModel",   "region": "国内", "url": "https://open.bigmodel.cn",       "free": "GLM-4.7-Flash / GLM-4-Flash / GLM-4.6V-Flash 永久免费；签到送 1 万 token", "note": "新用户赠 2000 万 token"},
    {"name": "阿里云百炼",        "region": "国内", "url": "https://bailian.aliyun.com",    "free": "通义千问全系免费；新人 7000 万 token + 100 张生图", "note": "有效期 180 天，可开「用完即停」"},
    {"name": "百度千帆",         "region": "国内", "url": "https://qianfan.cloud.baidu.com", "free": "ERNIE-Speed/Lite/Tiny 永久免费不限量；新用户 150 万 token", "note": "需实名认证"},
    {"name": "硅基流动 SiliconFlow", "region": "国内", "url": "https://cloud.siliconflow.cn", "free": "部分模型永久免费 + 新户赠 14 元", "note": "国内直连海外模型"},
    {"name": "美团 LongCat",     "region": "国内", "url": "https://longcat.chat/platform", "free": "LongCat-2.5-Preview 资源包 + 活动赠送", "note": "1M 上下文"},
    {"name": "讯飞星辰 MaaS",     "region": "国内", "url": "https://platform.xfyun.cn",     "free": "Qwen3-Coder-Next / Qwen3-1.7B 永久免费不限量", "note": "星火 X2.5 限时免费"},
    {"name": "魔搭 ModelScope",  "region": "国内", "url": "https://modelscope.cn",        "free": "3000+ 模型，魔棒兑换调用", "note": "需绑阿里云并实名"},
    {"name": "Google AI Studio", "region": "海外", "url": "https://aistudio.google.com",   "free": "Gemini 全系免费层，15 RPM / 1500 RPD", "note": "无需信用卡，额度最慷慨"},
    {"name": "NVIDIA NIM",      "region": "海外", "url": "https://build.nvidia.com",      "free": "100+ 模型无限额（速率限制 ~40 RPM）", "note": "无需信用卡"},
    {"name": "Groq",            "region": "海外", "url": "https://groq.com",              "free": "免费 tier 极低延迟", "note": "GPT-OSS / Qwen / Kimi"},
    {"name": "OpenRouter",      "region": "海外", "url": "https://openrouter.ai",         "free": "20+ free 模型，每日请求上限", "note": "搜 free 标签"},
    {"name": "Cloudflare Workers AI", "region": "海外", "url": "https://developers.cloudflare.com", "free": "免费层每天 10,000 Neurons", "note": "边缘推理"},
    {"name": "HuggingFace",     "region": "海外", "url": "https://huggingface.co",       "free": "每月 $0.10 credits", "note": "模型生态最全"},
    {"name": "Cerebras",        "region": "海外", "url": "https://cerebras.ai",           "free": "$5 试用额度（需绑支付方式）", "note": "推理极快"},
]

# ---------------------------------------------------------------- 开源工具
OPEN_SOURCE = [
    {"name": "New-API",      "url": "https://github.com/QuantumNous/new-api",   "desc": "国内最流行的统一网关，支持签到系统 / 额度分配 / 用量看板"},
    {"name": "One API",      "url": "https://github.com/songquanpeng/one-api",  "desc": "38k+ Star，100+ 渠道，New-API 前身"},
    {"name": "All API Hub",  "url": "https://github.com/Jekeer/all-api-hub",    "desc": "浏览器插件，统一管理所有 NewAPI 站点的余额/模型/密钥 + 自动签到"},
    {"name": "newapi-ai-check-in", "url": "https://github.com/chiocai/newapi-ai-check-in", "desc": "NewAPI 多账号自动签到，支持 WAF/Turnstile 绕过与多种登录方式"},
    {"name": "FreeLLMAPI",   "url": "https://github.com/Alvaro-Cintas/freellmapi", "desc": "聚合 14+ 家免费额度为单一 OpenAI 端点，自动故障转移"},
    {"name": "AI Proxy",     "url": "https://github.com/labring/aiproxy",       "desc": "企业级多租户网关，负载均衡 + 自动重试"},
    {"name": "free-newapi 清单", "url": "https://github.com/kirito8/free-newapi", "desc": "本目录的公益站数据来源，持续维护"},
    {"name": "One API Hub",  "url": "https://github.com/DaemonRamble/one-api-hub", "desc": "轻量 Web 服务，SQLite + JWT，聚合 NewAPI 系站点签到与额度"},
]

# ---------------------------------------------------------------- APK 反解明细
APK_ANALYSIS = {
    "base.apk": {
        "package": "aigw.app", "version": "0.1.18", "sdk": "min 24 / target 34",
        "components": ["aigw.app.service.GatewayService", "androidx.core.app.CoreComponentFactory"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE", "FOREGROUND_SERVICE_SPECIAL_USE",
                        "WAKE_LOCK", "REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
                        "ACCESS_WIFI_STATE", "POST_NOTIFICATIONS", "DUMP"],
        "capabilities": ["签到领积分", "新用户任务", "兑换码", "积分流水", "团队积分", "权益套餐", "用量账单"],
    },
    "base(1).apk": {
        "package": "dev.doubao2api", "version": "1.0.6", "sdk": "min 24 / target 34",
        "components": ["dev.doubao2api.ui.MainActivity", "dev.doubao2api.App",
                      "dev.doubao2api.service.GatewayService", "dev.doubao2api.service.BootReceiver"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE_SPECIAL_USE", "RECEIVE_BOOT_COMPLETED",
                        "SYSTEM_ALERT_WINDOW", "REQUEST_IGNORE_BATTERY_OPTIMIZATIONS"],
        "capabilities": ["开源自启", "悬浮窗", "图片生成", "文件上传"],
    },
    "base(2).apk": {
        "package": "com.joy4fire.wb2apimobile", "version": "—", "sdk": "min 21 / target 28",
        "components": ["com.joy4fire.wb2apimobile.MainActivity"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE"],
        "capabilities": ["GoMobile 原生网关（libgojni.so × 4 架构）"],
    },
    "base(3).apk": {
        "package": "com.joy4fire.workbuddy2api", "version": "1.1.0-native-international", "sdk": "min 24 / target 34",
        "components": ["com.joy4fire.workbuddy2api.MainActivity", "com.joy4fire.workbuddy2api.ApiHostService",
                      "com.joy4fire.workbuddy2api.OAuthWebActivity"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE_SPECIAL_USE", "WAKE_LOCK"],
        "capabilities": ["OAuth 网页授权", "Anthropic Messages 兼容", "Responses API", "token 计数"],
    },
    "base(4).apk": {
        "package": "dev.raccoon2api", "version": "1.15", "sdk": "min 24 / target 34",
        "components": ["dev.raccoon2api.ui.MainActivity", "dev.raccoon2api.App",
                      "dev.raccoon2api.service.GatewayService", "dev.raccoon2api.service.BootReceiver",
                      "dev.raccoon2api.service.FloatingWindowService"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE_SPECIAL_USE", "RECEIVE_BOOT_COMPLETED",
                        "SYSTEM_ALERT_WINDOW", "WAKE_LOCK"],
        "capabilities": ["登录送积分", "积分余额", "消费账单", "权益查询", "悬浮窗"],
    },
    "base(5).apk": {
        "package": "dev.yuanbao2api", "version": "1.2.0", "sdk": "min 24 / target 34",
        "components": ["dev.yuanbao2api.ui.MainActivity", "dev.yuanbao2api.App",
                      "dev.yuanbao2api.service.GatewayService", "dev.yuanbao2api.service.BootReceiver"],
        "permissions": ["INTERNET", "FOREGROUND_SERVICE_SPECIAL_USE", "RECEIVE_BOOT_COMPLETED",
                        "SYSTEM_ALERT_WINDOW", "WAKE_LOCK"],
        "capabilities": ["开源自启", "悬浮窗", "用户信息查询"],
    },
}


def all_sites():
    """把中转站 + 官方平台合并成统一的可管理站点列表"""
    out = []
    for s in RELAY_SITES:
        out.append({
            "name": s["name"], "url": s["url"], "signup": s["signup"],
            "bonus": s["bonus"], "models": s["models"], "limit": s["limit"],
            "checkin": s["checkin"], "category": "relay", "region": "—",
            "endpoints": ["/api/user/checkin", "/api/user/checkin/status"],
        })
    for s in OFFICIAL_PLATFORMS:
        out.append({
            "name": s["name"], "url": s["url"], "signup": s["url"],
            "bonus": s["free"], "models": "—", "limit": "—",
            "checkin": s["name"].startswith("智谱"), "category": "official",
            "region": s["region"], "endpoints": [],
        })
    return out
