# 办公型 AI 助手调研报告

> 调研日期：2026-10-04
> 调研范围：国内办公 AI 16 家 + 国际办公 AI 12 类 + GitHub 反代/集成项目 30+
> 方法：WebSearch / WebFetch 实际查证 + curl 实测关键端点 + GitHub Search API 取元数据
> 标注约定：`✅实测` = 本机 curl 打过并拿到返回码/字段；`📄官方文档` = 有公开文档页面；`🔒私有接口` = 网页版内部接口，无文档，随时会挂

---

## 1. 结论先行：最值得接入的 10 个

按「签到确定性 × 接口稳定性 × 办公场景契合度」排序。

| # | 平台 | 一句话理由 | 有没有签到 | 签到确定性 | 接口类型 |
|---|---|---|---|---|---|
| 1 | **千问办公 QwenWork**（阿里） | 阿里官方文档写明「每日登录奖励 100/200/300 积分」，唯一有**官方文档背书的每日签到**办公 AI | ✅ 有 | 🟢 **高**（阿里云官方文档明文） | 📄 官方文档（未见公开 OpenAI 兼容端点） |
| 2 | **库库 AI**（百度文库+网盘） | 官方活动页明写「每日登录+50 / 完成一次对话+50 / 邀请+50~300」，且已实测域名存活 | ✅ 有 | 🟡 **中**（活动期制，第 4 期 10/10 结束） | 🔒 私有接口（PC 客户端 + Skill） |
| 3 | **扣子 Coze**（字节） | 官方文档明写「每日登录 1500 活动积分」+「新用户 1500 / 邀请 6000」，且**官方 Open API 有完整文档可实测** | ✅ 有 | 🟢 **高**（官方文档明文） | 📄 **官方 Open API** ✅实测 |
| 4 | **腾讯元宝** | 福利中心签到 + AI 提问 3 次等任务，单日可攒近万积分，可换腾讯视频 VIP | ✅ 有 | 🟡 中（入口在 App「我的→福利中心」） | 🔒 私有接口（有开源反代可参考） |
| 5 | **WPS AI / 灵犀智点**（金山办公） | 官方社区帖明确「每天签到领 200 智点」，未订阅用户每月另有 800 智点 | ✅ 有 | 🟡 中（智点/积分双轨，规则较杂） | 🔒 私有接口 |
| 6 | **纳米 AI**（360） | 官方签到页 + 邀请码机制，有官方 **Open API**（developer.n.cn）+ 明确 Token 折算 | ✅ 有 | 🟡 中 | 📄 官方 API（付费，100 Token/次） |
| 7 | **秘塔 AI**（metaso.cn） | **唯一有官方开放 API 且支持 MCP** 的国内办公 AI，搜索/PPT/深度研究 | ❌ 无签到（每日刷新积分自动到账） | 🟢 高（官方用户协议明文） | 📄 **官方 Open API + MCP** |
| 8 | **智谱清言**（GLM） | 官方积分规则页明写「每日登录赠送积分」；GLM-4-Flash 官方永久免费不限量 | ✅ 有（每日登录赠送） | 🟢 **高**（官方协议明文） | 📄 **官方 Open API** ✅实测 |
| 9 | **飞书妙记** | **唯一有官方会议纪要 API**（minutes/v1），可上传音视频转写、下载音频、错误码齐全 | ❌ 无签到（按月额度） | — | 📄 **官方 Open API** ✅实测 |
| 10 | **豆包办公**（字节） | 桌面版「办公任务模式」+ 与豆包主站通用积分，「我的→活动中心」手动签到，连续签到奖励递增 | ✅ 有 | 🟡 中 | 🔒 私有接口（doubao2api 可反代主站） |

### 关于「签到」的重要区分

用户要求区分「有积分」和「有签到」，这是对的。实测下来分三类：

- **A 类 · 登录即送（无需点按钮）**：千问办公、扣子 Coze、智谱清言、库库 AI 的「每日登录+50」
  → 自动化最简单，一次带 Cookie 的 GET/POST 即可，适合直接做进面板的定时任务。
- **B 类 · 必须手动点签到**：腾讯元宝、WPS AI、豆包办公、纳米 AI
  → 需要找到签到端点，多数在 App 内，网页版不一定复现。
- **C 类 · 每日自动刷新额度，无签到概念**：秘塔 AI、飞书妙记、Kimi
  → 不要在面板里给它们做「签到」按钮，会误导。

---

## 2. 国内办公 AI 完整表格

### 2.1 概览表

