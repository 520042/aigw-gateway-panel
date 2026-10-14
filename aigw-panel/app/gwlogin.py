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

# ★ copilot 家族：同一套腾讯授权（copilot.tencent.com），网关 EXE 不可达时
#   一律回退面板原生扫码（app/tlogin.py 链路）。apk-trae 以前漏在这个集合外，
#   导致「启动登录失败」（连不上已移除的 8317）。
# ★ 2026-10-13：apk-trae 从这里移除 —— 它已改为 file+browser_capture 三方式
#   （跳转 Trae 官网登录 / 读取本机 Trae 登录态 / 手动粘贴 x-ide-token），
#   不再走 copilot 原生扫码（授权页是 WorkBuddy 登录页，张冠李戴；
#   且拿到的 copilot accessToken 喂不饱 api.trae.cn 中继）。
_COPILOT_NATIVE_FALLBACK = {"wb-gateway", "wb-gateway-intl", "apk-go"}

PLATFORMS = {
    "wb-gateway": {
        "name": "WorkBuddy 账号 · 国内站（扫码 · 面板原生直连）",
        "paste_type": "token",
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
        "paste_type": "token",
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
        "method": "oauth_lobster",
        "hosts": ["lobsterai.youdao.com"],
        "login_url": "https://lobsterai.youdao.com/#/index?keyfrom=invitation",
        "gateway": False,
        "upstream": "lobsterai.youdao.com",
        # ★ 协议逆向自社区项目 lobsterai2api：对话 /api/proxy/v1/chat/completions
        #   （OpenAI 兼容透传，Bearer accessToken）；模型 /api/models/available；
        #   每日签到 +100 credits（由 lobsterai2api 自带调度器做，面板暂不代签）。
        "hint": "★ 客户端 0.05 倍率。点击「启动登录」后，在浏览器打开授权链接，"
                "完成手机/微信登录，面板会自动获取 token。"
                "对话 model 写 模型id@web-lobster 即走面板原生中继。",
        # 验活：上游地址是用户在设置里填的（无静态域名），用 @lobster 哨兵——
        # verify_cookie 会换成 设置→Lobster 上游地址；未配置时明确说「未校验」。
        "verify": {"url": "https://@lobster/api/user/profile-summary",
                   "type": "bearer",
                   "ok_keys": ["data", "uid", "email", "credits"]},
    },
    "web-glm": {
        "name": "智谱清言网页版（glm2api 逆向）",
        "method": "cookie",
        "hosts": ["chatglm.cn"],
        "login_url": "https://chatglm.cn",
        "login_click": ["微信扫码登录", "扫码登录"],
        "gateway": False,
        "upstream": "chatglm.cn",
        "placeholder": "粘贴 chatglm_refresh_token（登录 chatglm.cn 后 F12 → Application → Cookies）",
        # ★ build38 对齐 glm2api(yxc0915) 一手源码：
        #   上游 https://chatglm.cn/chatglm/backend-api/assistant/stream（SSE），
        #   已实现 X-Sign/X-Timestamp/X-Nonce 签名 + refresh_token→access_token
        #   自动换取（/user-api/user/refresh），不再是 build33 的「推断/必 403」。
        "hint": "支持微信扫码登录（点开始登录后用手机扫二维码即可）；"
                "或手动粘贴 chatglm_refresh_token（F12 → Cookies）。"
                "对话 model 写 模型id@web-glm"
                "（glm-4-flash / glm-4 / glm-4-plus / glm-4-air / glm-4-all / glm-zero-preview）。",
        # 扫码登录捕获的是完整Cookie，先离线验活（检查有没有refresh_token），
        # 保存前自动提取出 refresh_token 转成 bearer 凭据
        "verify": {"session_markers": ["chatglm_refresh_token"]},
        "extract_secret_from_cookie": "chatglm_refresh_token",
    },
    "web-trae": {
        "name": "Trae 直连（Trae2api-cn 逆向）",
        "method": "file",
        "hosts": ["trae-api-cn.mchost.guru", "trae.cn", "trae.com.cn"],
        "login_url": "https://www.trae.com.cn",
        # ★ 2026-10-12：跳转+Cookie 自动链（一键登录）——登录页有扫码就抓码；
        #   ⚠ 中继吃的是桌面端 x-ide-token，网页 Cookie 未必能过 verify，
        #   过不了就按提示粘 token（两方式并存）。
        "browser_capture": True,
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
        "paste_type": "token",
        "placeholder": "粘贴 userToken（登录 chat.deepseek.com 后 F12 → Application → LocalStorage）",
        # ★ build38 对齐 deepseek-free-api(qiaojinxia) 一手源码：
        #   已实现 chat_session 创建 + create_pow_challenge 挑战 + 随包 Node/WASM
        #   解 POW（x-ds-pow-response 头）+ p/v/o 分片响应解析。
        #   ⚠ POW 求解依赖系统 node（app/webpow 已随包），node 缺失会明确报错。
        "hint": "★ 免 API Key，用免费网页额度。登录 chat.deepseek.com 后 F12 拿 "
                "LocalStorage 的 userToken 粘这里。对话 model 写 deepseek-chat@web-deepseek "
                "或 deepseek-reasoner@web-deepseek（深度思考）。"
                "首次对话需系统装有 node（用于解 POW 校验）。",
        # ★ 2026-10-12 补「跳转登录」方式：登录页有扫码/手机号，登录后
        #   userToken 在 **localStorage**（JSON，真值在 .value 字段，JWT）。
        #   走 cookie 看守流程 + storage 提取（参照元宝的 storage_keys）。
        "browser_capture": True,
        "storage_keys": ["userToken"],
        "storage_json_field": "value",     # localStorage['userToken'] = {"value": "<jwt>"}
        # 登录页的扫码入口需要点开（与豆包/小浣熊同款看守点击逻辑）
        "login_click": ["扫码登录", "登录", "手机号登录"],
        "verify": {"type": "bearer", "ok_keys": ["data", "choices", "id"]},
    },
    "apk-trae": {
        "name": "Trae 国内版",
        "paste_type": "token",
        # ★ 2026-10-13（用户三次反馈「Trae 登录打开的是 WorkBuddy 页，太离谱」）：
        #   原 method=qrcode 走 copilot 原生扫码 —— 授权页是 WorkBuddy/CodeBuddy
        #   登录页（张冠李戴），且拿到的 copilot accessToken 喂不饱 api.trae.cn
        #   中继（中继吃的是 Trae IDE 的 x-ide-token，两套 token，见 native_relay
        #   RELAY_SPECS apk-trae）。
        #   现改为与 web-trae 同款的「三方式」：
        #     ① 一键读取本机 Trae 登录态（trae_local，桌面端已登录时最快，
        #        main.py api/tencent?action=trae_local，前端抽屉有按钮）
        #     ② 跳转 www.trae.com.cn 官方登录页（browser_capture），面板自动
        #        抓 Cookie/localStorage（ideToken/x-ide-token 在 storage_keys）
        #     ③ 手动粘贴 x-ide-token（desktop leveldb 或 OAuth refresh 换）
        "method": "file",
        "hosts": ["trae.com.cn", "www.trae.com.cn", "trae.cn", "api.trae.cn"],
        "login_url": "https://www.trae.com.cn",
        # Trae 网页登录页有扫码/手机号入口，点开后才出码（与豆包/小浣熊同款逻辑）
        "login_click": ["登录", "扫码登录", "立即登录"],
        "browser_capture": True,
        "storage_keys": ["ideToken", "x-ide-token", "iCubeAuthInfo", "authInfo"],
        "gateway": False,
        "upstream": "api.trae.cn",
        "hint": "Trae 国内版，三种方式任选：① 本机装着并登录过 Trae 桌面端 → "
                "点「读取本机 Trae 登录态」一键提取入库；② 点「↗ 接入并打开登录页」"
                "在 Trae 官网登录，面板自动抓取凭据；③ 手动粘贴桌面端 x-ide-token。"
                "对话 model 写 模型id@apk-trae。",
        "placeholder": "粘贴 Trae x-ide-token（桌面端 leveldb 或 OAuth refresh 换取）",
        # 验活改打 api.trae.cn 活着的 /trae 前缀端点（gwextra 2026-10-04 实测
        # verified），不再打 copilot /console/account —— 那是 CodeBuddy 的
        # accessToken 验活口，Trae ide-token 打过去只会误报「未校验」。
        # ⚠ 该端点无凭据时返回 200 + {"code":1001,"message":"...not able to
        #   authenticate you..."} —— verify_cookie 的「业务码非 0 判无效」
        #   天然能识别（实测 2026-10-13 探活）；ok_keys 用响应里真实存在的字段
        #   （checked_in/enable），别用泛泛的 "code"（那是业务码不是标记）。
        "verify": {"url": "https://api.trae.cn/trae/api/v2/ug/checkin_credits/status",
                   "type": "bearer",
                   "ok_keys": ["checked_in", "enable", "credits", "data"]},
    },
    # ★ 2026-10-08 整理：apk-codebuddy 已合并到 wb-gateway-intl（同一账号体系）
    "apk-doubao": {
        "name": "豆包 网页版",
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
        "name": "元宝 网页版",
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
        "paste_type": "token",
        # ★ 2026-10-12：与 CodeBuddy 共用同一套 copilot 授权 →
        #   改走原生扫码（跳转+扫码+token 三方式齐备），不再只有粘贴。
        "method": "qrcode", "edition": "cn",
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
        "name": "Loomy 讯飞（loomy.xunfei.cn）",
        # ★ 2026-10-12 终版：走**账号服务签名 API**（短信/密码登录，参照
        #   Patrick130306/loomy2api 的逆向协议）—— 不再需要浏览器！
        #   之前走 CDP 浏览器登录会被讯飞设备绑定风控拦下
        #   （device rebind requires explicit user action）。
        #   登录三方式：短信验证码 / 密码 / 粘贴 session（32 字符，14 天）。
        "method": "loomy",
        "hosts": ["loomy.xunfei.cn", "xunfei.cn", "xfinfr.com"],
        "login_url": "https://loomy.xunfei.cn/login",
        "no_remote_scan": True,
        "gateway": False,
        "upstream": "loomyad.xunfei.cn",
        "paste_type": "token",
        "placeholder": "粘贴 session（32 字符，登录后服务端下发）",
        "hint": "讯飞 Loomy。★ 支持三种登录（均无需浏览器）："
                "① 手机号+短信验证码 ② 手机号+密码 ③ 粘贴 session。"
                "登录后模型清单实时拉取（含倍率）。",
        "verify": {"url": "https://loomyad.xunfei.cn/api/v1/models",
                   "type": "bearer", "ok_keys": ["data", "models", "id"]},
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
    # ★ 2026-10-08 整理：apk-codebuddy-cn 已合并到 wb-gateway（同一账号体系）
    # ==================== 办公 AI / Agent 平台（2026-10-04 调研） ====================
    "apk-coze": {
        "browser_capture": True,
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
    # ★ 2026-10-07 新增（参照 dsh-our-free-model src/kilo.js）：
    #   第二条"点一下就白嫖"的免凭据车道，与 anon-zen 共用 method=anon 流程。
    "kilo": {
        "name": "Kilo Gateway（免凭据免费池）",
        "method": "anon",
        "hosts": ["api.kilo.ai", "kilo.ai"],
        "login_url": "https://kilo.ai",
        "gateway": False,
        "upstream": "api.kilo.ai",
        "placeholder": "无需填写（免凭据车道，点接入即用）",
        "hint": "★ 免登录、免 Key、免注册：Kilo 的免费池对匿名请求直接应答，"
                "点「接入」只是把它登记进路由。清单取上游 models 的 isFree 切片。"
                "⚠ 上游明确说明免费池的提示可能被记录用于改进其服务，"
                "请勿用它处理敏感内容。",
    },
    "web-qoder": {
        "browser_capture": True,
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
        "browser_capture": True,
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
    # ★ 2026-10-15 新增（对齐 dsh-our-free-model MiniMax Code 渠道）：
    #   MiniMax 网页版 OAuth 设备码登录（account.minimax.cn，client_id=mcode-public，
    #   PKCE S256）→ access_token 入库 → 签到(/minimax-cloud/api/v1/signin/claim)
    #   + 模型(/mavis/api/v1/models) + 对话。Rust 端 start/poll 已接全自动。
    "minimax": ("MiniMax Code（网页版·设备码）", "https://account.minimax.cn",
                "https://agent.minimax.cn/mavis/api/v1/models", "eyJ...",
                "点「接入」→ 打开验证页输入码完成授权，面板自动拿 token；"
                "支持每日签到领积分"),
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

# ============ 2026-10-09 新增：订阅反代平台（原生实现，不需要外部 exe）============
# 这些平台用 OAuth 登录（设备码/PKCE），不是填 API Key 的
_SUBSCRIPTION_PLATFORMS = {
    # pid: (名称, 登录方式, hint)
    "sub-kimi": ("Kimi Code 国内站", "oauth_device",
                 "Kimi Code 订阅登录。国内站 auth.kimi.com。"
                 "设备码登录后自动获取 token，网关直接反代。"),
    "sub-kimi-ai": ("Kimi Code 国际站", "oauth_device",
                 "Kimi Code 订阅登录。国际站 auth.kimi.ai。"
                 "设备码登录后自动获取 token，网关直接反代。"),
    "sub-codex": ("OpenAI Codex", "oauth_pkce",
                  "ChatGPT Plus/Pro 订阅登录。PKCE OAuth 流程，"
                  "登录后自动获取 access_token，网关直接反代。"),
    "sub-claude": ("Anthropic Claude Code", "oauth_pkce",
                   "Claude Pro/Max 订阅登录。PKCE OAuth 流程，"
                   "登录后自动获取 access_token，网关直接反代。"),
    "sub-grok": ("xAI Grok", "oauth_device",
                 "Grok Build 订阅登录。设备码 OAuth 流程，"
                 "登录后自动获取 access_token，网关直接反代。"),
    "sub-antigravity": ("Google Antigravity", "oauth_google",
                        "Google Gemini 订阅登录。Google OAuth 流程：点「接入」生成"
                        "授权链接 → 浏览器登录授权 → 地址栏跳 "
                        "localhost:51121/callback?code=…（页面打不开是正常的）→ "
                        "把 code= 后面那段粘贴到下方即完成登录，token 自动入库。"),
    "sub-devin": ("Devin", "oauth_pkce",
                  "Devin 订阅登录。PKCE OAuth 流程，"
                  "登录后自动获取 session_token，网关直接反代。"),
    "sub-meta": ("Meta Muse", "oauth_device",
                 "Meta AI 订阅登录。设备码 OAuth 流程，"
                 "登录后自动获取 access_token，网关直接反代。"),
}
for _pid, (_nm, _method, _hint) in _SUBSCRIPTION_PLATFORMS.items():
    PLATFORMS[_pid] = {
        "name": _nm, "method": _method, "type": "subscription",
        "gateway": False, "hosts": [], "login_url": "",
        "upstream": "",
        "hint": _hint,
        "verify": None,  # 暂时没有验活
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
            "no_remote_scan": bool(p.get("no_remote_scan")),
            "browser_capture": bool(p.get("browser_capture")),
            "paste_type": p.get("paste_type", ""),
            "placeholder": p.get("placeholder", ""),
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
                "callback_port": self.opts.get("lobster_callback_port", 0),
                "result": _mask(self.result), "created_at": self.created_at,
            }

    def finish(self, status, message="", error="", result=None):
        with self.lock:
            self.status = status
            self.message = message
            self.error = error
            if result is not None:
                self.result = result


def _norm_cookie_text(cookie):
    """规范化用户粘贴的 Cookie 文本。

    用户按「F12 → Network → Request Headers」复制时，常见三种脏数据：
      · 连 `cookie:` 前缀一起复制（最常见）
      · 多行粘贴（Request Headers 整块复制时带上换行）
      · 首尾带引号（从 JSON / 日志里抠出来的片段）
    原样送进验活必然 401，用户视角就是「已登录，无法识别」（2026-10-13 实拍）。
    """
    if not cookie:
        return ""
    s = str(cookie).strip()
    if s[:1] in ('"', "'") and s[-1:] == s[:1] and len(s) >= 2:
        s = s[1:-1]
    if s[:7].lower() == "cookie:":
        s = s[7:].lstrip()
    # 多行 → 单行（header 块复制时换行分隔的其它头会以 "x: y" 形态混入，
    # 只保留含 = 且不像 "头名: 值" 的段太复杂——直接压成空格后再按 ; 切，
    # 由 verify 侧决定哪些段可用）
    s = s.replace("\r", "\n")
    s = "; ".join(p.strip() for p in s.split("\n") if p.strip())
    return s.strip().strip(";").strip()


def _mask(res):
    """对外隐藏完整密钥，只留可辨识的指纹。

    ★ 2026-10-07 修（凭据明文回传）：原来只匹配
      secret / token / cookie / api_key / password 五个**精确**键名，
      而三条 OAuth 链路的结果用的是 access_token / refresh_token /
      accessToken / refreshToken —— 于是完整 JWT 原封不动地出现在
      /api/login?action=start|poll 的响应里。现在按关键字模糊匹配所有
      凭据类字段。"""
    if not isinstance(res, dict):
        return res
    out = dict(res)
    words = ("secret", "token", "cookie", "apikey", "api_key",
             "password", "passwd", "credential", "authorization")
    for k, v in list(out.items()):
        if not isinstance(v, str) or len(v) <= 12:
            continue
        kl = str(k).lower()
        if any(w in kl for w in words):
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



def _find_headless_chrome(auto_download=True):
    """
    找服务端无头 Chromium（2026-10-06 新增，远程部署兜底）。
    优先级：AIGW_HEADLESS_CHROME 环境变量 → 系统 Chrome/Edge →
    项目内置自动下载的 headless shell（data/browsers/）→
    Playwright 缓存 → 系统 PATH 里的 chromium / chrome。
    
    auto_download=True 时，找不到会自动下载（首次约 80MB）。
    找不到返回 ""（上层据此报错并引导手动粘贴 Cookie）。
    """
    exe = os.environ.get("AIGW_HEADLESS_CHROME", "")
    if exe and os.path.isfile(exe):
        return exe
    
    # 系统安装的 Chrome / Edge
    try:
        from . import cdp
        found = cdp.find_browser()
        if found:
            return found
    except Exception:
        pass
    
    # 项目内置自动下载的 headless shell
    try:
        from . import headless_dl
        local = headless_dl.find_local()
        if local:
            return local
        # 找不到就自动下载
        if auto_download:
            try:
                return headless_dl.download()
            except Exception as e:
                # 下载失败就继续往下找
                pass
    except Exception:
        pass
    
    home = os.path.expanduser("~")
    pats = (
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
        # Lobster 的常驻回调服务也要收掉，否则端口一直挂着
        if sess.method == "oauth_lobster":
            try:
                from . import oauth_lobster
                info = sess.opts.get("lobster_login_info") or {}
                oauth_lobster.stop_callback_server(info.get("port"))
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
            elif sess.method == "oauth_device":
                self._start_oauth_device(sess, spec)
            elif sess.method == "oauth_pkce":
                self._start_oauth_pkce(sess, spec)
            elif sess.method == "oauth_lobster":
                self._start_oauth_lobster(sess, spec)
            elif sess.method == "oauth_google":
                # ★ 2026-10-13：Antigravity（Google OAuth）—— 原先没有分派，
                #   点「接入」掉进 _start_file 只回「请粘贴」，授权根本发不起来
                #   （用户反馈「登录不行」的又一处）。
                self._start_oauth_google(sess, spec)
            elif sess.opts.get("force_browser") and spec.get("browser_capture"):
                # ★ 2026-10-12：file 类平台补「跳转登录」第二方式 ——
                #   平台声明 browser_capture=True（有 hosts/login_url/verify/
                #   storage_keys）时，前端可强制走 cookie 看守流程：开浏览器
                #   → 用户在登录页登录 → 抓 Cookie/localStorage → 提取凭据。
                #   （web-deepseek：userToken 在 localStorage，JSON 取 .value）
                sess.method = "cookie"
                self._start_cookie(sess, spec)
            else:
                self._start_file(sess, spec)
        except Exception as e:
            sess.finish("error", "启动登录失败", "%s: %s" % (type(e).__name__, e))

    # ---------------------------------------------------------- 设备码 OAuth
    def _start_oauth_device(self, sess, spec):
        """设备码 OAuth 登录（Kimi、Grok、Meta 等）"""
        from . import oauth_device
        
        # 确定平台 pid（处理国内/国际变体）
        pid = sess.platform
        if pid == "sub-kimi":
            # 国内/国际变体，由 opts 决定
            edition = sess.opts.get("edition", "cn")
            pid = "sub-kimi" if edition == "cn" else "sub-kimi-ai"
        
        try:
            info = oauth_device.start_device_flow(pid)
        except Exception as e:
            sess.finish("error", "启动设备码登录失败", str(e))
            return
        
        with sess.lock:
            sess.opts["device_info"] = info
            sess.opts["device_pid"] = pid
            # 验证码和验证 URL
            sess.qr_text = info.get("verification_uri_complete") or info.get("verification_uri")
            sess.auth_url = info.get("verification_uri_complete") or info.get("verification_uri")
            # 过期时间
            expires_in = info.get("expires_in", 600)
            sess.expires_at = time.time() + expires_in
            # user_code 显示在消息里
            user_code = info.get("user_code", "")
        
        sess.finish("waiting",
                    "请在浏览器打开授权页，输入验证码 %s 完成登录" % user_code,
                    result=None)
        
        # 开始后台轮询
        self._poll_oauth_device(sess)

    def _poll_oauth_device(self, sess):
        """设备码 OAuth 后台轮询"""
        from . import oauth_device
        
        # 从会话里取出设备码信息
        device_info = sess.opts.get("device_info")
        if not device_info:
            return
        
        interval = device_info.get("interval", 5)
        max_wait = 600  # 最多等 10 分钟
        waited = 0
        
        while waited < max_wait:
            if getattr(sess, "_stop", False):
                return
            
            time.sleep(interval)
            waited += interval
            
            try:
                result = oauth_device.poll_token(device_info)
            except Exception as e:
                sess.finish("error", "轮询登录状态失败", str(e))
                return
            
            if result is None:
                # 还在等待用户授权
                with sess.lock:
                    left = max_wait - waited
                    sess.message = "等待用户授权…（剩余 %d 秒）" % left
                continue
            
            if "access_token" in result:
                # 登录成功
                pid = sess.opts.get("device_pid", sess.platform)
                account = result.get("email") or result.get("username") or "已登录"
                
                sess.finish("success", "登录成功，凭据已保存",
                            result={
                                "type": "oauth_device",
                                "pid": pid,
                                "access_token": result.get("access_token"),
                                "refresh_token": result.get("refresh_token"),
                                "expires_in": result.get("expires_in"),
                                "account": account,
                            })
                self._save(sess)
                return
            else:
                # 登录失败
                err = result.get("error_description") or result.get("error") or "登录失败"
                sess.finish("error", err, "oauth_failed")
                return
        
        # 超时
        sess.finish("expired", "等待超时（10 分钟内未完成登录）", "expired")

    # ---------------------------------------------------------- PKCE OAuth
    def _start_oauth_pkce(self, sess, spec):
        """PKCE OAuth 登录（Codex、Claude、Devin 等）"""
        from . import oauth_pkce
        
        pid = sess.platform
        
        try:
            auth_url, state, pkce = oauth_pkce.generate_auth_url(pid)
        except Exception as e:
            sess.finish("error", "生成授权链接失败", str(e))
            return
        
        with sess.lock:
            sess.opts["pkce"] = pkce
            sess.opts["state"] = state
            sess.auth_url = auth_url
            sess.expires_at = time.time() + 600  # 10 分钟有效期
        
        sess.finish("waiting",
                    "请在浏览器打开授权链接，登录并授权后，把授权码（code）粘贴回来",
                    result=None)

    def submit_pkce_code(self, sid, code):
        """提交授权码，换 token"""
        from . import oauth_pkce
        
        sess = self.get(sid)
        if not sess:
            return False, "会话不存在"
        
        pkce = sess.opts.get("pkce")
        if not pkce:
            return False, "PKCE 信息不存在"
        
        try:
            result = oauth_pkce.exchange_code_for_token(sess.platform, code, pkce)
        except Exception as e:
            return False, "换 token 失败：%s" % str(e)
        
        if "access_token" in result:
            account = result.get("email") or result.get("username") or "已登录"
            sess.finish("success", "登录成功，凭据已保存",
                        result={
                            "type": "oauth_pkce",
                            "pid": sess.platform,
                            "access_token": result.get("access_token"),
                            "refresh_token": result.get("refresh_token"),
                            "expires_in": result.get("expires_in"),
                            "account": account,
                        })
            self._save(sess)
            return True, "登录成功"
        else:
            err = result.get("error_description") or result.get("error") or "登录失败"
            return False, err

    # ---------------------------------------------------------- Google OAuth
    def _start_oauth_google(self, sess, spec):
        """Google OAuth 登录（Antigravity / Cloud Code）。
        协议取自 app/relays/antigravity.py（CLIProxyAPI 翻译版）：
        generate_auth_url → 用户授权 → 地址栏跳 localhost:51121/callback?code=…
        → 用户复制 code 回来 → exchange_code_for_tokens 换 token。
        redirect_uri 是 localhost 固定端口（Google 对已安装应用的约定），
        授权成功后浏览器必然跳一个连不上的地址 —— 所以手动粘 code 是主路径，
        不是兜底（与 lobster 的常驻回调服务不同，这里不起本地服务）。"""
        from app.relays import antigravity as _AG
        try:
            auth_url, state = _AG.generate_auth_url()
        except Exception as e:
            sess.finish("error", "生成授权链接失败", str(e))
            return
        with sess.lock:
            sess.opts["state"] = state
            sess.auth_url = auth_url
            sess.expires_at = time.time() + 600  # 10 分钟有效期
        sess.finish("waiting",
                    "请在浏览器打开授权链接并用 Google 账号登录授权；授权后地址栏会跳到 "
                    "localhost:51121/callback?code=…（页面打不开是正常的），"
                    "把其中 code= 后面那一段粘贴到下方即可。",
                    result=None)

    def submit_google_code(self, sid, code):
        """提交 Google 授权码，换 token"""
        from app.relays import antigravity as _AG
        sess = self.get(sid)
        if not sess:
            return False, "会话不存在"
        try:
            result = _AG.exchange_code_for_tokens(code)
        except Exception as e:
            return False, "换 token 失败：%s" % str(e)
        if "access_token" in result:
            # Google 不直接回 email；有 id_token 就解一下 payload，否则用占位名
            account = "Google 账号"
            try:
                import base64 as _b64
                idt = result.get("id_token") or ""
                if idt.count(".") == 2:
                    payload = idt.split(".")[1]
                    payload += "=" * (-len(payload) % 4)
                    claims = json.loads(_b64.urlsafe_b64decode(payload).decode("utf-8", "replace"))
                    account = claims.get("email") or claims.get("name") or account
            except Exception:
                pass
            sess.finish("success", "登录成功，凭据已保存",
                        result={
                            "type": "oauth_google",
                            "pid": sess.platform,
                            "access_token": result.get("access_token"),
                            "refresh_token": result.get("refresh_token"),
                            "expires_in": result.get("expires_in"),
                            "account": account,
                        })
            self._save(sess)
            return True, "登录成功"
        err = result.get("error_description") or result.get("error") or "登录失败"
        return False, str(err)[:200]

    # ---------------------------------------------------------- Lobster AI OAuth
    def _start_oauth_lobster(self, sess, spec):
        """Lobster AI OAuth 登录（有道龙虾）"""
        from . import oauth_lobster

        # ★ 2026-10-07：把面板设置里的「Lobster 上游地址」传下去。
        #   原实现是空参调用 → 换 token 恒用硬编码域名，而同一平台的验活
        #   读的是 settings.lobster_server，两处会打到不同主机。
        settings = None
        try:
            if self.accounts is not None and getattr(self.accounts, "store", None):
                settings = self.accounts.store.get("settings")
        except Exception:
            settings = None

        # ★ 2026-10-12：起真正的本地回调服务（旧实现绑完端口就关，授权后
        #   浏览器跳回 127.0.0.1 必然 connection refused）。拿到 code 直接换
        #   token；前端也保留手动粘贴入口做兜底。
        def _on_code(code):
            try:
                ok, msg = self.submit_lobster_code(sess.id, code)
                if not ok:
                    sess.finish("error", msg, "lobster_failed")
            except Exception as e:
                sess.finish("error", "回调处理失败", str(e))
            finally:
                info = sess.opts.get("lobster_login_info") or {}
                oauth_lobster.stop_callback_server(info.get("port"))

        try:
            login_info = oauth_lobster.start_login(settings, on_code=_on_code)
        except Exception as e:
            sess.finish("error", "生成登录链接失败", str(e))
            return

        login_url = login_info["login_url"]

        with sess.lock:
            sess.opts["lobster_login_info"] = login_info
            sess.auth_url = login_url
            sess.expires_at = time.time() + 600  # 10 分钟有效期
            # 前端提示用：告诉用户面板已经在哪个端口等他回调
            sess.opts["lobster_callback_port"] = login_info.get("port")

        sess.finish("waiting",
                    "已在本地 127.0.0.1:%s 起回调服务。在浏览器打开登录链接，"
                    "完成手机/微信登录后会自动返回并完成授权；"
                    "若长时间没反应，请把地址栏里的 code 参数粘贴到下方。"
                    % login_info.get("port"),
                    result=None)
    
    def submit_lobster_code(self, sid, code):
        """提交 Lobster AI 授权码，换 token"""
        from . import oauth_lobster

        sess = self.get(sid)
        if not sess:
            return False, "会话不存在"

        login_info = sess.opts.get("lobster_login_info")
        if not login_info:
            return False, "Lobster 登录信息不存在"

        # 无论成败都收掉常驻回调服务，别把端口留在监听状态
        try:
            oauth_lobster.stop_callback_server(login_info.get("port"))
        except Exception:
            pass

        try:
            result = oauth_lobster.exchange_code(code, login_info)
        except Exception as e:
            return False, "换 token 失败：%s" % str(e)
        
        if "accessToken" in result:
            user = result.get("user", {})
            account = user.get("nickname") or user.get("userId") or "已登录"
            sess.finish("success", "登录成功，凭据已保存",
                        result={
                            "type": "oauth_lobster",
                            "pid": sess.platform,
                            "access_token": result.get("accessToken"),
                            "refresh_token": result.get("refreshToken"),
                            "expires_in": result.get("expiresIn"),
                            "account": account,
                        })
            self._save(sess)
            return True, "登录成功"
        else:
            return False, "换 token 失败：%s" % result.get("msg", "unknown")

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
            # ★ 网关 EXE 已彻底移除（build26 起纯原生模式）：任何 copilot 家族
            #   平台（wb-gateway / wb-gateway-intl / apk-trae）都改走面板原生
            #   扫码（tlogin 链路），不再要求先启动 EXE。
            #   ⚠ 旧代码只判 spec.get("gateway")，而 apk-trae 标的是 False ——
            #     于是它连不上 8317 就直接报「启动登录失败」，用户根本登不进去
            #     （2026-10-12 用户实测反馈）。
            if spec.get("gateway") or sess.platform in _COPILOT_NATIVE_FALLBACK:
                # ★ 2026-10-13：message 改人话 —— 原句「网关 EXE 未运行
                #   （unreachable…由于目标计算机积极拒绝）」把没启动外部组件的
                #   正常状态说成故障，用户被吓到（截图反馈「登录不行」）。
                #   EXE 早已彻底移除（build26 起纯原生模式），这就是默认路径。
                with sess.lock:
                    sess.message = "已自动切换为面板原生扫码（无需任何外部组件）"
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
        # ★★★ 2026-10-13（用户明确要求：「不要再单独开一个浏览器窗口了，
        #   只允许打开一个新建标签」）：原先这里起一个 minimized 的独立
        #   Chrome 当「令牌通道」（token 只能由 tlogin.exchange_in_page 在
        #   面板控制的页面上下文里 POST /console/login/enterprise 拿到，
        #   纯 HTTP 打该端点会被 APISIX 拦）。
        #   现在改为：只在你自己的浏览器里打开授权页标签，不拉任何面板
        #   控制的窗口。代价是没有页面上下文 → 拿不到自动 token，登录引导
        #   改为手动粘贴 accessToken（F12 → Network → /console/login/
        #   enterprise 响应里的 accessToken；抽屉「手动粘贴」已支持）。
        #   ⚠ 因此不再进 _poll_native 空转 300 秒（没有通道，轮询必然超时）。
        import webbrowser
        try:
            webbrowser.open(url)
        except Exception as e:
            sess.opts["native_browser_error"] = "%s: %s" % (type(e).__name__, e)
        sess.finish("waiting",
                    "已在你自己的浏览器里打开授权页（新标签页，没有额外窗口）。"
                    "请在标签页里完成登录；登录成功后，按 F12 → Network 找到 "
                    "/console/login/enterprise 请求，把响应里的 accessToken "
                    "粘贴到下方「手动粘贴 accessToken」即可入库。",
                    result=None)

    def _poll_native(self, sess, spec):
        """原生扫码的收尾线程：纯 HTTP 轮询 token 端点 → 落账号池。"""
        from app import tlogin
        state = sess.opts.get("native_state")
        if not state:
            sess.finish("error", "轮询登录状态失败", "no_state")
            return
        try:
            # ★ 2026-10-07：把会话端口传下去 —— poll_token(port, state, …)
            #   的 port 原来被写死成 CDP 默认端口，会话自己的端口配了也白配。
            port = int(sess.opts.get("port") or CDP_PORT)
            d = tlogin.poll_token(port=port, state=state, timeout=300)
        except Exception as e:
            if getattr(sess, "_stop", False):
                return
            sess.finish("error", "原生登录交换失败",
                        "%s: %s" % (type(e).__name__, e))
            return
        if getattr(sess, "_stop", False):
            return
        try:
            if d.get("accessToken"):
                sess.finish("success", "登录成功（面板原生直连，token 已入账号池）",
                            result={"type": "token",
                                    "secret": d["accessToken"],
                                    "account": d.get("userId") or "原生登录",
                                    "uid": d.get("userId") or "",
                                    # ★ 2026-10-12（对齐 L0NE-6/WorkBuddy-Daily）：
                                    #   把 refreshToken 一起落库 —— 账号池保活线程
                                    #   靠它做 RT 轮换续期，没有它「永续」无从谈起。
                                    "refresh_token": d.get("refreshToken") or "",
                                    "edition": spec.get("edition", ""),
                                    "source": "原生登录"})
                self._save(sess)
            else:
                sess.finish("expired", "等待超时（5 分钟内未完成登录）", "expired")
        finally:
            # 收尾：关掉为本次登录开的调试浏览器，别把窗口留在用户桌面上
            br = sess.opts.get("native_browser")
            if br is not None:
                try:
                    br.close()
                except Exception:
                    pass

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
        # ★ 2026-10-13（E2E 实测：从点「接入」到浏览器起来要 10~20 秒——
        #   附加用户浏览器失败重试 + 单端口回收等待 + Chrome 冷启动）：
        #   入口只设 message、**保持 pending**（不能 finish("waiting")——poll 的
        #   cookie 分支一见 waiting 就打 _poll_cookie，而 _browser 还没设上，
        #   会误判「浏览器会话已丢失」把会话打死，2026-10-13 实测踩中）。
        #   pending 态 UI 同样显示 message（viewLoginSession 渲染 lsMsg），
        #   用户看得到「正在打开登录页，请稍候…」，不再面对空白 pending。
        with sess.lock:
            sess.message = "正在打开登录页，请稍候…"
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
        # ★ 2026-10-12：storage_json_field 平台（deepseek userToken 等）**跳过直读**
        #   —— 凭据在 localStorage，磁盘 Cookie 直读只能拿到匿名/设备 Cookie，
        #   而 verify 没有在线端点时会「未校验」放行 → 没登录却显示登录成功
        #   （2026-10-12 用户实测踩中）。这类平台必须等到 storage token 出现。
        if not spec.get("storage_json_field"):
            try:
                from . import browser_cookie
                got = browser_cookie.fetch(hosts)
                if got.get("ok") and got.get("cookie"):
                    self._finish_cookie(sess, spec, got["cookie"],
                                        "本机浏览器（%s）" % got.get("source", ""))
                    return
            except Exception as e:
                sess.message = "直读浏览器失败：%s" % e
        # ★★★ 2026-10-13（用户明确要求：「不要再单独开一个浏览器窗口了，
        #   只允许打开一个新建标签」）：
        #   旧链路有两处「单独开窗口」—— default_profile 附加本质是拿用户
        #   profile 起一个新 Chrome 进程（用户浏览器在跑就移交失败），失败后
        #   又退隔离 profile 独立窗口，用户看到的就是凭空多一个浏览器窗口。
        #   现在非 headless 模式**只做一件事**：用系统默认方式在用户自己的
        #   浏览器里打开登录页（已在跑 → 新标签页；没在跑 → 启动他自己的
        #   浏览器，都不是面板拉的窗口）。凭据回收改两条路：
        #     ① 用户登录完点「我已登录，立即获取」→ _poll_cookie 直读本机
        #        浏览器 Cookie（纯本地 SQLite，不需要调试端口；Chrome127+
        #        app-bound 解不开时会明确提示）；
        #     ② 手动粘贴（抽屉里已有粘贴框）。
        import webbrowser
        login_url = spec.get("login_url") or ("https://" + (hosts[0] if hosts else ""))
        if not login_url or login_url == "https://":
            sess.finish("error", "该平台没有配置登录页地址", "no_login_url")
            return
        with sess.lock:
            sess.message = "正在你的浏览器里打开登录页…"
        try:
            webbrowser.open(login_url)
        except Exception as e:
            sess.finish("error", "打开登录页失败：%s" % e, "open_failed")
            return
        sess.finish("waiting",
                    "已在你自己的浏览器里打开登录页（新标签页，没有额外窗口）。"
                    "请在标签页里完成登录，然后点「我已登录，立即获取」，"
                    "或把登录后的 Cookie 粘贴到下方。",
                    result=None)

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
            #   ① 先回收「活动会话」的浏览器；
            #   ② 就算没有活动会话，也可能上一个 Chrome 刚被 cancel、进程还在
            #     退出中（close() 返回 ≠ 端口立即释放，实测要 1~3s）——
            #     等端口真正空出来再重试一次，而不是直接报错踢给用户。
            #     （2026-10-12 实测：cancel 完 2s 内起无头必报
            #       「调试端口 9333 已被其它登录会话占用」。）
            self._recycle_port(sess, b)
            for _ in range(20):
                try:
                    if not b.is_up():
                        break
                except Exception:
                    break
                time.sleep(0.5)
            try:
                b.start(url=spec.get("login_url")
                        or ("https://" + ((sess.opts.get("hosts")
                                           or spec.get("hosts") or [""])[0])))
            except Exception as e2:
                sess.message = "无头浏览器启动失败：%s" % e2
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
            # ★ 2026-10-13「只开标签」模式：非 headless 会话没有 CDP 浏览器，
            #   「我已登录，立即获取」改走**直读本机浏览器 Cookie**
            #   （browser_cookie.fetch 纯本地 SQLite，不需要调试端口）。
            #   解不开（Chrome 127+ app-bound 加密）就提示手动粘贴，
            #   并把「未检测到」说清楚，别误报成会话丢失。
            if spec.get("storage_json_field"):
                # 凭据在 localStorage（deepseek userToken 等），磁盘直读拿不到，
                # 但 storage 提取只在 CDP 看守链路里做——这里只能引导手动粘贴。
                with sess.lock:
                    sess.message = ("该平台的凭据在浏览器 localStorage 里，自动读取不到。"
                                    "请按 F12 → Application → Local Storage 找到后粘贴到下方。")
                return
            try:
                from . import browser_cookie
                got = browser_cookie.fetch(hosts)
                if got.get("ok") and got.get("cookie"):
                    self._finish_cookie(sess, spec, got["cookie"],
                                        "本机浏览器（%s）" % got.get("source", ""))
                    return
                _why = got.get("error") or "未找到登录 Cookie"
            except Exception as e:
                _why = "读取失败：%s" % e
            with sess.lock:
                sess.message = ("还没自动检测到登录态（%s）。本机 Chrome/Edge 127+ "
                                "的 Cookie 是 app-bound 加密，面板读不出来是正常的——"
                                "请在浏览器标签页里确认已登录，然后 F12 → Network → "
                                "任选一个请求 → Request Headers → 把 cookie: 整行"
                                "复制到下方粘贴框，点「提交」。" % str(_why)[:60])
            return
        # ★ 2026-10-12（用户实测「没登录却显示登录成功」的真凶）：
        #   这条轮询路径原来**自己拼凭据串**（Cookie + __ls_ 原样合并），
        #   没走 _collect_browser_cred —— storage_json_field 提取和
        #   「没提到 token 返回空串继续等」的守卫全被绕过，匿名 Cookie +
        #   「未校验」直接假成功。现在统一走同一个出口，两路判定一致。
        cred = self._collect_browser_cred(sess, spec, b)
        if not cred:
            with sess.lock:
                sess.message = ("未检测到 %s 的登录态（Cookie/localStorage），"
                                "请先在浏览器里登录" % "/".join(hosts))
            return
        # 浏览器会话来源：验活失败只提示、不打死会话（登录页本来就有
        # 未登录 Cookie，401 ≠ 登录失败）；手动粘贴路径才 strict。
        self._verify_and_finish(sess, spec, cred,
                                "服务端无头浏览器" if sess.opts.get("headless") else "CDP 浏览器",
                                strict=False)

    def _collect_browser_cred(self, sess, spec, b):
        """
        组装浏览器会话的完整凭据串：Cookie + storage_keys 合并。
        _poll_cookie 与 _watch_cookie 共用，保证两边判断口径一致
        （2026-10-06 修正：看守线程以前只查 Cookie，漏掉元宝这类
        登录态在 localStorage 的平台，永远验不过）。
        ★ 2026-10-12：storage_json_field —— localStorage 值是 JSON、真值在
        某个字段里（deepseek：userToken={"value":"<jwt>"}）。命中时直接返回
        该字段值（Bearer 可用），并把类型标记写进 sess.opts（落库 type=token）。
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
        # storage JSON 提取（deepseek userToken 等）
        tok = self._extract_storage_token(spec, header)
        if tok:
            with sess.lock:
                sess.opts["_force_type"] = "token"
            return tok
        # ★ 2026-10-12：声明了 storage_json_field 的平台，凭据**只能**来自
        # localStorage —— Cookie 里那点匿名/设备标记不算登录态。没提到 token
        # 就返回空串，让看守线程继续等（否则「未校验」会放行假成功）。
        if spec.get("storage_json_field"):
            return ""
        return header or ""

    def _extract_storage_token(self, spec, cred):
        """spec.storage_json_field：遍历凭据串里的 __ls_* 段，值为 JSON 时取
        指定字段（如 value）。命中返回字段值，否则 None。"""
        field = spec.get("storage_json_field")
        if not field or "__ls_" not in (cred or ""):
            return None
        import urllib.parse as _up
        for seg in str(cred).split(";"):
            seg = seg.strip()
            if not seg.startswith("__ls_") or "=" not in seg:
                continue
            _k, v = seg.split("=", 1)
            try:
                obj = json.loads(_up.unquote(v))
            except Exception:
                continue
            val = obj.get(field) if isinstance(obj, dict) else None
            if isinstance(val, str) and val.strip():
                return val.strip()
        return None

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
        # ★ 2026-10-13：用户从 F12 → Network → Request Headers 复制时，常把
        #   `cookie:` 前缀、换行、首尾引号一起带进来。原样送验活必然失败，
        #   用户视角就是「已登录，无法识别」。这里先规范化再验。
        cookie = _norm_cookie_text(cookie)
        okk, info = verify_cookie(spec, cookie)
        if not okk:
            if strict:
                # ★ 2026-10-13：别只甩一句「未通过校验」—— 用户不知道下一步。
                #   手动粘贴失败的三大常见原因写全，让人能自己排查。
                sess.finish("error",
                            "提交的凭据未通过登录校验（%s）。请确认："
                            "① 是在该平台**已登录**的浏览器里复制的；"
                            "② 复制的是完整 cookie（F12 → Network → 任选一个请求 "
                            "→ Request Headers → cookie: 整行）；"
                            "③ 登录态没有过期（换个页面刷新后再复制一次）。"
                            % str(info)[:80], "verify_failed")
                return False
            with sess.lock:
                if cookie:
                    sess.message = ("自动读取到的 Cookie 未通过登录校验（%s）——"
                                    "它可能是旧的/匿名登录态。请在下方粘贴框里贴入"
                                    "浏览器里登录后的 Cookie。" % str(info)[:60])
                else:
                    sess.message = ("已检测到凭据但登录校验未通过（%s），"
                                    "请继续完成扫码登录" % info)
            return False
        # ★ 从 Cookie 中提取指定键作为最终 secret（如 web-glm 需要从完整Cookie
        #   里抽出 chatglm_refresh_token 单独存，后端 relay 用它换 access_token）
        extract_key = spec.get("extract_secret_from_cookie")
        final_secret = cookie
        final_type = "cookie"
        # ★ 2026-10-12：storage_json_field 提取路径（deepseek userToken 等）
        #   由 _collect_browser_cred 预先标记 —— 凭据本身已是 Bearer 可用的
        #   token，落库 type=token 而不是 cookie。
        ft = sess.opts.pop("_force_type", None)
        if ft:
            final_type = ft
        account = info
        if extract_key and cookie:
            import re as _re
            m = _re.search(r"(?:^|;\s*)" + _re.escape(extract_key) + r"=([^;]+)", cookie)
            if m:
                final_secret = m.group(1)
                final_type = "bearer"
                account = (account + "（已提取 %s）" % extract_key) if account else extract_key
        sess.finish("success", "登录成功（来源：%s）" % (source or "未知"),
                    result={"type": final_type, "secret": final_secret, "account": account,
                            "source": source})
        self._save(sess)
        return True

    def _finish_cookie(self, sess, spec, cookie, source=""):
        # 防止「看守线程自动捕捉」与「前端手动点已登录」重复落库。
        # ★ 2026-10-13：只有**手动粘贴**才 strict（失败即报错踢回）——
        #   自动读取（browser_cookie 直读 / 看守线程捕捉）拿到过期或匿名
        #   Cookie 时不能杀会话：用户还得靠这个会话里的粘贴框补救，
        #   杀掉会话＝粘贴框一起消失（用户实拍「无 cookie 填入框」）。
        manual = ("手动" in (source or "")) or ("粘贴" in (source or ""))
        self._verify_and_finish(sess, spec, cookie, source, strict=manual)

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
        last_page_msg = ""
        while time.time() - t0 < timeout:
            if getattr(sess, "_stop", False) \
                    or sess.status in ("success", "error", "expired"):
                return
            tab = b._first_page_ws()
            # ★ 2026-10-12 页面级报错感知（Loomy 讯飞实测）：登录页自身被
            #   后端风控拦下（如「device rebind requires explicit user
            #   action」——CDP 调试浏览器是全新设备，讯飞要求设备重绑必须
            #   用户显式确认）时，把页面上的报错透传成可操作指引，
            #   别让用户对着一个干等的会话猜。
            if tab:
                try:
                    from . import cdp as _cdp
                    _s = _cdp.Session(tab["webSocketDebuggerUrl"])
                    _r = _s.call("Runtime.evaluate", {"expression":
                        '(document.body?document.body.innerText:"").slice(0,1200)',
                        "returnByValue": True}, timeout=8)
                    _s.close()
                    _txt = ((_r.get("result") or {}).get("value")) or ""
                    _msg = _page_blocker(_txt)
                    if _msg and _msg != last_page_msg:
                        last_page_msg = _msg
                        with sess.lock:
                            sess.message = "登录页提示：" + _msg
                except Exception:
                    pass
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
        
        # PKCE 类型：提交授权码换 token
        if sess.method == "oauth_pkce":
            code = kv.get("code") or kv.get("secret") or ""
            if not code:
                sess.finish("error", "授权码不能为空", "empty")
                return sess
            ok, msg = self.submit_pkce_code(sid, code)
            if not ok:
                sess.finish("error", msg, "pkce_failed")
            return sess
        
        # ★ 2026-10-07：Lobster（有道）此前没有分支 —— 用户提交的授权码会落进
        #   下面的 file/api_key 分支，被当成 api_key 明文入库（或报"内容不能为空"）。
        if sess.method == "oauth_lobster":
            code = kv.get("code") or kv.get("secret") or ""
            if not code:
                sess.finish("error", "授权码不能为空", "empty")
                return sess
            ok, msg = self.submit_lobster_code(sid, code)
            if not ok:
                sess.finish("error", msg, "lobster_failed")
            return sess

        # ★ 2026-10-13：Google OAuth（Antigravity）授权码提交
        if sess.method == "oauth_google":
            code = kv.get("code") or kv.get("secret") or ""
            if not code:
                sess.finish("error", "授权码不能为空", "empty")
                return sess
            ok, msg = self.submit_google_code(sid, code)
            if not ok:
                sess.finish("error", msg, "google_failed")
            return sess

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
            # 设备码 / PKCE / Lobster 登录：用 pid 作为 platform，
            # 用 access_token 作为 secret
            platform = sess.platform
            secret = sess.result.get("secret", "")
            # ★ 刷新令牌键名统一：OAuth 结果里是蛇形 refresh_token，
            #   而这里原来读驼峰 refreshToken —— 除设备码分支外全部落成 ""，
            #   注释宣称的"到期前自动刷新"对 PKCE/Lobster/扫码都失效。
            refresh_token = (sess.result.get("refresh_token")
                             or sess.result.get("refreshToken") or "")
            rtype = sess.result.get("type")
            if rtype in ("oauth_device", "oauth_pkce", "oauth_lobster",
                         "oauth_google"):
                # ★ 原来只有 oauth_device 做了 access_token→secret 映射，
                #   PKCE / Lobster 因此 secret 恒为 "" —— 账号池里出现空账号，
                #   而且 add() 总是带 secret 键，会把已有有效密钥覆盖成空。
                platform = sess.result.get("pid", platform)
                secret = (sess.result.get("access_token")
                          or sess.result.get("accessToken") or secret)
            # ★ 网关扫码成功时 secret 本来就是空的 —— 凭据在网关进程侧
            #   （type "gateway"），账号池里只记一条占位行。空守卫只拦
            #   "凭据应该在面板侧"的类型，别把网关登录也挡掉。
            if not secret and rtype != "gateway":
                sess.result["save_error"] = "凭据为空，未写入账号池"
                return
            
            self.accounts.add({
                "platform": platform,
                "name": sess.result.get("account") or PLATFORMS.get(
                    platform, {}).get("name", platform),
                "type": sess.result.get("type", ""),
                "secret": secret,
                "source": sess.result.get("source", "面板登录"),
                "edition": sess.result.get("edition", ""),
                # ★ 保存 refreshToken：账号池保活线程到期前自动刷新（社区标配）
                "refresh_token": refresh_token,
                # copilot 风控头（X-User-Id）与 token 必须同账号，登录响应里就有
                "uid": sess.result.get("uid", ""),
            })
        except Exception as e:
            try:
                sess.result["save_error"] = "%s: %s" % (type(e).__name__, e)
            except Exception:
                pass


# ------------------------------------------------------------------ 校验

def _page_blocker(text):
    """登录页自身的拦截性报错 → 可操作指引（透传给用户）。"""
    t = (text or "").lower()
    if "rebind" in t or "设备重绑" in text or "重新绑定设备" in text:
        return ("讯飞检测到新设备，要求「设备重绑」且必须由你显式确认 —— "
                "请在 Loomy 官方客户端（手机 App）或网页里完成一次设备确认，"
                "然后回到本浏览器重新登录（登录态会持久保存，下次不用重登）")
    if "验证设备" in text or "新设备登录" in text or "设备验证" in text:
        return "讯飞检测到新设备登录，请按页面/手机上的提示完成设备验证"
    return None


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
    # ★ 2026-10-07：恢复证书校验（这里发的是用户的 Cookie/API Key/JWT），
    #   并改用 ProxyHandler({}) 直连，不再全局删除进程的 *_PROXY。
    ctx = ssl.create_default_context()
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        urllib.request.HTTPSHandler(context=ctx))
    try:
        with opener.open(req, timeout=12) as r:
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
        # 网络层失败不是"凭据有效"的证据，但也不是"凭据无效"的证据：
        # 保持可用（宁可放过），但措辞必须让用户知道这是**未校验**。
        return True, "未校验（校验请求未完成：%s）—— 以实际对话为准" % type(e).__name__

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
        # ★ 200 + 非 JSON 不能当作"已登录"：很多网关用 200 + 业务错误体
        #   表达鉴权失败。这里保持可用（不误杀），但必须标成未校验。
        return True, "未校验（接口返回非 JSON，无法判定登录态）"
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
