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
import sys
import threading
import time
import urllib.parse
import uuid

# ------------------------------------------------------------------ 平台规格

# platform 与 upstreams.GATEWAYS 的 id 对齐，另加若干「纯上游平台」
PLATFORMS = {
    "wb-gateway": {
        "name": "WorkBuddy 账号 · 国内站（扫码 · 面板原生直连）",
        "method": "qrcode", "edition": "cn",
        "gateway": True,
        "upstream": "copilot.tencent.com",
        "hint": "扫码登录 WorkBuddy / CodeBuddy 国内账号。凭据由面板直接持有并"
                "直连 copilot.tencent.com 拉倍率/签到 —— 不需要启动网关 EXE；"
                "「本地网关 EXE」是另一个东西（外部可选组件）",
        # 2026-10-04 实测：/v3/config 免登录就 200，带 token 后返回倍率表
        "verify": {"url": "https://copilot.tencent.com/console/account",
                   "type": "bearer",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["uid", "nick", "userId"]},
    },
    "wb-gateway-intl": {
        "name": "WorkBuddy 账号 · 国际站（扫码 · 面板原生直连）",
        "method": "qrcode", "edition": "intl",
        "gateway": True,
        "upstream": "copilot.tencent.com",
        "hint": "国际站账号，扫码走 copilot.tencent.com 国际授权；同样由面板"
                "原生直连，不需要网关 EXE",
        "verify": {"url": "https://copilot.tencent.com/console/account",
                   "type": "bearer",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["uid", "nick", "userId"]},
    },
        "web-lobster": {
        "name": "网易 Lobster AI（有道龙虾 · 0.05 倍率）",
        "method": "file",
        "hosts": ["lobsterai.youdao.com"],
        "login_url": "https://lobsterai.youdao.com/#/index?keyfrom=invitation",
        "gateway": False,
        "upstream": "lobsterai.youdao.com",
        "placeholder": "粘贴 accessToken（lobsterai2api 的 auths/lobsterai-*.json 里那个）",
        # ★ 协议逆向自社区项目 lobsterai2api：对话 /api/proxy/v1/chat/completions
        #   （OpenAI 兼容透传，Bearer accessToken）；模型 /api/models/available；
        #   每日签到 +100 credits（由 lobsterai2api 自带调度器做，面板暂不代签）。
        "hint": "★ 客户端 0.05 倍率。接入两步：① 设置→「Lobster 上游地址」填 "
                "lobsterai2api 的 LB2A_UPSTREAM_BASE（其登录页/抓包可得）；"
                "② 运行其 login.exe 完成手机/微信登录，把 auths/lobsterai-*.json "
                "里的 accessToken 粘到这里。粘好后：模型用「查看模型」动态拉，"
                "对话 model 写 模型id@web-lobster 即走面板原生中继。",
        # 验活：上游地址是用户在设置里填的（无静态域名），用 @lobster 哨兵——
        # verify_cookie 会换成 设置→Lobster 上游地址；未配置时明确说「未校验」。
        "verify": {"url": "https://@lobster/api/user/profile-summary",
                   "type": "bearer",
                   "ok_keys": ["data", "uid", "email", "credits"]},
    },
    "web-glm": {
        "name": "智谱清言网页版（glm2api 逆向）",
        "method": "file",
        "hosts": ["chatglm.cn"],
        "login_url": "https://chatglm.cn",
        "gateway": False,
        "upstream": "chatglm.cn",
        "placeholder": "粘贴 chatglm_refresh_token（登录 chatglm.cn 后 F12 → Application → Cookies）",
        # ★ build38 对齐 glm2api(yxc0915) 一手源码：
        #   上游 https://chatglm.cn/chatglm/backend-api/assistant/stream（SSE），
        #   已实现 X-Sign/X-Timestamp/X-Nonce 签名 + refresh_token→access_token
        #   自动换取（/user-api/user/refresh），不再是 build33 的「推断/必 403」。
        "hint": "登录 chatglm.cn，F12 拿 chatglm_refresh_token（Cookies）粘这里；"
                "面板会自动换 access_token 并补签名头。对话 model 写 模型id@web-glm"
                "（glm-4-flash / glm-4 / glm-4-plus / glm-4-air / glm-4-all / glm-zero-preview）。",
        "verify": {"type": "bearer", "ok_keys": ["data", "choices", "id"]},
    },
    "web-trae": {
        "name": "Trae 直连（Trae2api-cn 逆向）",
        "method": "file",
        "hosts": ["trae-api-cn.mchost.guru", "trae.cn", "trae.com.cn"],
        "login_url": "https://www.trae.com.cn",
        "gateway": False,
        "upstream": "trae-api-cn.mchost.guru",
        "placeholder": "粘贴 Trae x-ide-token（桌面端登录态 leveldb 或 OAuth refresh 换取）",
        # ★ build38 对齐 dsh-trae-connect(lament-z) 一手源码：
        #   真实 CN base = https://trae-api-cn.mchost.guru（不是旧档案的
        #   a0ai-api-sg.byteintlapi.com）；CN appId=6eefa01c-…（932204 会被拒）。
        #   已实现 Trae 私有 chat 请求体（user_input/intent_name/variables/
        #   chat_history/function='chat'）与 event:response 增量 delta 解析。
        "hint": "★ 直连 Trae 国内版，无需手机开 App。取 x-ide-token：桌面端登录一次后，"
                "从 Trae 的 leveldb/Electron LocalStorage 取 ideToken（或用 OAuth "
                "refresh 换）。对话 model 写 模型id@web-trae"
                "（gpt-4o / claude-3-7-sonnet / deepseek-v3 / deepseek-r1 等）。",
        "verify": {"type": "bearer", "ok_keys": ["data", "choices", "output"]},
    },
    "web-deepseek": {
        "name": "DeepSeek 网页版（deepseek-free-api 逆向）",
        "method": "file",
        "hosts": ["chat.deepseek.com"],
        "login_url": "https://chat.deepseek.com",
        "gateway": False,
        "upstream": "chat.deepseek.com",
        "placeholder": "粘贴 userToken（登录 chat.deepseek.com 后 F12 → Application → LocalStorage）",
        # ★ build38 对齐 deepseek-free-api(qiaojinxia) 一手源码：
        #   已实现 chat_session 创建 + create_pow_challenge 挑战 + 随包 Node/WASM
        #   解 POW（x-ds-pow-response 头）+ p/v/o 分片响应解析。
        #   ⚠ POW 求解依赖系统 node（app/webpow 已随包），node 缺失会明确报错。
        "hint": "★ 免 API Key，用免费网页额度。登录 chat.deepseek.com 后 F12 拿 "
                "LocalStorage 的 userToken 粘这里。对话 model 写 deepseek-chat@web-deepseek "
                "或 deepseek-reasoner@web-deepseek（深度思考）。"
                "首次对话需系统装有 node（用于解 POW 校验）。",
        "verify": {"type": "bearer", "ok_keys": ["data", "choices", "id"]},
    },
    "apk-trae": {
        "name": "Trae 国内版（aigw.app 网关，同一源）",
        "method": "qrcode", "edition": "cn",
        "gateway": False,
        "upstream": "api.trae.cn",
        "hint": "Trae 走同一套 copilot 授权，登录后可直连 api.trae.cn",
        # ⚠ 2026-10-04 复测后修正：verify2.txt 记的
        #     /cloudide/api/v3/trae/GetUserInfo 现在**两个域都不通**
        #     （api.trae.cn → 404；www.trae.com.cn → 返回 HTML 页面）。
        #     Trae 的 user_info / quota / 手机登录那一批已全部退化成网页路由，
        #     活着的只剩挂在 /trae 前缀下的 5 条（见 app/gwextra.py::ACTIONS）。
        #     Trae 与 CodeBuddy 共用授权，所以验活走 copilot 统一后端。
        "verify": {"url": "https://copilot.tencent.com/console/account",
                   "type": "bearer",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["uid", "nick", "userId"]},
    },
    "apk-codebuddy": {
        "name": "CodeBuddy 国际版",
        "method": "qrcode", "edition": "intl",
        "gateway": False,
        "upstream": "www.codebuddy.ai",
        "hint": "CodeBuddy 国际站账号（OAuthWebActivity 对应的网页授权）",
        # 2026-10-04 实测：与国内站同一套后端，换 /console/account 验活
        # （不能用 /v3/config —— 那个免登录就 200，用它验活等于没验）
        "verify": {"url": "https://copilot.tencent.com/console/account",
                   "type": "bearer",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["uid", "nick", "userId"]},
        # ✓ 来自 data/verify2.txt（对 www.codebuddy.ai 探测的结果）
    },
    "apk-doubao": {
        "name": "豆包 dev.doubao2api",
        "method": "cookie",
        "hosts": ["doubao.com"],
        "login_url": "https://www.doubao.com/",
        "gateway": False,
        "upstream": "www.doubao.com",
        "hint": "在打开的浏览器里登录豆包，面板自动取 Cookie（也支持手动粘贴）",
        # ★★ 2026-10-04 开浏览器抓包纠错（前两版都判错了）：
        #   豆包网页版的真实接口前缀是 **/alice/** 和 **/samantha/**（带 aid=497858），
        #   不是 /api/v1/* —— 我一直打错前缀，所以拿到的全是 openresty 网关层
        #   「401 + 空 body」（真/假 cookie 响应一样），才误判成"无法在线验活"。
        #   实测真验活口：
        #     GET /alice/user/config/pull?...aid=497858...
        #     未登录 → 200 + {"code":710012001,"msg":"登录已过期，请重新登录"}
        #   这是**可区分**的应用层鉴权结论，verify_cookie 的「业务码非 0」判定
        #   天然就能识别，不需要 errno_field 额外配置。
        "verify": {"url": "https://www.doubao.com/alice/user/config/pull"
                          "?version_code=20800&language=zh&device_platform=web"
                          "&doubao_device_platform=web&aid=497858&real_aid=497858",
                   "type": "cookie"},
        # ✓ 参考 data/verify2.txt；chat / image_gen 无凭据也返 200，不能验活。
    },
    "apk-yuanbao": {
        "name": "元宝 dev.yuanbao2api",
        "method": "cookie",
        "hosts": ["yuanbao.tencent.com"],
        "login_url": "https://yuanbao.tencent.com/",
        "gateway": False,
        "upstream": "yuanbao.tencent.com",
        # ★★ 2026-10-04 开浏览器实测（推翻旧结论）：
        #   元宝的登录态**不在 Cookie 里**。yuanbao.tencent.com 域下只有
        #   `_TDID_CK`、`561553b2…` 这类**设备 ID**；真正的会话在 localStorage 的
        #   `LOCAL_AUTH_INFO_KEY_yuanbao.tencent.com`（页面还有 `__is_guest_mode__`）。
        #   → 以前只抓 Cookie，拿到的是设备 ID，验活当然过不了。
        #   现已支持 storage_keys：登录后会连同 localStorage 一起取回。
        "storage_keys": ["LOCAL_AUTH_INFO_KEY_yuanbao.tencent.com"],
        "hint": "⚠ 元宝的会话在 localStorage（不是 Cookie）：请在打开的浏览器里"
                "登录元宝后再点「我已登录」。若只在 Cookie 里看到 _TDID_CK / "
                "561553b2 这类设备 ID，说明还没登录成功。",
        "verify": {"url": "https://yuanbao.tencent.com/api/getuserinfo",
                   "type": "cookie", "ok_keys": ["user", "data", "userId"]},
        # ✓ 来自 data/verify2.txt
        # ⚠ 三个都返 401 → 都真要凭据（对比 /api/models 也是 401，端点确实存在）
    },
    "apk-raccoon": {
        "name": "小浣熊 dev.raccoon2api",
        "method": "cookie",
        "hosts": ["xiaohuanxiong.com"],
        "login_url": "https://xiaohuanxiong.com/",
        "gateway": False,
        "upstream": "xiaohuanxiong.com",
        "hint": "登录小浣熊；登录后可调「登录送积分」接口领积分",
        # 2026-10-04 实测：3 个端点无凭据都返 401 code=200003（存在且要凭据）
        "verify": {"url": "https://xiaohuanxiong.com/api/web/points/v1/balance",
                   "type": "cookie", "ok_keys": ["balance", "points", "data"]},
        # ✓ 全部来自 data/verify2.txt（已验证存在，括号内是无凭据响应）
        "bonus": {"url": "https://xiaohuanxiong.com/api/web/desktop/v1/login/points/grant",
                  "method": "POST"},
    },
    "apk-go": {
        "name": "Go 原生网关 com.joy4fire.wb2apimobile",
        "method": "file",
        "gateway": False,
        "upstream": "copilot.tencent.com",
        "hint": "Go 版网关未提供网页授权入口，支持导入凭据文件 / 直接填 API Key",
        # 2026-10-04 实测：Go 版和 CodeBuddy 共用同一套 token（模型接口 200）
        "verify": {"url": "https://copilot.tencent.com/console/account",
                   "type": "bearer",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["uid", "nick", "userId"]},
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
                   "type": "cookie",
                   "ok_keys": ["data", "models", "code"]},
        # ✓ 来自 data/verify2.txt
        # ⚠ 无凭据也返 200（code=100002「缺少 token」）→ **models 不能当验活端点**，
        #   它只能证明「接口通」。真正要判断登录态得看 models 里的 data 有没有内容。
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
        # ⚠ 2026-10-04 实测：原先用 /v1internal:listModels 做验活，但它 GET/POST
        #   都返 **404**，verify_cookie 对 404 是「未判定为无效」→ 恒为 True，
        #   等于没验。改成实测返 401（真实存在、需鉴权）的 :loadCodeAssist。
        "verify": {"url": "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist",
                   "method": "POST", "type": "bearer"},
    },
    "apk-codebuddy-cn": {
        "name": "CodeBuddy 国内站（www.codebuddy.cn）",
        "method": "qrcode", "edition": "cn",
        "gateway": False,
        "upstream": "www.codebuddy.cn",
        "hint": "CodeBuddy 国内站，与国际站 www.codebuddy.ai 是两套"
                "（实测国内站的模型目录与积分接口都通）",
        # ✓ 修正 2026-10-04：之前误判成「域错了/404」，其实是**方法错了**。
        #   GET  /v2/billing/meter/get-user-resource → 404（路由不匹配 GET）
        #   POST 同路径                        → 401（存在，要凭据）
        #   www.codebuddy.cn 和 copilot.tencent.com 两域行为一致，都能用。
        #   data/verify2.txt 里记的是 GET 才 401，那是当时无凭据的 400/401 混了。
        "verify": {"url": "https://www.codebuddy.cn/v2/billing/meter/get-user-resource",
                   "type": "bearer", "method": "POST",
                   "ua": "CLI/2.143.1 CodeBuddy/2.143.1",
                   "ok_keys": ["Response", "Data", "Accounts"]},
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
        "hint": "⚠ qwenwork.cn 是 JWT 鉴权（实测 2026-10-05）：网关中间件只认"
                " cookie 名 token=eyJ… 或 Authorization Bearer 头，其它 Cookie"
                " 一律「JWT is missing」。自动捕获会双发 Bearer；若仍校验失败，"
                "请在登录后浏览器 DevTools→Application→Cookies 里找 token="
                "开头为 eyJ 的那条手动粘贴。每日登录奖 100 积分",
        # 2026-10-04 实测：/api/user/info、/api/v1/user/info、/api/user/profile、
        #   /api/user/points 无凭据全返 401 → 都存在，取第一个当验活端点
        "verify": {"url": "https://qwenwork.cn/api/user/info",
                   "type": "cookie",
                   "ok_keys": ["user", "data", "nick", "avatar"]},
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
        # ✓ 来自 2026-10-04 登录后 CDP 抓包（data/kuku_wps_capture.jsonl）
        #   ⚠ 库库的登录态标志**不是 HTTP 401**，而是响应体里
        #     errno = -6 + show_msg = "未登录"/"need login"（状态码仍是 200）。
        #     所以 ok_keys 必须挑「只有登录了才会填的字段」：
        #     未登录时 profile 返回 nickname/account/avatar_url 全是空串。
        "verify": {"url": "https://kuku.baidu.com/api/genflowpro/settings/profile",
                   "type": "cookie",
                   "ok_keys": ["nickname", "account", "avatar_url"],
                   "errno_field": "errno",
                   "unlogin_errno": -6,
                   "unlogin_msg": "未登录 / need login",
                   "note": "200 + errno:-6 = 未登录；errno:0 = 已登录"},
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
        # ✓ 全部来自 2026-10-04 登录后 CDP 抓包（data/wps_ai_api.json）
        #   ⚠ WPS AI（灵犀）**不在 www.wps.cn**，独立域是 lingxi.kdocs.cn。
        #     www.wps.cn 首页全是云文档接口（/api/v5/files 那些），
        #     AI 积分/模型全在 lingxi.kdocs.cn 下。
        #   实测 200 的 7 条见 app/gwextra.py::ACTIONS["apk-wps"]
        "verify": {"url": "https://drive.kdocs.cn/api/v3/userinfo",
                   "type": "cookie",
                   "ok_keys": ["id", "name", "avatar", "status"],
                   "note": "AI 积分在 lingxi.kdocs.cn/api/public/v1/credits/balance"},
    },
    "apk-nano": {
        "name": "纳米 AI（360）",
        "method": "cookie",
        "hosts": ["n.cn", "360.cn"],
        "login_url": "https://www.n.cn/",
        "gateway": False,
        "upstream": "www.n.cn",
        "hint": "360 旗下 AI 搜索/办公。端点需登录后探测",
        # 2026-10-04 实测：/api/user/info 返 200 code=110001（=未登录的业务码，不是 404）
        "verify": {"url": "https://www.n.cn/api/user/info",
                   "type": "cookie",
                   "ok_keys": ["user", "data", "nickname", "avatar"]},
    },
    "anon-zen": {
        "name": "Our Free Model（OpenCode Zen 匿名车道）",
        "method": "anon",
        "hosts": ["opencode.ai"],
        "login_url": "https://opencode.ai",
        "gateway": False,
        "upstream": "opencode.ai",
        "placeholder": "无需填写（公共凭据自动落池）",
        # ★ 2026-10-05 整合 dsh-our-free-model（1376★）逆向的匿名车道：
        #   上游 https://opencode.ai/zen/v1/*，Authorization: Bearer public
        #   （公共池凭据，无个人密钥）；UA 指纹 opencode/1.18.31 + x-opencode-* 会话头；
        #   免费额度按会话计（429=FreeUsageLimitError），地区受限回 403。
        #   模型清单动态：GET /zen/v1/models（DeepSeek V4.1 Flash、Kimi K3 等）。
        "hint": "★ 免登录、免 Key、免注册：点接入即把公共凭据写入账号池。"
                "模型含 DeepSeek V4.1 Flash、Kimi K3 等前沿款（动态清单）。"
                "免费额度按会话计，短时间打满会 429（换会话或稍后再用）；"
                "个别地区 403。上游仅 OpenCode Zen 一家，无中转无号池。",
        # 验活：2026-10-05 实测 GET /zen/v1/models 免登录可读（Bearer public +
        # Zen UA → 200 模型清单），能证明车道活着；个人凭据本就是公共池，无隐私。
        "verify": {"url": "https://opencode.ai/zen/v1/models",
                   "type": "bearer", "ua": "opencode/1.18.31",
                   "ok_keys": ["data", "id", "model"]},
    },
    "web-qoder": {
        "name": "Qoder（直连模型端点 · qwen3.8-flash 免费）",
        "method": "file",
        "hosts": ["qoder.com", "api2-v2.qoder.sh"],
        "login_url": "https://www.qoder.com/",
        "gateway": False,
        "upstream": "api2-v2.qoder.sh",
        "placeholder": "粘贴 Qoder 的 accessToken（桌面端登录态）",
        # ★ 2026-10-05 升级：社区 Qoder2Api 逆向出直连模型端点
        #   https://api2-v2.qoder.sh/model/v1/chat/completions（纯 OpenAI 格式，
        #   绕开 COSY 签名的 agent 端点），qwen3.8-flash 免费。
        "hint": "★ 直连模型端点（社区 Qoder2Api 逆向）：qwen3.8-flash 免费，"
                "qwen3.7-plus/kimi-k2.7-code/deepseek-v4-pro/minimax-m2.5 走 trial "
                "credits。获取 token：运行 Qoder2Api 的设备登录（python main.py "
                "--login），token 存于 ~/.qoder2api-auth.json，把 accessToken 粘到这里。"
                "粘好后对话 model 写 模型id@web-qoder 即走面板原生中继。",
        # 验活：2026-10-04 审计实测 www.qoder.com/api/v1/user/info 无凭据 401
        # （带响应体，能区分真假），贴对 token 后 200。
        "verify": {"url": "https://www.qoder.com/api/v1/user/info",
                   "type": "bearer",
                   "ok_keys": ["data", "id", "model", "choices"]},
    },
    "apk-qoder": {
        "name": "Qoder（阿里桌面端，原通义灵码国际版）",
        "method": "file",
        "hosts": ["qoder.com", "lingma.aliyun.com"],
        "login_url": "https://www.qoder.com/",
        "gateway": False,
        "upstream": "www.qoder.com",
        "placeholder": "粘贴 Qoder 的 token/Cookie",
        # ★ 2026-10-05 改口：Qoder 是**桌面端应用**，凭据在桌面客户端本地，
        #   没有"用浏览器 Cookie 接入"一回事（旧流程误导，已删）。
        #   CLIProxyAPI v8.0.13 实测无 -qoder-login 旗标（此前 hint 写支持是错的）。
        #   现只保留手动粘贴凭据；等 CLIProxyAPI 新版支持 Qoder OAuth 后
        #   再接「本地反代」路径。
        "hint": "★ 桌面端应用：凭据在 Qoder 桌面客户端本地，浏览器 Cookie "
                "接入无意义（已移除该流程）。每日 10:00 领 100 Credits 要在"
                "桌面端里领（官方写明仅桌面端主动领取，未公布 API）。"
                "CLIProxyAPI v8.0.13 尚无 -qoder-login。若你能从客户端取到"
                "凭据，可手动粘贴这里（验活走 /api/v1/user/info）",
        # 2026-10-04 实测：/api/v1/user/info 返 401（存在）；/api/user/info 返 404
        "verify": {"url": "https://www.qoder.com/api/v1/user/info",
                   "type": "cookie",
                   "ok_keys": ["user", "data", "email", "credits"]},
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
        # 2026-10-04 实测：/api/user/info 等 5 个路径无凭据全返 200（比基线有区别），
        # 说明 metaso 的读接口对未登录也返回结构化数据 → 不能只靠 200 判登录成功，
        # 必须配合 ok_keys 检查有没有用户字段。
        "verify": {"url": "https://metaso.cn/api/user/info",
                   "type": "cookie",
                   "ok_keys": ["user", "data", "id", "name"]},
    },
}