| 平台 | 公司 | 官网 | 免费额度机制 | 有积分 | **有签到** | 签到规则（已查证部分） | 鉴权方式 | 接口类型 |
|---|---|---|---|---|---|---|---|---|
| **千问办公 QwenWork** | 阿里 | `qwenwork.cn`<br>`help.aliyun.com/zh/qwenwork` | 免费版 0 元；新用户 2000 积分（3个月）；**每日登录赠 100** | ✅ | ✅ **有** | 每日登录奖励：免费版 100 / 标准版 200 / 高级版 300 积分。标注「限时」。积分消耗遵循「最快过期优先」，当日赠送优先扣减 | 桌面端 / 钉钉唤起免登录 / 网页端登录 | 📄 官方文档（未见公开 OpenAI 兼容 API） |
| **库库 AI**（原 GenFlow） | 百度（文库+网盘） | `kuku.baidu.com` | 新用户 2000 积分；**每日登录+50**、完成对话+50、邀请网页版+50（上限150/日）、邀请桌面版+100（多得300/日） | ✅ | ✅ **有** | 官方活动页原文：「每日登录+50 登录领取 / 完成一次对话+50 / 邀请新用户登录网页版+50 每账号每日最多送150 / 邀请新用户登录电脑版+100 每账号每日再多得300」。另有连续签到 3/5/7 天各再给 3/5/7 天百度网盘 SVIP，最多 15 天 | 百度账号（PC 客户端扫码） | 🔒 私有接口（客户端 + Skill 云端签到） |
| **扣子 Coze** | 字节 | `coze.cn`<br>`space.coze.cn/open/docs` | 个人免费版 0 元；**每日登录 1500 活动积分**；新用户一次性 1500（30天）；邀请新用户 6000（30天） | ✅ | ✅ **有** | 官方规则原文：「用户每日完成登录即可获得当日的登录奖励，每个自然日每个账号仅限领取一次」「每日成功登录可获得 1500 活动积分」「积分通常在实时或 T+1 发放」「有效期为 1 天，从发放之时起算，每日获得的积分有效期独立计算，可累计使用」 | `Bearer pat_xxx`（个人访问令牌） | 📄 **官方 Open API** ✅实测 |
| **腾讯元宝** | 腾讯 | `yuanbao.tencent.com` | 基础功能免费；混元 Lite 永久免费；新用户 100 万 Token 包（1年） | ✅ | ✅ **有** | 入口：App「我的→福利中心」。每日签到 + AI 提问 3 次 + AI 写作 + AI 绘图，**单日最高攒近万积分**。兑换池：QQ 会员/腾讯视频/酷狗/王者皮肤。另有「连续 6 天每天 1 天腾讯视频 VIP，第 7 天月卡（最高 31 天）」 | 微信/QQ 登录 Cookie | 🔒 私有接口 |
| **WPS AI / 灵犀智点** | 金山办公 | `vip.wps.cn`<br>`forum.wps.cn` | 未订阅：体验版每月 800 智点；任务中心几百点起；大会员任务中心可领 2000 智点 | ✅ | ✅ **有** | 官方社区原文：「每天签到可以领 200 个点」。签到入口 PC：个人中心→天天签到；手机：我的→签到。另有「周一到周五签到得积分，周末签到直接赠 1 天超级会员」「90 积分=1 天大会员，600 积分=5 天」 | WPS 账号登录态 | 🔒 私有接口 |
| **纳米 AI** | 360（天津三六零快看） | `n.cn`（App 为 `search.n.cn`） | 每日签到得「纳米」；邀请码得 5000 Tokens（30天）；Nano Pro 198元/月含 30 万 Tokens | ✅ | ✅ **有** | 官方路径：头像→签到页→输入邀请码→免费领纳米。邀请码**仅限首次绑定，过期或重复无效**。连续签到天数越多纳米越多。历史活动码如 `JCYDK`（100 积分） | 手机号 / 开发者密钥 | 📄 **官方 API**（`developer.n.cn`，搜索 100 Token/次、对话按长度折算最低 50） |
| **秘塔 AI** | 秘塔科技 | `metaso.cn` | 基础检索免费；**每日免费刷新积分**（当日有效、不累计、次日清零）；会员 ¥39/月 | ✅ | ❌ **无签到**（自动刷新） | 官方用户协议 3.2.2 原文：「普通用户每日可获得免费额度的刷新积分；会员用户每日可获得的刷新积分额度，以其已购买会员权益或销售政策约定为准。每日获得的刷新积分均仅限当日有效，未使用部分不累计、不结转，并于次日清零」 | 开放 API Key | 📄 **官方 Open API + MCP**（¥0.03/次，支持 MCP） |
| **智谱清言 / GLM** | 智谱 AI | `chatglm.cn`<br>`open.bigmodel.cn` | 新用户 100 万 Token（30天）；实名再叠加 400 万；**GLM-4-Flash 永久免费不限量**（仅限速） | ✅ | ✅ **有**（每日登录赠送） | 官方《积分规则》原文：「登录赠送：用户每日登录智谱清言，可获赠一定积分，具体下发数量见积分明细」。另有任务中心（AgentMore）、订阅赠送 | `Bearer {api_key}` | 📄 **官方 Open API** ✅实测 |
| **豆包办公** | 字节 | `doubao.com`（桌面版 `/download/desktop`） | 与豆包主站通用积分；新用户可领 30 天专业版订阅 | ✅ | ✅ **有** | 「我的→活动中心」手动签到，**连续签到天数越多单次奖励越高**。日常任务（浏览/分享/体验新功能）每个几十积分。邀请码双方得积分 | 手机号/抖音/飞书扫码 | 🔒 私有接口（`dev.doubao2api` 可反代） |
| **Kimi** | 月之暗面 | `kimi.com` / `kimi.com`（国内）<br>`kimi.ai`（海外） | 网页版/App **完全免费无限对话**；Kimi+ 会员；「登月同行计划」邀请抽奖得会员额度 | ✅ | ⚠️ **非签到制** | 官方口径是**任务中心制**：「每日登录、分享一篇对话、评价 AI 回答、绑定手机号」完成任务得「算力点数」，换 K3 体验包。属于「登录+做任务」，不是纯签到 | `access_token`（JWT，网页版）<br>官方平台 `Bearer sk-xxx` | 📄 **官方 Open API** ✅实测<br>🔒 网页版私有接口（多项目可反代） |
| **讯飞听见 / 讯飞办公** | 科大讯飞 | `iflytek.com` | 免费额度口径混乱（各渠道说法 2h~10h/月 不等），新用户赠 30 分钟试用 | ⚠️ 有（按时长） | ❌ **未查到** | 未查到官方公开的每日签到规则。免费版多不支持 AI 结构化总结 | 讯飞开放平台 AppID + APIKey | 📄 官方开放平台（WebAPI 转写） |
| **钉钉 AI 助理** | 阿里 | `dingtalk.com` | 依托钉钉组织，部分能力随组织版本 | ⚠️ | ❌ **未查到独立签到** | 未查到面向 C 端办公 AI 的公开签到规则。千问办公可从钉钉左侧导航栏直接唤起（免二次登录） | 钉钉开放平台 AccessToken | 📄 官方开放平台 |
| **飞书 AI / 飞书妙记** | 字节（飞书） | `feishu.cn` | 个人免费：妙记每月 300~500 分钟（各渠道口径不一）；AI 高级会员 69元/月 | ✅（按分钟） | ❌ **无签到** | 未查到飞书侧有每日签到领积分机制 | `tenant_access_token` 或 `user_access_token` | 📄 **官方 Open API** ✅实测（妙记有完整 API） |
| **百度文库 AI / 库库 AI** | 百度 | `wenku.baidu.com`<br>`kuku.baidu.com` | 见「库库 AI」行 | ✅ | ✅ **有** | 同库库 AI | 百度账号 | 📄 / 🔒 混合 |
| **纳米 AI** | 360 | 见上 | 见上 | — | — | — | — | — |
| **扣子 Coze** | 字节 | 见上 | 见上 | — | — | — | — | — |
| **Kimi Code / OpenCode Zen** | 月之暗面 / 其他 | — | 未查到与办公场景强相关 | ⚠️ | ❌ | 与办公无强关联，不建议纳入本面板 | — | — |
| **wolai / flomo / 飞书多维表格 AI** |  wolai(中国)、flomo(中国) | `wolai.com` / `flomoai.com` | 未查到办公 AI 签到机制 | ❌ | ❌ **未查到** | 未查到官方公开签到规则。飞书多维表格 AI 走飞书平台额度 | — | 📄 飞书多维表格 API ✅ |

