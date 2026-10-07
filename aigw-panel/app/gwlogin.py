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

import glob
import json
import os
import shutil
import sys
import threading
import time
import urllib.parse
import uuid

# ------------------------------------------------------------------ 平台规格

# ★ 服务端无头浏览器的**唯一**调试端口（2026-10-06 用户点破后定型）。
#   以前为防止多平台串台，把各平台散列到 9333~9992 —— 那是绕路。
#   正解是「一个端口 + owner 归属校验 + 前端离开即收口 + 空闲超时兜底」，
#   详见 LoginManager._try_headless 与 cdp.Browser.start 的注释。
CDP_PORT = 9333

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
                "（gpt-4o / claude-3-7-sonnet / deepseek-v3 / deepseek-r1 等）。"
                "★ 嫌手动抠麻烦？在「腾讯/原生登录」页点「读取本机 Trae 登录态」"
                "（/api/tencent?action=trae_local）可一键自动提取并落库（需面板与本机 "
                "Trae 同一台电脑）。",
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
        # ★ 2026-10-06 无头实测：豆包网页版**有扫码登录**——点右上角「登录」
        #   弹窗里默认就展示二维码（标题「使用豆包或飞书账号登录」，
        #   下方「打开 豆包 / 飞书 App 扫码登录」）。所以扫码路径完全可用，
        #   无头浏览器点开弹窗 → 抓码 → 手机扫 → Cookie 自动入库。
        "login_click": ["账号登录", "登录"],
        # ✅ 2026-10-06 无头 E2E 实测通过：点登录 → 弹窗出码 → 抓回面板（164x162）
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
        # ★ 首页不直接出码，要点「登录」进登录面板才出二维码（2026-10-06 无头实测）
        "login_click": ["Log In", "登录", "Sign in", "扫码登录"],
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
        # ★ 实测扫码页盖着「请阅读并同意《服务条款》《隐私政策》」弹窗，
        #   要先点「同意并继续」二维码才可扫
        "login_click": ["同意并继续", "同意", "扫码登录", "登录", "立即登录"],
        # ✅ 2026-10-06 无头 E2E 实测通过（182x182）；先点「同意并继续」过协议弹窗
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
        # ★ 2026-10-06 补齐：多份 Loomy 上手教程一致写明「扫码登录讯飞账号，
        #   或使用手机号+验证码」，所以登录入口是有的，只是之前没配点击词，
        #   无头浏览器打开登录页后无从下手 → 抓不到码。
        "login_click": ["扫码登录", "登录", "立即登录", "立即注册"],
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
                "pluginType=GEMINI）。需 Google 账号，取 oauth2 token 或 Cookie。"
                "⚠ 2026-10-06 实测：accounts.google.com 在服务端无头浏览器里"
                "抓不到二维码——Google 官方说明 QR 登录是「用已登录手机给电脑"
                "做二次验证」，且明确写了「电脑端可能无法扫描」，遇风控/VPN 更会"
                "直接隐藏 QR 选项退回密码登录。故本平台**不支持远程扫码**，"
                "请在自己电脑浏览器登录后手动粘贴 Cookie（或 oauth2 token）",
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
        # ★ 2026-10-06 补齐：阿里云官方帮助文档写明千问办公网页端登录页是
        #   qwenwork.cn/signin，支持**钉钉扫码登录**（也可手机号验证码）。
        #   原来 login_url 是首页 qwenwork.cn/ 且没有 login_click，无头浏览器
        #   打开首页后没有任何登录入口 → 抓不到码（实测 45 秒 hint 一直 False）。
        "login_url": "https://qwenwork.cn/signin",
        "login_click": ["扫码登录", "登录", "立即登录"],
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
        "login_click": ["立即登录", "登录", "扫码登录", "账号登录", "去登录"],
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
        # ★ 2026-10-06 无头实测：点「登录」弹出小程序码（180x180，带 code 语义）
        "login_click": ["登录", "立即登录", "免费登录", "扫码登录"],
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
        "login_click": ["立即登录", "登录", "扫码登录", "账号登录", "去登录"],
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
        "login_click": ["登录", "立即登录", "扫码登录", "登录/注册"],
        # ✅ 2026-10-06 无头 E2E 实测通过：点登录出「微信扫码登录」（175x185）
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
            "qr": pid in _remote_qr_platforms(),
        })
    return out


_REMOTE_QR_PLATFORMS = None