# ============ 2026-10-05 全项目复查补：官方 API 平台（api-*）的配 Key 入口 ============
# 之前这 15 家只在「接入源」里挂名，登录页没有入口，用户没地方粘 API Key，
# platform_models 也永远拉不到实时清单。现按 coze 的 file/api_key 模式统一注册：
#   粘贴 Key → 在线验活（GET {base}/models + Bearer：200=有效 / 401=无效）→ 落账号池
# 注意：verify 用 api_key 类型（verify_cookie 会加 Authorization: Bearer）。
_API_KEY_PLATFORMS = {
    # pid: (名称, 控制台地址, 验活 URL, placeholder, hint)
    "api-deepseek": ("DeepSeek 官方 API", "https://platform.deepseek.com/",
                     "https://api.deepseek.com/models", "sk-...",
                     "在 DeepSeek 开放平台「API keys」创建后粘贴"),
    "api-moonshot": ("Kimi 月之暗面 API", "https://platform.moonshot.cn/",
                     "https://api.moonshot.cn/v1/models", "sk-...",
                     "在 Moonshot 开放平台「API Key 管理」创建后粘贴"),
    "api-zhipu": ("智谱 BigModel API", "https://open.bigmodel.cn/usercenter/apikeys",
                  "https://open.bigmodel.cn/api/paas/v4/models", "id.xxx",
                  "在智谱「API keys」创建后粘贴；GLM-Flash 系免费"),
    "api-dashscope": ("阿里云百炼 API", "https://bailian.console.aliyun.com/",
                      "https://dashscope.aliyuncs.com/compatible-mode/v1/models", "sk-...",
                      "在百炼控制台「API-KEY 管理」创建后粘贴；新用户赠大量 token"),
    "api-bailian": ("阿里百炼 API", "https://bailian.console.aliyun.com/",
                    "https://dashscope.aliyuncs.com/compatible-mode/v1/models", "sk-...",
                    "与阿里云百炼同一套 Key（DashScope 兼容模式）"),
    "api-volcengine": ("火山方舟 API", "https://console.volcengine.com/ark",
                       "https://ark.cn-beijing.volces.com/api/v3/models", "key...",
                       "在火山方舟「API Key 管理」创建后粘贴"),
    "api-minimax": ("MiniMax API", "https://platform.minimaxi.com/",
                    "https://api.minimax.chat/v1/models", "eyJ...",
                    "在 MiniMax 开放平台创建后粘贴"),
    "api-stepfun": ("阶跃星辰 API", "https://platform.stepfun.com/",
                    "https://api.stepfun.com/v1/models", "sk-...",
                    "在阶跃「API 密钥」创建后粘贴"),
    "api-baichuan": ("百川智能 API", "https://platform.baichuan-ai.com/",
                     "https://api.baichuan-ai.com/v1/models", "sk-...",
                     "在百川开放平台「API Keys」创建后粘贴"),
    "api-siliconflow": ("硅基流动 API", "https://cloud.siliconflow.cn/",
                        "https://api.siliconflow.cn/v1/models", "sk-...",
                        "在硅基流动「API 密钥」创建后粘贴；部分模型永久免费"),
    "api-groq": ("Groq API", "https://console.groq.com/keys",
                 "https://api.groq.com/openai/v1/models", "gsk_...",
                 "在 Groq Console 创建后粘贴（免费档极低延迟）"),
    "api-openrouter": ("OpenRouter API", "https://openrouter.ai/keys",
                       "https://openrouter.ai/api/v1/models", "sk-or-...",
                       "在 OpenRouter「Keys」创建后粘贴；有 free 标签免费模型"),
    "api-modelscope": ("魔搭 ModelScope API", "https://modelscope.cn/my/myaccesstoken",
                       "https://api-inference.modelscope.cn/v1/models", "ms-...",
                       "在魔搭「访问令牌」创建后粘贴；3000+ 模型"),
    "api-xfyun": ("讯飞星辰 MaaS API", "https://console.xfyun.cn/",
                  "https://spark-api-open.xf-yun.com/v1/models", "sk-...",
                  "用星辰 MaaS 的 http key（Bearer 直用）；"
                  "注意 spark-api.xf-yun.com 老星火是 HMAC 签名，不适用"),
    "api-qianfan": ("百度千帆 API", "https://console.bce.baidu.com/iam/",
                    "https://qianfan.baidubce.com/v2/models", "bce-...",
                    "在百度智能云创建千帆 API Key 后粘贴；ERNIE-Speed/Lite 永久免费"),
}
for _pid, (_nm, _lu, _vu, _ph, _hint) in _API_KEY_PLATFORMS.items():
    PLATFORMS[_pid] = {
        "name": _nm, "method": "file", "type": "api_key",
        "gateway": False, "hosts": [], "login_url": _lu,
        "upstream": _vu.split("/")[2] if _vu.startswith("http") else "",
        "placeholder": _ph, "hint": _hint,
        "verify": {"url": _vu, "type": "api_key"},
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
            elif sess.method == "anon":
                self._start_anon(sess, spec)
            else:
                self._start_file(sess, spec)
        except Exception as e:
            sess.finish("error", "启动登录失败", "%s: %s" % (type(e).__name__, e))

    # ---------------------------------------------------------- 匿名车道
    def _start_anon(self, sess, spec):
        """匿名免登录渠道（OpenCode Zen）：无需任何凭据，公共池凭据
        直接落账号池（secret="public"，对话框写明这一点）。"""
        sess.finish("success",
                    "匿名渠道无需登录：公共凭据已直接写入账号池",
                    result={"type": "token", "secret": "public",
                            "account": "匿名公共池",
                            "source": "匿名渠道（免登录）"})
        self._save(sess)

    # ---------------------------------------------------------- 二维码
    def _start_qrcode(self, sess, spec):
        c = self._client_factory()
        edition = sess.opts.get("edition") or spec.get("edition") or "cn"
        # ★ 集成优先：网关 EXE 不可达时，copilot 系平台回退面板原生扫码
        #   （tlogin 链路：state → copilot 授权页 → 页面上下文换 token），
        #   不再要求用户先启动 EXE（用户明确要求「只启动一个软件」）。
        #   判定用 start_login 的真实异常，而不是 ping 预判——避免误伤测试桩
        #   和「EXE 活着但 ping 抖动」的场景。
        try:
            r = c.start_login(edition)
        except Exception as e:
            if spec.get("gateway"):
                with sess.lock:
                    sess.message = "网关 EXE 未运行（%s），改走面板原生直连扫码" % e
                return self._start_native_qrcode(sess, spec)
            sess.finish("error", "启动登录失败", "%s: %s" % (type(e).__name__, e))
            return
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

    # ---------------------------------------------- 原生扫码（免网关 EXE）
    def _start_native_qrcode(self, sess, spec):
        """copilot 系平台的「面板原生直连」扫码：
        本地生成 state → 面板把授权 URL 渲染成二维码（前端 vendor 库出码）
        → 用户用 CodeBuddy/WorkBuddy App 扫码授权（或在手机/电脑浏览器里
        打开授权页完成登录）→ 面板轮询 /v2/plugin/auth/token?state= 拿 token
        （tlogin 实测：state 本地生成、服务端不校验、轮询端点可用）。
        全程不启动网关 EXE、不强制跳转本机网页。
        凭据（accessToken）直接进面板账号池。"""
        try:
            from app.tencent import Tencent
            state = Tencent.new_state()
            url = Tencent.login_url(state, "CLI")
        except Exception as e:
            sess.finish("error", "生成登录会话失败",
                        "%s: %s" % (type(e).__name__, e))
            return
        with sess.lock:
            sess.opts["native_state"] = state
            sess.auth_url = url
            sess.qr = ""          # 前端用 auth_url 现场出码
            sess.expires_at = time.time() + 300
        sess.finish("waiting",
                    "请用 CodeBuddy / WorkBuddy App 扫码授权；"
                    "或在手机/电脑浏览器打开授权页完成登录"
                    "（面板原生直连，未启动网关 EXE）", result=None)
        self._poll_native(sess, spec)

    def _poll_native(self, sess, spec):
        """原生扫码的收尾线程：纯 HTTP 轮询 token 端点 → 落账号池。"""
        from app import tlogin
        state = sess.opts.get("native_state")
        if not state:
            sess.finish("error", "轮询登录状态失败", "no_state")
            return
        try:
            d = tlogin.poll_token(state=state, timeout=300)
        except Exception as e:
            if getattr(sess, "_stop", False):
                return
            sess.finish("error", "原生登录交换失败",
                        "%s: %s" % (type(e).__name__, e))
            return
        if getattr(sess, "_stop", False):
            return
        if d.get("accessToken"):
            sess.finish("success", "登录成功（面板原生直连，token 已入账号池）",
                        result={"type": "token",
                                "secret": d["accessToken"],
                                "account": d.get("userId") or "原生登录",
                                "uid": d.get("userId") or "",
                                "edition": spec.get("edition", ""),
                                "source": "原生登录"})
            self._save(sess)
        else:
            sess.finish("expired", "等待超时（5 分钟内未完成登录）", "expired")

    def _poll_qrcode(self, sess):
        c = self._client_factory()
        sid = sess.opts.get("gw_session")
        if not sid:
            # ★ 网关会话 id 还没拿到（后台线程正在调网关，约 1-2 秒）。
            #   以前这里直接报「缺少登录会话 id」并把会话打进 error 终态——
            #   前端 2 秒一轮的轮询比后台线程快时必然误伤。改为静默等下一轮；
            #   真正失败由后台线程自己 finish("error")。
            if getattr(sess, "_stop", False):
                return
            with sess.lock:
                if not sess.message:
                    sess.message = "正在获取登录二维码…"
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
        # ★ 登录态不在 Cookie 的站点（实测元宝）：补读 localStorage，
        #   以 `__ls_<key>=<urlencoded json>` 形式并进凭据串，落库后可原样还原。
        skeys = spec.get("storage_keys") or []
        if skeys and b:
            try:
                import urllib.parse as _up
                parts = []
                for k in skeys:
                    v = b.storage_item(k, match=(hosts[0] if hosts else None))
                    if v:
                        parts.append("__ls_%s=%s" % (k, _up.quote(str(v), safe="")))
                if parts:
                    header = (header + "; " + "; ".join(parts)).strip("; ")
            except Exception:
                pass
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
        # ★ 以前这里是 `except Exception: pass` —— 落库失败被**静默吞掉**，
        #   表现就是「登录成功但账号池里什么都没有」，而且看不出原因。
        #   现在把失败原因写回 result，接口/测试都能查得到。
        if not self.accounts:
            if isinstance(sess.result, dict):
                sess.result["save_error"] = "未接入账号池（LoginManager.accounts 为空）"
            return
        if not sess.result:
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
                # ★ 保存 refreshToken：账号池保活线程到期前自动刷新（社区标配）
                "refresh_token": sess.result.get("refreshToken", ""),
                # copilot 风控头（X-User-Id）与 token 必须同账号，登录响应里就有
                "uid": sess.result.get("uid", ""),
            })
        except Exception as e:
            try:
                sess.result["save_error"] = "%s: %s" % (type(e).__name__, e)
            except Exception:
                pass