### 2.2 官方文档明文规定的签到条款（可直接引用的关键句）

这部分是本报告最有价值的部分——**这些是官方页面原文，不需要猜**：

**千问办公（`help.aliyun.com/zh/qwenwork/qw-personal-benefits`）**

> 每日登录奖励 限时 100积分 / 200积分 / 300积分（对应免费版/标准版/高级版）
> 新用户注册赠送（限时）：首次进入个人版（我的 AI 团队）赠送 2000 积分，有效期 3 个月。
> 积分包单价：0.05 元/积分，最低 200 积分 = 10 元

**扣子 Coze（`space.coze.cn/open/docs/guides/coze_points_activity_rules`）**

> 1.2 参与方式：在活动期间内，用户每日完成登录即可获得当日的登录奖励，每个自然日每个账号仅限领取一次。
> 1.3 奖励规则：每日成功登录可获得 1500 活动积分。
> 1.4 发放与有效期：积分通常在用户完成登录后实时或 T+1 发放至您的账户。每次发放的活动积分有效期为 1 天，从发放之时起算。每日获得的积分有效期将独立计算，可累计使用。
> 3.2.2 每日刷新积分：普通用户每日可获得免费额度的刷新积分…未使用部分不累计、不结转，并于次日清零。（秘塔）

**库库 AI（`kuku.baidu.com/landing/tscp_doc/...`）**

> 首次下载登录送500积分
> 每日登录+50 登录领取
> 完成一次对话+50 登录领取
> 邀请新用户登录网页版+50 每账号每日最多送150
> 邀请新用户登录电脑版+100 邀请登录电脑版，每账号每日再多得300
> 第4期·10/10 结束

**智谱清言（`chatglm.cn/pay/score/policy?lang=zh`）**

> 登录赠送：用户每日登录智谱清言，可获赠一定积分，具体下发数量见积分明细。
> 积分消耗…以对话型消耗举例，每次发起对话或执行任务，系统将根据不同难度任务的实际用量扣减相应积分

### 2.3 ✅实测结果（本机 curl，2026-10-04）

```
# 扣子 Coze 官方 Open API —— 返回业务错误码而非 HTTP 错误，说明端点健康
$ curl -X POST https://api.coze.cn/v3/chat -H "Content-Type: application/json" -d '{}'
HTTP 200
{"code":4101,"msg":"The token you entered is incorrect. ...refer to https://coze.cn/docs/developer_guides/authentication",
 "detail":{"logid":"202610040957096760980A3A8DCAF1FDC2"}}

$ curl -X POST https://api.coze.cn/v3/chat -H "Authorization: Bearer pat_fake" \
       -H "Content-Type: application/json" -d '{"bot_id":"1","user_id":"1"}'
HTTP 200
{"code":4200,"msg":"Requested resource bot_id=1 does not exist. Ensure you are the correct resource identifier.",
 "detail":{"logid":"2026100409580525576377BFF6288AAA03"}}
# → 结论：假 Token 能被识别到 bot_id 校验这一步，说明网关与鉴权链路通

# 智谱 GLM 官方 API
$ curl -X POST https://open.bigmodel.cn/api/paas/v4/chat/completions -d '{"model":"glm-4-flash"}'
HTTP 401
{"error":{"code":"1001","message":"Header中未收到Authorization参数，无法进行身份验证。"}}

# Kimi 月之暗面官方 API
$ curl -X POST https://api.moonshot.cn/v1/chat/completions -d '{}'
HTTP 401
{"error":{"message":"Incorrect API key provided","type":"incorrect_api_key_error"}}

# 飞书妙记官方 API（minutes/v1，文档: open.feishu.cn/document/server-docs/minutes-v1）
$ curl "https://open.feishu.cn/open-apis/minutes/v1/minutes/testtoken12345678901234" -H "Authorization: Bearer t-invalid"
{"code":99991663,"msg":"Invalid access token for authorization...",
 "error":{"log_id":"20261004095805BF6A6F75AC3743D94B1D",
   "troubleshooter":"https://open.feishu.cn/search?from=openapi&log_id=...&code=99991663"}}
# → 飞书错误码 + 排障链接都返回了，官方 API 链路健康

# 站点存活
200  qwenwork.cn                200  metaso.cn
200  yuanbao.tencent.com       200  chatglm.cn
200  kuku.baidu.com            200  kuku.baidu.com/genflowpro   ← 签到活动页
200  help.aliyun.com/zh/qwenwork/  200  kimi.com
200  www.coze.cn              200  www.n.cn
302  www.doubao.com           302  vip.wps.cn
302  yiyan.baidu.com307  www.notion.so
200  www.feishu.cn            200  www.dingtalk.com
200  www.iflytek.com          200  tongyi.aliyun.com
302  www.kdocs.cn             403  wenku.baidu.com（反爬，正常）
000  genflow.baidu.com        （已迁移至 kuku.baidu.com，域名不通）
000  workspace.google.com     （网络受限，非站点问题）
000  www.microsoft.com/microsoft-365 （网络受限，非站点问题）
```

---

## 3. 国际办公 AI 完整表格