def _remote_qr_platforms():
    """惰性取远程扫码支持的平台（避免模块加载期循环依赖）。"""
    global _REMOTE_QR_PLATFORMS
    if _REMOTE_QR_PLATFORMS is None:
        try:
            from . import qrlogin
            _REMOTE_QR_PLATFORMS = set(qrlogin.supported_platforms())
        except Exception:
            _REMOTE_QR_PLATFORMS = set()
    return _REMOTE_QR_PLATFORMS


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
        self.qr_text = ""              # 待编码成二维码的文本（如 Google 核验 URL）
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
                "qr": self.qr, "qr_text": self.qr_text, "auth_url": self.auth_url,
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


def _is_blank_png(b64_png, sample_step=97):
    """
    判断 base64 PNG 是否「几乎纯色」（页面还没渲染出内容的空白截图）。
    用法：decode → 跳过 PNG 头 → 按固定步长采样若干字节 → 看取值是否单一。
    不依赖 PIL；任何异常一律返回 False（宁可放过，也不误杀真码）。
    """
    try:
        import base64 as _b64
        import zlib as _zl
        raw = _b64.b64decode(b64_png)
        if len(raw) < 100:
            return False
        # PNG: 8 字节签名 + IHDR(25) → 之后是 IDAT 压缩流
        pos, idat = 8, b""
        while pos + 8 <= len(raw):
            ln = int.from_bytes(raw[pos:pos + 4], "big")
            typ = raw[pos + 4:pos + 8]
            if typ == b"IDAT":
                idat += raw[pos + 8:pos + 8 + ln]
                break
            pos += 12 + ln
        if not idat:
            return False
        data = _zl.decompressobj().decompress(idat)
        if len(data) < 200:
            return False
        vals = set()
        for i in range(0, len(data), sample_step):
            vals.add(data[i])
            if len(vals) > 3:
                return False        # 有多种颜色 → 不是空白
        return len(vals) <= 2
    except Exception:
        return False


def _png_gray(b64_png):
    """
    PNG → 灰度像素（纯标准库：IHDR/IDAT 解析 + zlib 解压 + 行滤镜还原）。
    返回 (gray bytearray, w, h)；无法解码时 h/w 为 0。
    原是 _looks_like_qr 的内联段，2026-10-06 抽出来供诊断脚本复用。
    """
    import base64 as _b64
    import zlib as _zl
    gray, w, h = bytearray(0), 0, 0
    try:
        raw = _b64.b64decode(b64_png)
    except Exception:
        return gray, 0, 0
    pos, ct = 8, 6
    idat = b""
    while pos + 8 <= len(raw):
        ln = int.from_bytes(raw[pos:pos + 4], "big")
        typ = raw[pos + 4:pos + 8]
        if typ == b"IHDR":
            w = int.from_bytes(raw[pos + 8:pos + 12], "big")
            h = int.from_bytes(raw[pos + 12:pos + 16], "big")
            bd, ct = raw[pos + 16], raw[pos + 17]
            if bd != 8 or ct not in (0, 2, 4, 6):
                return gray, 0, 0
        elif typ == b"IDAT":
            idat += raw[pos + 8:pos + 8 + ln]
        elif typ == b"IEND":
            break
        pos += 12 + ln
    if not idat or w <= 0 or h <= 0:
        return gray, 0, 0
    ch = {0: 1, 2: 3, 4: 2, 6: 4}[ct]
    try:
        data = _zl.decompressobj().decompress(idat)
    except Exception:
        return gray, 0, 0
    stride = w * ch
    if len(data) < (stride + 1) * h * 0.5:
        return gray, 0, 0
    prev = bytearray(stride)
    gray = bytearray(w * h)
    off = 0
    for y in range(h):
        if off + 1 + stride > len(data):
            break
        ft = data[off]
        line = bytearray(data[off + 1:off + 1 + stride])
        off += 1 + stride
        if ft == 1:
            for i in range(ch, stride):
                line[i] = (line[i] + line[i - ch]) & 0xFF
        elif ft == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ft == 3:
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ft == 4:
            for i in range(stride):
                a = line[i - ch] if i >= ch else 0
                b = prev[i]
                c = prev[i - ch] if i >= ch else 0
                pp = a + b - c
                pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        base = y * w
        if ch >= 3:
            for x in range(w):
                i = x * ch
                gray[base + x] = (line[i] * 299 + line[i + 1] * 587
                                  + line[i + 2] * 114) // 1000
        elif ch == 1:
            gray[base:base + w] = line[:w]
        prev = line
    return gray, w, h