# ------------------------------------------------------------------ 校验

def verify_cookie(spec, cookie):
    """
    用凭据调一次用户信息接口，确认登录态有效。
    返回 (bool, 说明)。无法联网或平台未提供校验接口时返回 (True, "未校验")，
    避免因网络问题把可用的 Cookie 判死。

    2026-10-04 修正：原来只支持 GET + Cookie，实际有平台是
      · method=POST（CodeBuddy 国内站的 get-user-resource）
      · type=bearer（所有 copilot.tencent.com 系）
      · 自带 ua（少了伪装 UA，/v3/config 报 12403 check ua）
    这三种都走同一个函数，所以按 spec 动态组装。
    """
    v = spec.get("verify")
    if not v or not cookie:
        # 没有 verify 定义：如果显式标了 verify_pending 就说清「没验过」，
        # 别返回「已登录」让人误以为验过了
        pend = spec.get("verify_pending")
        if pend:
            return True, "未校验（%s）" % pend
        return True, "未校验"
    # ★ 离线验活：有些平台（豆包）的接口全部要请求签名，在线根本验不了，
    #   只能退而求其次——看 Cookie 里有没有会话标记键。这仍能拦住「没登录」，
    #   但不会像在线 401 那样把有效 cookie 一律判死。
    mk = v.get("session_markers") if v else None
    if mk and cookie:
        found = _cookie_markers(cookie, mk)
        if not found:
            return False, ("Cookie 里没有会话标记（%s）——多半是没登录成功，"
                           "请在打开的浏览器里重新登录豆包" % "/".join(mk[:3]))
        return True, "已取得会话 Cookie（%s；该平台接口需签名，无法在线校验）" % \
            ",".join(found[:4])
    import ssl
    import urllib.error
    import urllib.request
    url = v.get("url")
    if not url:
        return True, "未校验"
    # @lobster 哨兵：上游地址由 设置→Lobster 上游地址 提供（与 gwextra/
    # native_relay 同一套约定）。gwlogin 拿不到 main.APP，直接读
    # data/settings.json（冻结时 data 目录在 exe 同级，同 main.base_dir）。
    if "@lobster" in url:
        _root = (os.path.dirname(sys.executable) if getattr(sys, "frozen", False)
                 else os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        _lob = ""
        try:
            with open(os.path.join(_root, "data", "settings.json"),
                      encoding="utf-8") as _f:
                _lob = ((json.load(_f) or {}).get("lobster_server") or "").strip()
        except Exception:
            _lob = ""
        if not _lob:
            return True, "未校验（设置→「Lobster 上游地址」未填，无法在线验活）"
        url = url.replace("https://@lobster", _lob.rstrip("/"))
    method = (v.get("method") or "GET").upper()
    vtype = v.get("type") or spec.get("type")
    hdrs = {
        "User-Agent": v.get("ua") or spec.get("ua") or
                      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                      "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/154.0 Safari/537.36",
        "Accept": "application/json",
    }
    # bearer 类的 token 长得像 JWT（含 . 且长度大）
    looks_jwt = ("=" in cookie[:40] and " " not in cookie[:40] and len(cookie) > 80)
    if vtype in ("bearer", "api_key") or looks_jwt:
        hdrs["Authorization"] = "Bearer " + cookie
    else:
        hdrs["Cookie"] = cookie
        # ★ 双发：凭据里带 JWT 形态的 token=（eyJ 开头）时，同时给 Bearer 头。
        #   实测 2026-10-05：qwenwork.cn 的网关 JWT 中间件只认
        #   「Authorization: Bearer」或「cookie 名 token=」——
        #   只发 Cookie 会因中间件不认识其它 cookie 名而报「JWT is missing」。
        import re as _re
        _m = _re.search(r"(?:^|;\s*)token=(eyJ[\w\-]+\.[\w\-]+\.[\w\-]+)", cookie or "")
        if _m:
            hdrs["Authorization"] = "Bearer " + _m.group(1)
    data = b"{}" if method == "POST" else None
    if data is not None:
        hdrs["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    for k in list(os.environ):
        if k.upper().endswith("_PROXY"):
            os.environ.pop(k, None)
    try:
        with urllib.request.urlopen(req, timeout=12, context=ctx) as r:
            raw = r.read().decode("utf-8", "replace")
            code = r.status
    except urllib.error.HTTPError as e:
        # 401/403 说明凭据无效；其它（404/405）视为接口形态变化，不判死
        if e.code in (401, 403):
            # ★ 空 body 的 401 是**网关层**拦截，不是应用层的鉴权结论。
            #   实测豆包（openresty）带真 cookie / 带假 cookie / 不带 cookie
            #   返回的都是同一个「401 + 空 body」——它**证明不了**凭据无效
            #   （真正原因是要 a_bogus 签名）。判死会把好 cookie 全否掉。
            #   只有"带响应体"的 401 才是应用层说「你没登录」。
            try:
                _eb = (e.read(512) or b"").strip()
            except Exception:
                _eb = b""
            if not _eb:
                return True, ("接口返回 %d 但无响应体（网关层拦截，多因需签名），"
                              "无法判定凭据是否有效，按可用处理" % e.code)
            # ★ JWT 鉴权站（实测 qwenwork.cn）：401 的 body 是「JWT is missing /
            #   JWT verification fails」—— 给用户能落地的提示，别只说凭据无效
            try:
                _ebt = _eb.decode("utf-8", "replace")
            except Exception:
                _ebt = ""
            if "JWT" in _ebt:
                if "verification fails" in _ebt:
                    return False, ("JWT 鉴权失败：找到了 token 但已失效/过期，"
                                   "请重新登录后再粘贴（该站只认 cookie 名 "
                                   "token= 或 Authorization Bearer）")
                return False, ("该站是 JWT 鉴权：凭据里没有叫 token= 的 JWT。"
                               "请在登录后的浏览器里取包含 token=eyJ… 的 "
                               "Cookie/或 localStorage 里的 JWT 再粘贴")
            return False, "接口返回 %d，凭据无效或已过期" % e.code
        return True, "校验接口返回 %d（未判定为无效）" % e.code
    except Exception as e:
        return True, "校验请求未完成（%s），按可用处理" % type(e).__name__

    if code >= 400:
        return False, "接口返回 %d" % code
    # ★ 2026-10-04 实测：copilot.tencent.com/console/account 在**未登录/无凭据**
    #   时直接返回 200 + Keycloak 登录页 HTML（class="login-pf"），不是 401。
    #   旧代码走到下面 json.loads 失败就「按可用处理」→ 返回 True，
    #   于是 wb-gateway / wb-gateway-intl / apk-trae / apk-codebuddy / apk-go
    #   这 5 个平台**用空凭据也能显示登录成功**。验活端点必须能区分真假，
    #   所以拿到 HTML 一律判未登录。
    _s = (raw or "").lstrip()
    if _s.startswith("<") or _s.lower().startswith("<!doctype"):
        return False, "返回登录页 HTML（未登录或凭据无效）"
    try:
        d = json.loads(raw)
    except json.JSONDecodeError:
        return True, "接口返回非 JSON，按可用处理"
    if isinstance(d, dict) and d.get("code") not in (0, None):
        # 业务码非 0 = 未登录/无权限（实测 nano 是 110001、metaso 也有业务码）
        return False, "业务码 %s：%s" % (d.get("code"),
                                        str(d.get("msg") or d.get("message") or "")[:50])
    # ★ 有些站（库库 baidu 系）不用 HTTP 状态码表达登录态：
    #   200 + errno:-6 + show_msg:"未登录" 才是未登录。
    #   verify 里用 errno_field / unlogin_errno 声明这种情况。
    ef = v.get("errno_field")
    if ef and isinstance(d, dict):
        try:
            ev = int(d.get(ef))
        except (TypeError, ValueError):
            ev = None
        if ev is not None and ev == int(v.get("unlogin_errno", -6)):
            return False, "%s=%s（%s）" % (ef, ev,
                                          d.get(v.get("msg_field", "show_msg")) or "未登录")
        if ev is not None and ev == 0:
            # 业务码对了，再看标记字段
            keys = v.get("ok_keys") or []
            if keys and not _has_any(d, keys, 0):
                return False, "已登录但响应里没有 %s 字段" % "/".join(keys[:2])
            return True, "已登录（%s=0）" % ef
    keys = v.get("ok_keys") or []
    if keys and not _has_any(d, keys, 0):
        # 返回了合法 JSON 但找不到任何标记字段 → 多半是未登录页
        # ⚠ 两个坑：
        #   1) 不能只 `k in d` 查顶层 —— copilot 系的数据在 d["data"] 里嵌套
        #   2) 不能用 _dig 找 list/dict 类型的值（它只收 str/int）
        return False, "响应中未找到标记字段（%s），可能未登录" % "/".join(keys[:3])
    for k in ("nickname", "name", "user_name", "username", "email", "user_id", "userId"):
        val = _dig(d, k)
        if val:
            return True, str(val)
    return True, "已登录"


def _cookie_markers(cookie, markers):
    """
    从 Cookie 串里挑出「会话标记」键（值非空才算）。
    用于无法在线验活的平台（豆包）：Cookie 里只要出现 sessionid / sid_tt /
    passport_csrf_token 这类键，就认为确实登录过。
    """
    found = []
    try:
        for part in (cookie or "").split(";"):
            part = part.strip()
            if "=" not in part:
                continue
            k, val = part.split("=", 1)
            k, val = k.strip(), val.strip()
            if k in markers and val:
                found.append(k)
    except Exception:
        pass
    return found


def _has_any(d, keys, depth=0):
    """
    递归判断 d 里是否出现 keys 中任一键（值可以是任意类型）。
    和 _dig 的区别：_dig 只认 str/int 的值，models/agents 这种 list 会被漏掉。
    """
    if depth > 5 or not isinstance(d, dict):
        return False
    for k in keys:
        if k in d and d[k] not in (None, "", [], {}):
            return True
    for v in d.values():
        if isinstance(v, dict) and _has_any(v, keys, depth + 1):
            return True
    return False


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