| 平台 | 公司 | 免费额度机制 | 有签到 | 鉴权方式 | 接口类型 | 备注 |
|---|---|---|---|---|---|---|
| **Microsoft 365 Copilot** | 微软 | Copilot Chat 免费版可用；Microsoft 365 Personal $9.99/月、Premium $19.99/月 含 Office 内 Copilot；Copilot Business $21/用户/月（2026-06-30 前促销 $18）；M365 E7 $99/用户/月 | ❌ **无签到** | OAuth2 / Microsoft Graph | 📄 官方 Graph API | 免费版受 Microsoft credits 与高峰时段限制；企业级 Copilot 需付费席位 |
| **Google Workspace + Gemini** | Google | Workspace 基础版含基础 Gemini（Help me write、Meet 自动摘要、NotebookLM 基础、Workspace Studio 100 flows/月）；付费 AI Expanded / AI Ultra 档 | ❌ **无签到** | OAuth2 / Google Workspace API | 📄 官方 API | 免费账号 32k 上下文；AI Plus $4.99（128k，2× 用量）；AI Pro $19.99（1M，4× 用量）。限额每 5 小时刷新一次，到周上限 |
| **Slack AI** | Salesforce | 付费版功能，未查到免费签到 | ❌ | Slack OAuth | 📄 Slack API | — |
| **Zoom AI** | Zoom | 随会议方案 | ❌ | Zoom OAuth | 📄 Zoom API | — |
| **Otter.ai** | Otter | 免费 300 分钟/月，单场 30 分钟上限，3 次终身导入；Pro $8.33/用户/月（年付）1200 分钟，单场 90 分钟 | ❌ | 邮箱/OAuth | 📄 有 API（付费档） | 实时字幕最强；仅英语/法语/西班牙语 |
| **Fireflies.ai** | Fireflies | 免费版**无限转写**，存储 400 分钟/团队，20 AI credits；Pro $10/席位/月 | ❌ | OAuth | 📄 有 API | 60+ 语言，AskFred 助手，50+ 集成，HIPAA |
| **Fathom** | Fathom | 免费版无限录音 + 5 次 AI 摘要/月（被评为三者中最宽松） | ❌ | OAuth | 📄 | 转写准确率约 95% |
| **Granola** | Granola | 免费层有限 | ❌ | — | 未查到公开 API | 见下方开源替代 anarlog |
| **Reclaim.ai** | Dropbox | SaaS 订阅制 | ❌ | OAuth | 📄 | 偏日程管理，非纪要 |

### 3.1 ⭐ 开源会议纪要项目（这部分是重点，独立成表）

用户特意问了「开源的会议纪要项目，可能是反代方向」，这里单独列出，这是国际侧最有价值的发现：

| 项目 | Star | License | 核心能力 | 自托管 | 能否接你的网关 |
|---|---|---|---|---|---|
| **Vexa-ai/vexa** | 2k+（官网自述） | **Apache-2.0** | Bot 加入 Google Meet / Teams / Zoom，实时说话人分离转写，REST + WebSocket + **MCP Server** | ✅ 官方支持 Docker Compose + Helm(K8s)，可 air-gap | ✅✅ **首选**。有 REST API + WebSocket + MCP + Dashboard，能直接当「会议纪要上游」 |
| **Zackriya-Solutions/meetily** | 31,410 | 见仓库 | 隐私优先，Parakeet/Whisper 4× 加速实时转写 + 说话人分离 + Ollama 本地总结，Rust 编写 | ✅ 100% 本地 | ✅ 桌面应用（Win/macOS），适合本地部署，不提供 API |
| **silverstein/minutes** | 1,526 | — | 本地优先的 Granola/Otter 替代，可被 Claude Code / Codex / Cursor 及任意 MCP 客户端调用 | ✅ | ✅ **MCP 优先**，最贴你的网关架构 |
| **fastrepl/anarlog** | 9,428 | — | 开源 Granola AI 替代 | ✅ | ✅ 会议纪要 |
| **Natively-AI-assistant/natively-cluely-ai-assistant** | 2,656 | — | 开源 AI 会议助手 / 面试副驾 / 笔记 | ✅ | ✅ |
| **amicalhq/prismical** | 96 | — | 本地 AI 笔记，录音/讲座/语音笔记，本地 ASR | ✅ | ⚠️ 体量小 |
| **pasrom/meeting-transcriber** | 193 | — | macOS 端侧会议转写（Teams/Zoom/Webex） | ✅ | ⚠️ 仅 macOS |
| **Onyx-Dev-Labs/doodle-note** | 179 | — | 隐私优先本地会议捕获 + AI 笔记 | ✅ | ⚠️ |
| **Knuckles92/OpenWhisper** | 200 | — | 本地听写与会议笔记，Whisper 或 OpenAI API | ✅ | ⚠️ |

> **注意**：Vexa 官网明确说「Starred by 2k+ developers on GitHub」，且提供托管版（$12/席位/月、$0.30/bot-hr）**和完全自托管版（$0，Apache-2.0）**。它官方文档里直接给了自托管快速启动命令：
> ```
> git clone https://github.com/Vexa-ai/vexa.git && cd vexa && make all
> # 或 make lite（单容器全一体镜像，资源受限时用）
> ```
> 这意味着「会议纪要」这一类你原本以为必须反代的场景，现在有**正当的免费自托管开源方案**。

### 3.2 关于 Scrapling / Rev AI 开源替代

- **Scrapling**：是网页抓取/爬虫框架（Python），不是会议纪要工具。用来做网页版办公 AI 的抓取可以，但和会议纪要无关。
- **Rev AI 开源替代**：Rev AI 商业闭源，无官方开源替代。上表 meetily / anarlog / natively / Vexa 都可替代其转写+摘要核心能力。

---

## 4. 反代 / 集成项目（GitHub）

### 4.1 豆包系

| 项目 | Star | License | 最后推送 | 说明 |
|---|---|---|---|---|
| **wangchuxiaoji-oss/doubao2api** | 258 | Apache-2.0 | 2026-05-24 | ⭐ 主力项目。逆向豆包客户端 API → OpenAI 兼容 REST。多模态对话/思维链/联网搜索、读 60+ 种文件格式（PDF/Word/Excel/代码）、文生图、Seedance 2.0 视频、音乐生成、1GB 文件上传（永久 TOS URI）、QR 扫码登录全平台无需桌面端 |
| **robinxplorer/doubao2API** | 104 | — | 2026-06-03 | 豆包网页版对话转 OpenAI 接口 |
| **HongYan789/doubao2API** | 32 | — | 2026-05-11 | 豆包网页版逆向 API 网关 |
| **SeiShonagon520/doubao2api** | 15 | — | 2026-09-20 | 基于**最新网页端重构**，解决旧版接口失效与反爬拦截，多模态 + 思维链 + 无痕会话 + 沉浸式翻译 |
| **Hu410/Doubao2api** | 10 | — | 2026-09-25 | 轻量级代理，逆向豆包 Web API → 标准 OpenAI Chat Completions |
| **fresh-claw/doubao-web-to-api** | — | — | — | Playwright 方案（有 CSDN 部署教程，需改源码 3 处：注释 212/213 行、加 return "快速"、改 recv 选择器） |