def _looks_like_qr(b64_png):
    """
    判断截图里是否**真的存在二维码**——纯标准库解码 PNG 像素做结构判定。

    ★ 2026-10-06 第三版（v1/v2 都被真实截图证伪，见下）：
      现实中的 AI 登录页大量使用**装饰码**：WPS 圆角码、秘塔圆形艺术码、
      豆包带头像水印的圆角码。它们扫描完全正常，但：
        · v1「严格 1:1:3:1:1 定位点」认不出——实测 WPS 码 finder 横截面
          是 5:8:6:9:5，压根不符合 1:1:3:1:1；6/6 个真码全被误杀。
        · v2「自相关找模块周期」也不行——二维码模块黑白近似随机，
          自相关单调衰减无周期峰（raccoon 从 d=2 的 0.909 掉到 d=19 的 0.592）。
      本版用**双通道判定**（任一通过即算真码）：
        通道 A（强）：放宽版定位点扫描 —— five-segment 1,0,1,0,1 且
                      外侧四段彼此接近、中心段 ≥1.6×外侧；再要求找得到
                      「左上/右上/左下」直角三角布局。这条极特异，实测
                      零误报，能抓住标准码与部分装饰码。
        通道 B（兜底）：模块纹理网格 —— 用水平游程中位数当模块尺度切网格，
                      统计相邻格差异率。真码差异率高(≥0.16)且网格够细
                      (≥15×15)；整页 UI 差异率普遍 <0.14。
      外加两条硬闸门：近方形（AR≤2.7）+ 黑色占比在 [0.06,0.70]
      （全黑 logo dark=1.0、空白 dark≈0 直接出局）。

    解码失败等异常一律返回 True（宁可放过，不误杀真码）。
    回归集：test_qr_detect.py（21 张真实截图，含 9 真码 / 12 非码）。
    """
    try:
        import base64 as _b64
        import zlib as _zl
        raw = _b64.b64decode(b64_png)
        pos, w, h, ct = 8, 0, 0, 6
        idat = b""
        while pos + 8 <= len(raw):
            ln = int.from_bytes(raw[pos:pos + 4], "big")
            typ = raw[pos + 4:pos + 8]
            if typ == b"IHDR":
                w = int.from_bytes(raw[pos + 8:pos + 12], "big")
                h = int.from_bytes(raw[pos + 12:pos + 16], "big")
                bd, ct = raw[pos + 16], raw[pos + 17]
                if bd != 8 or ct not in (0, 2, 4, 6) or raw[pos + 20] != 0:
                    return True
            elif typ == b"IDAT":
                idat += raw[pos + 8:pos + 8 + ln]
            elif typ == b"IEND":
                break
            pos += 12 + ln
        if not idat or w <= 0 or h <= 0 or w < 40 or h < 40:
            return True
        ch = {0: 1, 2: 3, 4: 2, 6: 4}[ct]
        data = _zl.decompressobj().decompress(idat)
        stride = w * ch
        if len(data) < (stride + 1) * h * 0.5:
            return True
        # --- PNG 行滤镜还原 → 灰度图 ---
        prev = bytearray(stride)
        gray = bytearray(w * h)
        off = 0
        for y in range(h):
            if off + 1 + stride > len(data):
                break
            ft = data[off]
            line = bytearray(data[off + 1:off + 1 + stride])
            off += 1 + stride
            if ft == 1:
                for i in range(ch, stride):
                    line[i] = (line[i] + line[i - ch]) & 0xFF
            elif ft == 2:
                for i in range(stride):
                    line[i] = (line[i] + prev[i]) & 0xFF
            elif ft == 3:
                for i in range(stride):
                    a = line[i - ch] if i >= ch else 0
                    line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
            elif ft == 4:
                for i in range(stride):
                    a = line[i - ch] if i >= ch else 0
                    b = prev[i]
                    c = prev[i - ch] if i >= ch else 0
                    pp = a + b - c
                    pa, pb, pc = abs(pp - a), abs(pp - b), abs(pp - c)
                    pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                    line[i] = (line[i] + pr) & 0xFF
            base = y * w
            if ch >= 3:
                for x in range(w):
                    i = x * ch
                    gray[base + x] = (line[i] * 299 + line[i + 1] * 587
                                      + line[i + 2] * 114) // 1000
            elif ch == 1:
                gray[base:base + w] = line[:w]
            else:
                for x in range(w):
                    gray[base + x] = line[x * ch]
            prev = line

        # --- 闸门 1：近方形（抓码时按元素裁剪，裁剪框本就接近方形）---
        if max(w, h) / float(min(w, h)) > 2.7:
            return False

        # --- 反色（暗色主题）：白色模块 + 深色底。dark≈0.9 时整体取反再判 ---
        dark = sum(1 for v in gray if v < 128) / float(w * h)
        if dark > 0.70:
            for i in range(len(gray)):
                gray[i] = 255 - gray[i]
            dark = 1.0 - dark

        # --- 闸门 2：黑色占比 ---
        if not (0.06 <= dark <= 0.70):
            return False

        # --- 通道 A：放宽版定位点 + 三角布局 ---
        if _qr_three_corner(gray, w, h):
            return True
        # --- 通道 B：模块纹理网格差异率 ---
        return _qr_texture_grid(gray, w, h, dark)
    except Exception:
        return True