> **推荐**：优先 `wangchuxiaoji-oss/doubao2api`（功能最全、Apache-2.0、258 star）。若主站接口失效，`SeiShonagon520/doubao2api` 明确写了适配新页面结构。

### 4.2 Kimi 系

| 项目 | Star | License | 最后推送 | 说明 |
|---|---|---|---|---|
| **chopper1026/kimi2api** | 244 | — | 2026-05-14 | ⭐ Kimi 网页版反代，会话能力 → OpenAI 兼容 |
| **BuiltByLou/kimi2api** | — | — | — | 单文件 FastAPI，多账户轮询，有状态多轮对话 `/v1/chat/completions/{conversation_id}`，默认开联网搜索 |
| **XxxXTeam/kimi2api** | 61 | — | 2026-05-04 | — |
| **xiaoY233/Kimi-Free-API** | 60 | — | 2026-02-26 | Node.js/TS，高速流式、智能体对话、联网搜索、K2 思考模型、多路 token 负载均衡、自动清理会话痕迹 |
| **izaart95-jpg/KimiFreeAPI** | — | — | — | Go 原生实现，**区分国内/海外双区域**（`--region chinese` / `global`），Agent Mode 把 OpenAI tools[] 转成 XML 段注入 Kimi，Context 满自动轮换备用会话 |
| **ForgetMeAI/FreeKimiAPI** | 30 | — | 2026-06-20 | OpenAI/Anthropic/Responses 兼容代理 |
| **hellyangy/Kimi2API-Tutorial** | 9 | — | 2026-05-14 | Docker 部署教程 |
| **lixia051/kimi2api-tool-calls** | 6 | — | 2026-05-24 | 给 kimi2api 加标准 tool_calls |

> **推荐**：`chopper1026/kimi2api`（star 最高）。若要国内/海外双区域切换 + tool_calls 稳定性，用 `izaart95-jpg/KimiFreeAPI`。
> **重要**：`KimiFreeAPI` 的 `--region chinese|global` 设计说明 **kimi.com 与 kimi.ai 后端相同、仅域名不同** —— 这对你的面板很有价值，可以做成一个 Provider 两个 BaseURL。

### 4.3 秘塔系（办公属性强）

| 项目 | Star | License | 最后推送 | 说明 |
|---|---|---|---|---|
| **jonnyquan/metaso-free-api** | 5 | — | 2025-01-26 | 秘塔白嫖服务，流式输出、联网搜索、多轮对话，多路 token。⚠️ **最后推送 2025-01，已近一年半未更新，接口大概率已失效** |
| **SecretRichGarden/metasota-API-MCP** | 2 | — | 2026-03-05 | 基于秘塔搜索 API 的 MCP。README 直言「去掉了原生 chat 功能，因为官方功能就是一坨屎」。✅ **但秘塔本身已有官方 MCP，建议优先用官方的** |
| **tong-io/tongflow-api-metaso** | 0 | — | 2026-09-21 | TongFlow 官方插件：MiniMax H3 视频生成托管在秘塔 |

> **强烈建议**：秘塔**不要用反代**，直接用官方 Open API + MCP（`metaso.cn/search-api`，¥0.03/次）。理由：官方 API 已在售且支持 MCP，反代没有任何优势且随时挂。

### 4.4 千问 / Qwen 系

| 项目 | Star | 最后推送 | 说明 |
|---|---|---|---|
| **Vanszs/qwencloud-generator** | 291 | 2026-06-27 | QwenCloud 账号自动注册 + API key 批量获取，多线程，TUI 面板 ⚠️ 用途是批量薅号，请自行评估合规性 |
| **LyubomirT/intense-rp-next** | 198 | 2026-07-05 | 桌面应用 + OpenAI 兼容 API，代理 LLM 网页 UI |
| **pedrofariasx/qwenproxy** | 179 | 2026-10-01 | Playwright 自动化路由到模型。**最近推送（2026-10-01），活跃** |
| **aptdnfapt/qwen-code-oai-proxy** | 171 | 2026-04-16 | 用 qwen-code OAuth 文件起代理服务 |
| **SuperJJ007/CSSwitch** | 473 | 2026-08-22 | 一键把 DeepSeek/通义千问/GLM/Kimi/MiniMax/硅基流动接入 Claude Code |
| **wangxiaoshuai1998/QwenWorkGuide** | 8 | — | QwenWork 开源实战指南（非代码） |
| **ahang1598/doubao-workbuddy-qwenwork-skills** | 53 | — | 豆包办公模式 / WorkBuddy / 千问办公 / ChatGPT agent 内置的 skills 与专家团 |

> ⚠️ **重要发现**：**没有找到针对「千问办公 QwenWork」的成熟反代项目**。搜索 `qwenwork` 只返回指南和 skills 类仓库。这符合预期——千问办公是 2026 年的新 Agent 产品，走客户端 + 钉钉唤起，非 OpenAI 兼容结构。**不要期待能反代它，只能靠客户端自动化。**

### 4.5 扣子 Coze 系

| 项目 | Star | License | 最后推送 | 说明 |
|---|---|---|---|---|
| **deanxv/coze-discord-proxy** | 3,780 | — | 2026-02-22 | 通过 Discord 对话 Coze Bot，以 API 形式请求 GPT4，含对话/文生图/生文/知识库检索 |
| **fatwang2/coze2openai** | 655 | — | 2024-10-28 | ⚠️ **最后推送 2024-10，已废弃，Coze API 早已改为 v3** |
| **coze-dev/coze-py** | 492 | — | 2026-02-26 | ✅ **Coze 官方 Python SDK**。做官方 API 集成应该用这个 |

> **强烈建议**：Coze 有完整官方 Open API（`api.coze.cn/v3`，已实测），**不要用反代**。用官方 `coze-py` SDK 或直接 REST。

### 4.6 多平台统一网关（★ 与你的面板最相关）

| 项目 | Star | License | 说明 |
|---|---|---|---|
| **xllm-go/bypass** | 1,245 | — | ⭐ **最接近你要的东西**。集成 openai-api、**coze**、deepseek、cursor、windsurf、qodo、blackbox、you、grok、bing 绘画**多款 AI 的聊天逆向接口适配到 OpenAI**。star 1245，是这个细分领域最大的开源项目 |
| **spf0209/FreeAI-Gateway** | 32 | — | OpenAI 兼容网关，覆盖 GLM / Kimi / Qwen / MiniMax / DeepSeek / Z.ai |
| **qixing-jk/all-api-hub** | — | **AGPL-3.0** | ⭐ 你已知的项目。浏览器插件形态，统一管理 New API 兼容中转站：余额/用量看板、智能站点识别、**自动签到**、API 凭据库、模型比价、Web 嗅探。导出到 CherryStudio / CC Switch / **CLIProxyAPI** / Claude Code Router / Kilo Code |
| **qianbkk/all-api-hub** | — | — | all-api-hub 的个人 fork |
| **SuperJJ007/CSSwitch** | 473 | — | 一键接入多家 API 到 Claude Code |
| **Btluo1/cck** | 113 | — | CCK 账号管理器：添加自定义账号，**批量管理 AI 编程工具账号额度**，一键启动客户端 |
| **wicm84266964/Buddy2api** | 84 | — | Local OpenAI 兼容网关 for Codex 和桌面 AI 客户端，**Default WorkBuddy** |

> **对你的面板的直接结论**：
> 1. `all-api-hub` 的「自动签到」是**浏览器插件 + DOM/接口嗅探**模式，依赖 New API 中转站的统一结构。**办公 AI（千问办公/库库/元宝/WPS）不是 New API 站点，这套逻辑套不上去**，需要为它们单独写适配器。
> 2. `xllm-go/bypass` 是目前唯一把 coze 等多种异构源统一到 OpenAI 的项目，但它的定位是「聊天逆向接口」，不是「办公 Agent 平台」。
> 3. **市面上没有现成的「办公 AI 统一网关」**。你要做的东西在开源里是空白的 —— 这是个真实的机会点。
> 4. `Buddy2api` 的 **Default WorkBuddy** 说明 WorkBuddy 已经有现成网关实现了，你的 WorkBuddy 覆盖可以复用。

### 4.7 其他相关

| 项目 | Star | 说明 |
|---|---|---|
| **iptag/jimeng-api** | 1,039 | 即梦/Dreamina 官方 API 逆向，文生图/图生图 |
| **zh cognizan1997/jimeng-free-api-all** | 597 | 即梦反代 2api，5.x/4.x 图像+视频，OpenAI 兼容，自带管理控制台，自动同步最新模型 |
| **wwwzhouhui/jimeng-mcp-server** | 81 | 即梦 MCP Server，让 Claude/Cherry Studio 调用即梦 |
| **SleepingBag945/notion_manager** | 112 | Notion AI 账号池 + 面板 + **Anthropic 兼容 API 代理**（供 Claude Code 用） |
| **OmarElKadri/notion-proxy** | 13 | 把 Notion AI 暴露为 Anthropic Messages API |
| **vkop007/NotionAPI** | 9 | 轻量级 Notion AI 集成 |
| **1030694584/wps-airsheet-python** | 2 | WPS 智能表格 API 客户端，Python/JS 双实现 |
| **it235/officeai** | 9 | Office 加载项 JS API 写的 AI 智能体插件 |
| **DemoMacro/office-open** | 35 | 从纯 JSON 生成 docx/xlsx/pptx |
| **apexcheng/yingdao-xbot-ai-agent** | 35 | 影刀 xbot AI Agent，含 **Excel/WPS 自动化** API |

---

## 5. 「有签到但需要登录才能确定端点」清单

⚠️ **这一节就是为你避免瞎猜路径而写的**。以下平台的签到机制**确证存在**，但**签到端点必须登录后才能确定**，我没有、也不应该去猜具体路径。

### 5.1 已确证签到存在，但端点未知（必须登录抓包）

| 平台 | 签到确证来源 | 已知入口 | 未知部分 |
|---|---|---|---|
| **库库 AI** | 官方活动页原文（每日登录+50 等） | PC 客户端内签到；活动入口「点击活动进入签到页」 | ❓ Web 端是否有签到 POST 接口、路径、请求体、签名算法全未知。云端 Skill 可自动签到（电脑关机也能签），说明存在服务端接口，但未公开 |
| **腾讯元宝** | 社区攻略（App 我的→福利中心） | App 内「福利中心」 | ❓ 是否有 Web 签到接口。若只有 App 内，需要 mitm/Frida 或走 UI 自动化 |
| **WPS AI / 灵犀智点** | 官方社区帖「每天签到可以领 200 个点」 | PC：个人中心→天天签到；手机：我的→签到 | ❓ 智点 API 端点、是否区分 PC/移动端签到通道、连续签到断签规则未知。另有旧版「积分」体系（天天签到+3 分、周+5 分、会员+2 天）与新版「智点」并存，容易混淆 |
| **豆包办公** | 「我的→活动中心」手动签到 | 豆包客户端/网页版「我的→活动中心」 | ❓ 签到端点与签到奖励的具体阶梯数值（只知道「连续签到天数越多单次奖励越高」，具体每天多少未查到官方数值） |
| **纳米 AI** | 签到页 + 邀请码路径明确 | 头像→签到页 | ❓ 每日签到给多少纳米（只知道连续签到越多越多）、签到接口路径未知。⚠️ 网上流传的 `ZKTWT`/`360666`/`360888`/`JCYDK` 等邀请码是第三方分享，官方不保证有效 |

### 5.2 端点已明确，只需补 Token（可直接开工）

| 平台 | 端点 | 缺什么 |
|---|---|---|
| **千问办公 QwenWork** | ❓未找到公开端点。官方文档只说「桌面端左下角头像→设置→用量面板」或「官网积分详情页」 | 需登录抓包确定「每日登录奖励」的领取接口 |
| **扣子 Coze** | ✅ 官方文档明确积分在「扣子/扣子编程左下角单击积分卡片」查看。签到=登录即领，可能**无需单独调用**（登录即到账），但 T+1 发放意味着可能有补发接口 | 需登录确认是否有独立的「领取」动作，还是纯靠登录触发 |
| **智谱清言** | ✅ 官方《积分规则》明写「每日登录赠送」 | 需登录确认自动到账还是需点「领取」 |
| **秘塔 AI** | ✅ 官方协议明写「每日刷新积分」 | 无需签到，但需确认是否有「刷新/领取」接口（协议措辞是"每日可获得"，暗示自动） |

### 5.3 ⚠️ 不要猜路径的三个陷阱