def _line_finders(bits):
    """在一行/列二值序列里找放宽版定位点（1,0,1,0,1 且中心段明显偏大）。
    返回 [(中心位置, 外侧模块宽近似, 总跨度)]。"""
    out = []
    n = len(bits)
    runs = []
    i = 0
    while i < n:
        j = i
        while j < n and bits[j] == bits[i]:
            j += 1
        runs.append((bits[i], i, j - i))
        i = j
    for k in range(len(runs) - 4):
        if [runs[k + m][0] for m in range(5)] != [1, 0, 1, 0, 1]:
            continue
        a = runs[k][2]
        b = runs[k + 1][2]
        c = runs[k + 2][2]
        d = runs[k + 3][2]
        e = runs[k + 4][2]
        outer = (a + b + d + e) / 4.0
        if outer < 1.2 or c <= 0:
            continue
        omin = min(a, b, d, e)
        if omin <= 0 or max(a, b, d, e) / float(omin) > 2.4:
            continue
        if c < outer * 1.6:
            continue
        out.append((runs[k][1] + a + b + c // 2, outer, a + b + c + d + e))
    return out


def _qr_three_corner(gray, w, h):
    """放宽版定位点扫描 + 左上/右上/左下直角三角布局校验"""
    pts = []
    for y in range(0, h, 2):
        base = y * w
        bits = [1 if gray[base + x] < 128 else 0 for x in range(w)]
        for cx, mod, span in _line_finders(bits):
            if 3.5 <= span / max(0.8, mod) <= 12.0:
                pts.append((cx, y, mod))
    for x in range(0, w, 2):
        bits = [1 if gray[k * w + x] < 128 else 0 for k in range(h)]
        for cy, mod, span in _line_finders(bits):
            if 3.5 <= span / max(0.8, mod) <= 12.0:
                pts.append((x, cy, mod))
    # 聚类成 2D 定位点
    clusters = []
    for x, y, mod in pts:
        for cl in clusters:
            if abs(cl[0] - x) <= 6 and abs(cl[1] - y) <= 6:
                nn = cl[5]
                cl[0] = (cl[0] * nn + x) / (nn + 1.0)
                cl[1] = (cl[1] * nn + y) / (nn + 1.0)
                cl[2] = (cl[2] * nn + mod) / (nn + 1.0)
                cl[5] = nn + 1
                break
        else:
            clusters.append([float(x), float(y), float(mod), 1, 1, 1])
    fs = [c for c in clusters if c[5] >= 3 and c[2] >= 1.8]
    if len(fs) < 3:
        return False
    for i in range(len(fs)):
        for j in range(i + 1, len(fs)):
            for k in range(j + 1, len(fs)):
                tri = [fs[i], fs[j], fs[k]]
                mods = [t[2] for t in tri]
                if max(mods) / max(0.8, min(mods)) > 2.4:
                    continue
                srt = sorted(tri, key=lambda t: (t[1], t[0]))
                tl = srt[0]
                rest = sorted(srt[1:], key=lambda t: t[0])
                tr, bl = rest[0], rest[1]
                if bl[1] < tr[1]:
                    tr, bl = bl, tr
                wl = tr[0] - tl[0]
                hl = bl[1] - tl[1]
                if wl < 30 or hl < 30:
                    continue
                if abs(tr[1] - tl[1]) > max(6.0, wl * 0.18):
                    continue
                if abs(bl[0] - tl[0]) > max(6.0, hl * 0.18):
                    continue
                if abs(wl - hl) > max(12.0, wl * 0.30):
                    continue
                return True
    return False


def _qr_texture_grid(gray, w, h, dark):
    """
    模块纹理网格差异率：用水平游程中位数近似模块像素宽，按该尺度切网格，
    统计相邻格异色比例。真码 ≥0.16 且网格 ≥15×15。
    """
    # 水平游程中位数
    runs = []
    for y in range(0, h, 2):
        base = y * w
        start = 0
        prev = 1 if gray[base] < 128 else 0
        for x in range(1, w):
            cur = 1 if gray[base + x] < 128 else 0
            if cur != prev:
                runs.append(x - start)
                start = x
                prev = cur
        runs.append(w - start)
    if not runs:
        return False
    runs.sort()
    mod = float(runs[len(runs) // 2])
    if mod < 2.0:
        mod = 2.0
    nx = int(w // mod)
    ny = int(h // mod)
    if nx < 15 or ny < 15:
        return False
    # 逐格取样
    grid = []
    for gy in range(ny):
        y = min(h - 1, int((gy + 0.5) * mod))
        base = y * w
        row = []
        for gx in range(nx):
            x = min(w - 1, int((gx + 0.5) * mod))
            row.append(1 if gray[base + x] < 128 else 0)
        grid.append(row)
    diff = 0
    tot = 0
    for gy in range(ny):
        row = grid[gy]
        nrow = grid[gy + 1] if gy + 1 < ny else None
        for gx in range(nx):
            if gx + 1 < nx:
                if row[gx] != row[gx + 1]:
                    diff += 1
                tot += 1
            if nrow is not None:
                if row[gx] != nrow[gx]:
                    diff += 1
                tot += 1
    if tot == 0:
        return False
    rate = diff / float(tot)
    return rate >= 0.16 and 0.12 <= dark <= 0.62



def _find_headless_chrome():
    """
    找服务端无头 Chromium（2026-10-06 新增，远程部署兜底）。
    优先级：AIGW_HEADLESS_CHROME 环境变量 → Playwright 缓存的 Chrome for
    Testing / headless_shell → 系统 PATH 里的 chromium / chrome。
    找不到返回 ""（上层据此报错并引导手动粘贴 Cookie）。
    """
    exe = os.environ.get("AIGW_HEADLESS_CHROME", "")
    if exe and os.path.isfile(exe) and os.access(exe, os.X_OK):
        return exe
    home = os.path.expanduser("~")
    pats = (
        # 完整 chrome 支持 --headless=new，优先于 headless_shell
        home + "/.cache/ms-playwright/chromium-*/chrome-linux*/chrome",
        home + "/.cache/ms-playwright/chromium_headless_shell-*/chrome-linux*/headless_shell",
        "/usr/bin/chromium", "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable",
    )
    for pat in pats:
        hits = [c for c in sorted(glob.glob(pat), reverse=True)
                if os.path.isfile(c) and os.access(c, os.X_OK)]
        if hits:
            return hits[0]
    for name in ("chromium", "chromium-browser", "google-chrome", "chrome"):
        w = shutil.which(name)
        if w:
            return w
    return ""


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

    def start_remote_qr(self, platform):
        """服务端远程扫码（无需本机浏览器）：直接打平台 Web 二维码接口。
        不支持的平台会在会话里返回明确错误，自动降级。"""
        from . import qrlogin
        return qrlogin.start_remote_qr(self, platform)

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
        # ★ 2026-10-06：headless=1 → 跳过本机浏览器，直接用服务端无头 Chromium
        #   打开登录页。二维码由看守线程抓回面板，手机扫完 Cookie 自动落库——
        #   远程部署（面板跑在服务器上）不再被「需要本机 Chrome」卡死。
        if sess.opts.get("headless"):
            b = self._try_headless(sess, spec, int(sess.opts.get("port")
                                                   or CDP_PORT))
            if b is None:
                sess.finish("error",
                            "服务端无头浏览器不可用（%s）。请改用下方的"
                            "「手动粘贴 Cookie」。" % (sess.message or "未找到 Chromium"),
                            "no_headless")
                return
            sess._browser = b
            sess.finish("waiting",
                        "已在服务器上启动无头浏览器并打开登录页，页面二维码会抓到下方——"
                        "用手机扫即可，登录成功后 Cookie 自动入库；"
                        "也可以直接在下方手动粘贴。", result=None)
            threading.Thread(target=self._watch_cookie,
                             args=(sess, spec, b), daemon=True).start()
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
        # 单一固定端口 + owner 归属校验（详见 _try_headless 注释）：
        # 不再散列端口，靠 cdp.Browser 的归属校验保证不串台。
        _owner = "%s-%s" % (sess.platform, sess.id[:6])
        port = int(sess.opts.get("port") or CDP_PORT)
        b = cdp.Browser(port=port, owner=_owner)
        try:
            b.start(url=spec.get("login_url") or ("https://" + (hosts[0] if hosts else "")))
        except Exception as e:
            # ★ 本机没有可用 Chrome / Edge 时不再直接判死：自动回退到
            #   服务端无头浏览器（远程部署的标配路径），找不到才报错。
            b = self._try_headless(sess, spec, port)
            if b is None:
                sess.finish("error",
                            "本机没有可用的 Chrome / Edge（%s），服务端也无头浏览器可用；"
                            "请改用下方的「手动粘贴 Cookie」。" % e,
                            "no_browser")
                return
            sess._browser = b
            sess.finish("waiting",
                        "本机浏览器不可用，已改用服务端无头浏览器打开登录页："
                        "二维码会抓到下方，用手机扫即可，登录成功后 Cookie 自动入库。",
                        result=None)
            threading.Thread(target=self._watch_cookie,
                             args=(sess, spec, b), daemon=True).start()
            return
        sess._browser = b
        sess.finish("waiting",
                    "已打开浏览器登录页，请用手机扫描页面上的二维码完成登录；"
                    "面板也会把二维码抓到下方方便你扫。登录成功会自动捕捉，"
                    "或点「我已登录，立即获取」手动获取。", result=None)
        # ★ 强化：后台线程抓登录页二维码进面板 + 自动检测登录完成
        threading.Thread(target=self._watch_cookie,
                         args=(sess, spec, b), daemon=True).start()

    # ------------------------------------------------- 服务端无头浏览器兜底
    def _try_headless(self, sess, spec, port):
        """
        用服务端无头 Chromium 起 CDP 登录会话（远程部署的兜底路径）。
        复用 cdp.Browser（--headless=new），看守线程 / Cookie 轮询 /
        localStorage 补抓等逻辑全部原样生效。失败返回 None，原因写 sess.message。

        ★ 2026-10-06 修「点元宝出豆包码」串台 bug，思路两次迭代：
          · 第一版把各平台散列到 9333~9992 的不同端口 —— 能防串台但是绕路
            （占一堆端口、逻辑绕，且没解决「浏览器没人收口」的问题）。
          · 现行版（用户点破后的正解）：**就一个固定端口 CDP_PORT**，
            防串台靠 cdp.Browser.start() 里的 owner 归属校验 ——
            端口上若不是自己的浏览器就拒绝复用、直接报错，绝不连错目标。
            浏览器**生命周期由前端收口**：用户点「返回源列表」即触发
            cancel → sess._browser.close()（见 app.js closeSrc / leaveLoginView），
            另有这里的 watch 线程 timeout 做空闲兜底，不会一直挂着占端口。
        """
        from . import cdp
        exe = _find_headless_chrome()
        if not exe:
            sess.message = ("未找到 Chromium，可设环境变量 AIGW_HEADLESS_CHROME "
                            "指向可执行文件")
            return None
        # 会话专属 owner：同平台同会话复用自己的浏览器，跨平台绝不串台
        owner = "%s-%s" % (sess.platform, sess.id[:6])
        b = cdp.Browser(port=int(port or CDP_PORT), exe=exe,
                        headless=True, owner=owner)
        try:
            b.start(url=spec.get("login_url")
                    or ("https://" + ((sess.opts.get("hosts")
                                       or spec.get("hosts") or [""])[0])))
        except Exception as e:
            # ★ 端口被别的登录会话占用（单端口策略下的正常情况，不是缺陷）：
            #   自动清掉上一个会话的浏览器再重试一次，用户无需手动干预。
            if self._recycle_port(sess, b):
                try:
                    b.start(url=spec.get("login_url")
                            or ("https://" + ((sess.opts.get("hosts")
                                               or spec.get("hosts") or [""])[0])))
                except Exception as e2:
                    sess.message = "无头浏览器启动失败：%s" % e2
                    return None
            else:
                sess.message = "无头浏览器启动失败：%s" % e
                return None
        sess.opts["_cdp_port"] = b.port      # 便于诊断 / 取消时清理
        sess.opts["_cdp_owner"] = owner
        return b

    def _recycle_port(self, sess, browser):
        """
        单端口被上一个登录会话占用时的回收：把**别的**等待中会话结束掉，
        释放端口后由调用方重试起浏览器。返回 True 表示已腾出端口。
        """
        freed = False
        with self.lock:
            others = [v for v in self.sessions.values()
                      if v is not sess and v._browser
                      and v.status in ("pending", "waiting")]
        for v in others:
            try:
                v._stop = True
                v._browser.close()
                v.finish("error", "已被新的登录会话接管（单端口串行）", "replaced")
                freed = True
            except Exception:
                pass
        if not freed:
            return False
        # 端口真正空出来再返回（close 内部已等到端口释放，这里再兜一次）
        p = browser.port
        probe = browser.__class__(port=p)
        for _ in range(20):
            if not probe.is_up():
                return True
            time.sleep(0.5)
        return True

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
        if not header:
            with sess.lock:
                sess.message = ("未检测到 %s 的登录态（Cookie/localStorage），"
                                "请先在浏览器里登录" % "/".join(hosts))
            return
        # 浏览器会话来源：验活失败只提示、不打死会话（登录页本来就有
        # 未登录 Cookie，401 ≠ 登录失败）；手动粘贴路径才 strict。
        self._verify_and_finish(sess, spec, header,
                                "服务端无头浏览器" if sess.opts.get("headless") else "CDP 浏览器",
                                strict=False)

    def _collect_browser_cred(self, sess, spec, b):
        """
        组装浏览器会话的完整凭据串：Cookie + storage_keys 合并。
        _poll_cookie 与 _watch_cookie 共用，保证两边判断口径一致
        （2026-10-06 修正：看守线程以前只查 Cookie，漏掉元宝这类
        登录态在 localStorage 的平台，永远验不过）。
        """
        hosts = sess.opts.get("hosts") or spec.get("hosts") or []
        try:
            header, _rows = b.cookie_header(hosts)
        except Exception:
            header = ""
        skeys = spec.get("storage_keys") or []
        if skeys and header is not None:
            try:
                import urllib.parse as _up
                parts = []
                for k in skeys:
                    v = b.storage_item(k, match=(hosts[0] if hosts else None))
                    if v:
                        parts.append("__ls_%s=%s" % (k, _up.quote(str(v), safe="")))
                if parts:
                    header = ((header or "") + "; " + "; ".join(parts)).strip("; ")
            except Exception:
                pass
        return header or ""

    def _verify_and_finish(self, sess, spec, cookie, source, strict=True):
        """
        验活并落库。
          strict=True  （手动粘贴）：失败直接 error，让用户知道粘的东西没用；
          strict=False （浏览器自动捕捉）：失败只更新提示并保持 waiting——
            登录页未登录状态下也带着设备 Cookie，拿去验活必然 401，
            以前直接 finish("error") 把整个扫码会话杀掉，属于误伤。
        """
        if sess.status == "success":
            return True
        okk, info = verify_cookie(spec, cookie)
        if not okk:
            if strict:
                sess.finish("error", "Cookie 已取得但未通过登录校验", info)
                return False
            with sess.lock:
                sess.message = ("已检测到凭据但登录校验未通过（%s），"
                                "请继续完成扫码登录" % info)
            return False
        sess.finish("success", "登录成功（来源：%s）" % (source or "未知"),
                    result={"type": "cookie", "secret": cookie, "account": info,
                            "source": source})
        self._save(sess)
        return True

    def _finish_cookie(self, sess, spec, cookie, source=""):
        # 防止「看守线程自动捕捉」与「前端手动点已登录」重复落库
        self._verify_and_finish(sess, spec, cookie, source, strict=True)

    # ---------------------------------------------------------- 扫码看守线程
    def _watch_cookie(self, sess, spec, b, timeout=240):
        """
        后台看守：
          1) 等登录页加载后，把二维码抓回面板（优先只截二维码元素，否则整页截图），
             写进 sess.qr，前端直接渲染 —— 用户用手机扫，不必去盯弹出的浏览器；
          2) 周期性检测登录 Cookie，一旦拿到就自动落库（免去手动点「我已登录」）。
        超时未登录则**主动关掉浏览器**（释放单端口），不残留僵尸线程/进程。

        ★ 2026-10-06 加「等码超时」：以前只认总超时 240 秒，页面万一压根没
          渲染出二维码，浏览器就会空转 4 分钟白占着 9333 端口。现在抓不到码
          先给 90 秒（足够慢站点首屏 + 弹窗动画），仍抓不到就判定「该平台
          无扫码入口」，收浏览器并给出改用 Cookie 粘贴的明确提示。
        """
        hosts = sess.opts.get("hosts") or spec.get("hosts") or []
        click_texts = spec.get("login_click") or ()
        t0 = time.time()
        QR_WAIT = 90          # 等二维码出现的上限（秒）
        qr_tries = 0
        qr_done = False
        last_cred = ""
        while time.time() - t0 < timeout:
            if getattr(sess, "_stop", False) \
                    or sess.status in ("success", "error", "expired"):
                return
            tab = b._first_page_ws()
            # 二维码：前 ~56 秒每 4 秒试一次（有的站要点「扫码登录」才出码）
            if not qr_done and tab and qr_tries < 14:
                qr_tries += 1
                try:
                    rect = b.find_qr_rect(tab)
                    # ★ 2026-10-06 关键修正：**没码就继续点**，不能点一次就封死。
                    #   实测小浣熊(raccoon) 是两级弹窗：先点「同意并继续」
                    #   （用户协议）→ 登录面板才出现 → 还得再点「扫码登录」才出码。
                    #   旧逻辑用 click_done 一旦点中就永久停止，于是卡在第一级，
                    #   60 秒抓不到码被误判成「该平台无扫码入口」。
                    #   现在以「页面上有没有码」为准：没码 → 关弹窗 + 再点一次。
                    #   ★ 判定标准再放宽一层（raccoon 第二层坑）：登录面板打开后
                    #     默认显示的是「账号登录」tab，码容器（182x182）**存在但是空的**，
                    #     得再点「扫码登录」才真正渲染。所以不能用 `not rect` 当停止条件
                    #     —— 那会停在第一层。改成：**只要还没拿到真码就持续推进**。
                    #   ★ 也不能无脑重复点同一个词：raccoon 词表里「同意」在页脚
                    #     协议区也有匹配，反复点会把页面滚走。故按词序**轮转推进**：
                    #     第 n 轮从 texts[n-1] 起找，前几轮的前置按钮自然被跳过。
                    if click_texts and qr_tries % 2 == 1:
                        # 先清推广/公告弹窗（豆包「下载电脑版」实测挡死登录入口）
                        b.dismiss_popups(tab)
                        idx = min((qr_tries - 1) // 2, len(click_texts) - 1)
                        b.click_login(tab, texts=list(click_texts)[idx:])
                        time.sleep(2)
                        rect = b.find_qr_rect(tab)
                    # ★ 没有码元素、页面也还没出现「扫码」文案 → 弹窗未打开，
                    #   整页截图对用户毫无意义，直接等下一轮（以前正是这里
                    #   把首页截图当二维码发出去，用户只看到一片空白）。
                    if not rect and not b.page_has_qr_hint(tab):
                        time.sleep(4)
                        continue
                    clip = None
                    if rect:
                        pad = 14
                        clip = {"x": max(0, rect["x"] - pad),
                                "y": max(0, rect["y"] - pad),
                                "width": rect["width"] + pad * 2,
                                "height": rect["height"] + pad * 2,
                                "scale": 2}
                    data = b.screenshot(tab, clip=clip)
                    # 页面还没渲染出码时截到的是**纯空白**，不算成功，继续重试；
                    # 还要过一道「像不像二维码」的形态校验：loading 圈 / 方形
                    # 插画 / 公告图都会命中元素定位，必须挡住（用户扫不出来）。
                    if data and not _is_blank_png(data) and _looks_like_qr(data):
                        with sess.lock:
                            sess.qr = "data:image/png;base64," + data
                            qr_done = True
                except Exception:
                    pass
            # 检测登录完成：凭据（Cookie+localStorage）有**变化**才验活。
            # 未登录 Cookie 也会命中，验活失败只提示、保持 waiting（strict=False），
            # 用户扫码后凭据变化 → 下一轮验活通过 → 自动落库。
            cred = self._collect_browser_cred(sess, spec, b)
            if cred and cred != last_cred:
                last_cred = cred
                src = ("服务端无头浏览器（扫码自动捕捉）" if sess.opts.get("headless")
                       else "CDP 浏览器（扫码自动捕捉）")
                if self._verify_and_finish(sess, spec, cred, src, strict=False):
                    return
            if qr_done and sess.status == "waiting":
                with sess.lock:
                    if "二维码已抓到" not in (sess.message or ""):
                        sess.message = (sess.message or "") \
                            + "（二维码已抓到下方，用手机扫即可）"
            time.sleep(4)
            # ★ 等码超时：超时仍没抓到码 → 判定无扫码入口，收浏览器放端口
            if not qr_done and time.time() - t0 > QR_WAIT:
                self._stop_browser(sess, b)
                sess.finish("error",
                            "该平台的登录页没有抓到二维码（等码 %d 秒未出现）。"
                            "可能它只支持账号密码/短信登录 —— 请改用下方的"
                            "「手动粘贴 Cookie」把浏览器里的 Cookie 贴进来。"
                            % QR_WAIT, "no_qr")
                return
        if sess.status == "waiting" and not getattr(sess, "_stop", False):
            self._stop_browser(sess, b)
            with sess.lock:
                sess.message = ("看守超时（约 %d 秒未检测到登录），"
                                "已关闭服务端浏览器。可重新发起扫码登录" % timeout)

    @staticmethod
    def _stop_browser(sess, b):
        """收掉会话的浏览器（释放单端口），忽略任何异常。"""
        try:
            sess._stop = True
            if b:
                b.close()
        except Exception:
            pass

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