1. **WPS 双轨制**：WPS 现在同时存在「积分」（旧，天天签到 +3）和「灵犀智点」（新，签到 200 点）两套体系，来源不同、有效期不同、消耗场景不同。**接错体系会导致面板显示错误**。建议先登录确认当前账号属于哪套。
2. **千问办公「限时」标签**：官方文档里「每日登录奖励」和「新用户 2000 积分」都带「限时」标记。**这是活动性质，随时可能下线**。面板上要做成可配置开关，不要硬编码。
3. **库库 AI 是活动期制**：官方活动页明写「第4期·10/10 结束」，V2EX 帖子说「活动时间只有 27 天，9 月 27 号就结束了」（不同期次时间不同）。**签到规则每期可能变**，接入前必须重新核对活动页。

---

## 6. 已死 / 不推荐清单

| 项目 | 状态 | 原因 |
|---|---|---|
| **fatwang2/coze2openai**（655⭐） | ❌ **已死** | 最后推送 2024-10-28。Coze API 早已改版为 v3（`/v3/chat`），该项目对接的是老接口。star 数高但完全不可用 —— 这正是只看 star 会踩的坑 |
| **jonnyquan/metaso-free-api**（5⭐） | ⚠️ **半死** | 最后推送 2025-01-26，近一年半未更新。且秘塔已有官方 API + MCP + MCP 官方插件，反代无意义 |
| **metasota-API-MCP**（2⭐） | ⚠️ **不推荐** | 秘塔官方已支持 MCP，自己包一层反而是负资产 |
| **Vanszs/qwencloud-generator**（291⭐） | ⚠️ **不建议** | 功能是「QwenCloud 账号自动注册 + API key 批量获取」，多线程批量薅号。与你的「统一管理已登录账号」目标完全相反，且有封号风险 |
| **文库 wenku.baidu.com 直连** | ❌ | 本机 curl 返回 **403**（反爬）。百度系建议只走 kuku.baidu.com |
| **genflow.baidu.com** | ❌ **域名已失效** | curl 返回 000（连接不通）。已迁移至 `kuku.baidu.com`，任何指向 genflow 的配置都要改 |
| **千问办公的反代** | ❌ **不存在** | 搜遍 GitHub 无成熟项目。QwenWork 是客户端 + 钉钉唤起架构，非 OpenAI 兼容结构。不要在这上面花时间 |
| **钉钉 AI 独立签到** | ❌ **未查到** | 作为办公协作平台，没有面向 C 端的独立签到领积分机制。它的价值是「千问办公可在钉钉侧边栏直接唤起，无需额外登录」 |
| **讯飞听见免费额度** | ⚠️ **数据混乱不建议采信** | 各渠道说法从「新用户 30 分钟」到「每月 2 小时」到「每月 10 小时」到「每月 600 分钟」全都有，互相矛盾。且多篇提到免费版不支持 AI 结构化总结。接入前必须以官网实时页面为准 |
| **飞书妙记免费分钟数** | ⚠️ **数据混乱** | 查到 300 分钟 / 500 分钟 / 10G 存储换算 三种说法。以登录后账号内显示为准 |
| **Scrapling 当会议纪要方案** | ❌ **误解** | Scrapling 是网页抓取框架，与会议纪要无关。它在抓取网页版办公 AI 时有用，但别当会议纪要方案 |
| **Rev AI 开源替代** | ❌ **不存在** | Rev AI 商业闭源。用 meetily / anarlog / natively / Vexa 替代 |
| **Kimi Code / OpenCode Zen** | ⚠️ **不相关** | 与办公场景无强关联，纳入面板会造成功能边界混乱 |

---

## 7. 待确认事项

需要你登录后抓包才能确定的事项，按优先级：

### 优先级 P0（决定能不能做自动化）

1. **千问办公 QwenWork「每日登录奖励」的领取端点**
   - 官方文档确认了 100 积分/天，但没有任何公开的 API 文档或端点提示。
   - 需要：登录 `qwenwork.cn` → 打开 F12 → 看「用量详情页」(`/agents/qwenwork` 相关路径) 是否有积分领取/查询接口 → 记录 URL / 方法 / 请求体 / 鉴权头。
   - 特别确认：**登录是否本身就触发到账**，还是需要额外 POST。如果登录即到账，那自动化只需模拟登录，定时跑一次即可。

2. **库库 AI 签到接口**
   - 官方活动页规则明确，但活动期制（第 4 期 10/10 结束），每期可能变。
   - 需要：确认是否有 Web 端签到接口；官方提到的「Skill 云端自动签到」是怎么实现的（那个 Skill 的飞书文档链接 `my.feishu.cn/wiki/WrDvwl8SGi5ATOklRMBcMAk1nrf` 值得一读，可能直接给出端点）。
   - ⚠️ 我未能打开该飞书文档（需要登录），**建议你直接看**。

3. **WPS「智点」vs「积分」到底哪个是当前体系**
   - 需要：登录 WPS PC 客户端 → 个人中心 → 看签到给的是「积分」还是「智点」→ 抓包确认签到接口。
   - 判定：官方社区帖说「每天签到领 200 点」，但「积分宝宝养成记」帖说签到 +3 分。两帖时间不同，大概率已迁移到智点体系。

### 优先级 P1（影响面板设计）

4. **元宝签到是否有 Web 端点**
   - 若只有 App 内签到，实现方式将从「HTTP 定时任务」退化为「UI 自动化（Frida/改机/无障碍点击）」，成本差一个量级。建议先确认。

5. **豆包办公签到的具体阶梯**
   - 官方只有「连续签到天数越多奖励越高」，没有具体数值表。需要抓包或实际连签几天记录。

6. **纳米 AI 每日签到的纳米数额**
   - 网上信息混乱（500纳米/5000 Token 说法并存）。以 App 内签到页实际显示为准。

7. **智谱清言每日登录赠送的具体积分数**
   - 官方协议只说「一定积分，具体见积分明细」。需登录积分明细页。

### 优先级 P2（可选增强）

8. **秘塔开放 API 的具体规格**：`metaso.cn/search-api` 控制台（¥0.03/次）— 需注册后看文档，确认是否 OpenAI 兼容还是自定义 schema，以及 MCP 接入方式。

9. **飞书妙记 API 的额度与错误码实操**：`minutes/v1` 已确认存在（实测拿到 99991663 错误码）。需确认个人账号能否建自建应用拿到 `user_access_token`，以及 `asr quota not enough`（错误码 2091008）触发时怎么充值。

10. **Notion AI 反代合规边界**：`SleepingBag945/notion_manager`（112⭐）和 `OmarElKadri/notion-proxy` 已存在。需确认个人账号池方案在 ToS 上的位置，以及 Notion AI 是否已改版导致这些项目失效（notion-proxy 最后推送 2026-06-25，notion_manager 2026-08-05，都还比较新）。

---

## 附录 · 全部访问过的 URL

### 官方文档 / 产品页

**千问办公（阿里）**
- `https://help.aliyun.com/zh/qwenwork/qw-personal-benefits` ← 签到规则原始出处
- `https://www.qianwenai.com/agents/qwenwork` ← 积分表格 + 官方两入口说明
- `https://developer.aliyun.com/article/1765904`
- `https://developer.aliyun.com/article/1766145`

**扣子 Coze（字节）**
- `https://space.coze.cn/open/docs/guides/coze_points_activity_rules` ← 每日登录 1500 积分原始出处
- `https://loop.coze.cn/open/docs/coze_pro/credits` ← 积分有效期与抵扣顺序
- `https://docs.coze.cn/cozespace_start_use`

**库库 AI（百度）**
- `https://kuku.baidu.com/landing/tscp_doc/18efbcc43e82bdfb052f2fd76ace4c19` ← 签到规则原始出处
- `https://www.zhujib.com/baidu-kuku-ai-review.html?id=267`
- `https://global.v2ex.co/t/1244594` / `https://global.v2ex.co/t/1243373`
- `https://sechub.in/view/3296619`

**秘塔**
- `https://files.metaso.cn/meta-user-policy` ← 每日刷新积分原始出处
- `https://www.kun.net/tools/metaso` ← ¥0.03/次 Open API + MCP
- `https://metaso.cn/` / `https://metaso.cn/search-api`

**智谱**
- `https://chatglm.cn/pay/score/policy?lang=zh` ← 每日登录赠送原始出处
- `https://platform.moonshot.cn/`（Kimi 定价）
- `https://www.php.cn/faq/2914331.html`（智谱免费额度细节）

**WPS**
- `https://bbs.wps.cn/topic/97140?chan=xt05` ← 每天签到 200 智点
- `https://forum.wps.cn/topic/62524` ← 签到入口 + 积分规则
- `https://vip.wps.cn/`

**飞书**
- `https://open.feishu.cn/document/server-docs/minutes-v1/minute/get?lang=zh-CN`
- `https://open.feishu.cn/document/minutes-v1/minute-media/get?lang=zh-CN`
- `https://open.larkoffice.com/document/uAjLw4CM/ukTMukTMukTM/minutes-v1/minute/upload`

**其他产品页**
- `https://www.coze.cn` / `https://www.kimi.com` / `https://metaso.cn` / `https://chatglm.cn`
- `https://yuanbao.tencent.com` / `https://www.n.cn` / `https://www.coze.cn`
- `https://www.doubao.com/` / `https://doubao.readaitime.com/`（豆包三端差异）
- `https://help.aliyun.com/zh/qwenwork/`

### GitHub 仓库

- `https://github.com/wangchuxiaoji-oss/doubao2api`
- `https://github.com/robinxplorer/doubao2API`
- `https://github.com/SeiShonagon520/doubao2api`
- `https://github.com/Hu410/Doubao2api`
- `https://github.com/HongYan789/doubao2API`
- `https://github.com/chopper1026/kimi2api`
- `https://github.com/BuiltByLou/kimi2api`
- `https://github.com/xiaoY233/Kimi-Free-API`
- `https://github.com/izaart95-jpg/KimiFreeAPI`
- `https://github.com/jonnyquan/metaso-free-api`
- `https://github.com/SecretRichGarden/metasota-API-MCP`
- `https://github.com/xllm-go/bypass`
- `https://github.com/spf0209/FreeAI-Gateway`
- `https://github.com/qixing-jk/all-api-hub`
- `https://github.com/qianbkk/all-api-hub`
- `https://github.com/Vanszs/qwencloud-generator`
- `https://github.com/pedrofariasx/qwenproxy`
- `https://github.com/aptdnfapt/qwen-code-oai-proxy`
- `https://github.com/deanxv/coze-discord-proxy`
- `https://github.com/fatwang2/coze2openai`
- `https://github.com/coze-dev/coze-py`
- `https://github.com/Vexa-ai/vexa` + `https://vexa.ai/self-host`
- `https://github.com/Zackriya-Solutions/meetily`
- `https://github.com/silverstein/minutes`
- `https://github.com/SleepingBag945/notion_manager`
- `https://github.com/zh cognizan1997/jimeng-free-api-all`
- `https://github.com/iptag/jimeng-api`
- `https://github.com/wicm84266964/Buddy2api`
- `https://github.com/SuperJJ007/CSSwitch`
- `https://github.com/btl uo1/cck`（实际 `Btluo1/cck`）

### 国际 / 会议纪要

- `https://github.com/fastrepl/anarlog`
- `https://github.com/Natively-AI-assistant/natively-cluely-ai-assistant`
- `https://github.com/amicalhq/prismical`
- `https://github.com/pasrom/meeting-transcriber`
- `https://github.com/Knuckles92/OpenWhisper`
- `https://github.com/Onyx-Dev-Labs/doodle-note`
- `https://www.saasayer.com/comparisons/fireflies-vs-otter`
- `https://ifeeltech.com/google-workspace-vs-microsoft-365/`

### 知识库 / 综合

- `https://ima.qq.com/wiki/?shareId=fc02987e66c942696935aa5aba7e55aa3eb382b7604ed8a9214fcf864e12c229...`（9款AI桌面办公智能体对比 — 豆包办公/千问办公/TRAE Work 签到规则来源）
- `https://www.myzaker.com/article/6a70bbb6b15ec0494233dbb1`（腾讯全系 AI 免费额度汇总 — 元宝签到连续6天规则来源）

---

> **说明**：本报告所有签到规则均标注了来源 URL。凡是标注「未查到」的，是确实没有找到公开信息，未做推测填充。GitHub star 数为 2026-10-04 Search API 查询快照，可能随时间变化。GitHub API 在调研末段触发速率限制（`API rate limit exceeded for 221.14.126.82`），最后一批仓库的 star 数未能二次复核，但均已在早前的搜索查询中返回过。