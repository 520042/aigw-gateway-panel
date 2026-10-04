# Vibe Coding 工具与开源反代项目调研

> 调研日期：2026-10-04
> 调研方式：WebSearch + WebFetch + GitHub REST API（`api.github.com/repos/*`、`/search/repositories`、`/commits`）
> 所有 star 数、更新时间、语言、archived 状态均来自本次实际访问的 GitHub API 返回，非记忆推测。
> 标注「未查到」的部分表示本次调研未能取得可靠信息，不做编造。

---

## 一、结论先行：最值得接入的 10 个

按「可直接接进 OpenAI 兼容网关」+「有签到/积分可自动化」两个维度排序。

| # | 项目 | 角色 | 一句话理由 |
|---|---|---|---|
| 1 | **[router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)** | 反代（54,037★ / Go / 2026-10-03） | 事实标准。把 Claude Code、Codex、Gemini CLI、Kimi、Antigravity、Devin、Grok Build、Muse 的 OAuth 凭据统一转成 OpenAI/Claude/Gemini/Codex 兼容 API，带多账号轮询负载均衡 + Management API。**已原生支持 CodeBuddy CN / CodeBuddy 国际 / Qoder / Kimi** —— 你已做的 Trae/CodeBuddy 可以直接复用它的 OAuth 托管体系。 |
| 2 | **[musistudio/claude-code-router](https://github.com/musistudio/claude-code-router)** | 路由网关（37,523★ / TypeScript / 2026-09-26） | 有 Windows/macOS/Linux 桌面版（EXE 直接下载），一个本地端点 `127.0.0.1:3456` 接管 Claude Code / Codex / Kilo Code / OpenCode / **WorkBuddy** / Grok CLI / Kimi CLI，带请求日志、resolved route、token 用量、成本估算、账号状态。上手成本最低。 |
| 3 | **[jlcodes99/cockpit-tools](https://github.com/jlcodes99/cockpit-tools)** | 账号管理（18,612★ / Rust / 2026-10-01） | **与你的网关定位最互补**。一个工具覆盖 Antigravity / Codex / Copilot / Windsurf / Kiro / Cursor / Grok CLI / CodeBuddy / CodeBuddy CN / Qoder / Trae / TRAE SOLO / Trae CN / TRAE SOLO CN / Zed / ZCode 的切号 + 配额监控 + 自动唤醒 + 多开实例，内置 API 服务（集成 CLIProxyAPI）。 |
| 4 | **[autumnsentiment/Trae2api-cn](https://github.com/autumnsentiment/Trae2api-cn)** | Trae 反代（31★ / Python / 2026-10-01） | 唯一明确带 **checkin（签到）** 功能的 Trae CN 反代。`/v1/models`、`/v1/chat/completions`、`/v1/responses`、`/v1/chat` 全覆盖，支持多账号 round-robin + 网页 OAuth 登录 + 并发控制 + 空闲会话回收。支持 Docker。与你已做的 Trae 签到是同一批数据源。 |
| 5 | **[Wei-Shaw/sub2api](https://github.com/Wei-Shaw/sub2api)** | 订阅分发（43,253★ / Go / 2026-10-03） | 订阅资源池化 + 用户级 Key 分发 + 限速计费。new-api 已原生支持它作为渠道类型（`ChannelTypeSub2API = 59`）。如果你要做「多账号拼车分摊」，它是这一层的成熟实现。 |
| 6 | **[linguo2625469/workbuddy2api-panel](https://github.com/linguo2625469/workbuddy2api-panel)** | CodeBuddy 反代（1,798★ / Go / 2026-10-01） | 把腾讯 WorkBuddy/CodeBuddy 账号变成 OpenAI 兼容网关，**自动完成任务中心全部任务**（积分任务），带 Web 面板（账号池 / 积分任务 / 配置热更新）。虽然你已做 WorkBuddy 网关，但它的「任务自动化」实现值得直接抄。 |
| 7 | **[dwgx/WindsurfAPI](https://github.com/dwgx/WindsurfAPI)** | Windsurf 反代（3,058★ / JavaScript / 2026-10-03） | 零 npm 运行时依赖。Windsurf/Devin 100+ 模型 → OpenAI Chat / Responses / Anthropic / Gemini **四套**标准 API。Windsurf 已改名 Devin Desktop，免费额度仅 25 prompt credits/月，正代成本极低。 |
| 8 | **[XxxXTeam/codex-proxy](https://github.com/XxxXTeam/codex-proxy)** | Codex 反代（163★ / Go / 2026-10-01） | Go 写的 Codex 代理，OpenAI + Claude 双协议兼容接口，中文社区项目，活跃维护。作为 CLIProxyAPI 的轻量备选。 |
| 9 | **[QuantumNous/new-api](https://github.com/QuantumNous/new-api)** | 网关层（49,237★ / Go / 2026-10-01） | 你已有则忽略，但值得确认：它已有 `ChannelTypeCodex(57)` / `ChannelTypeSub2API(59)` / `ChannelTypeNewAPI(60)` 渠道类型，可以直接吃 CLIProxyAPI 作为上游，做成「反代套网关」的两层架构。 |
| 10 | **[nguyenphutrong/quotio](https://github.com/nguyenphutrong/quotio)** | 账号管理（4,878★ / Swift / 2026-10-03） | macOS 菜单栏应用，统一管理 Claude / Gemini / OpenAI / Qwen / Antigravity 订阅，实时配额追踪 + 自动故障转移。**注意：仅 macOS**，Windows 用户需找移植版（见下文 `0xtbug/zero-limit`）。 |

### 优先集成的 3 个（如果你只做 3 个）

1. **CLIProxyAPI** —— 覆盖面最广、star 最高、更新最快（2026-10-03），且已原生支持 CodeBuddy CN/国际、Qoder，与你现有面板重叠度最高、可复用最多。
2. **cockpit-tools** —— 唯一一个把 Cursor/Windsurf/Kiro/Qoder/Trae 全家桶统一管理的账号管理工具，直接补上你面板缺失的「海外厂商配额监控」这一整块。
3. **Trae2api-cn** —— 唯一一个把「签到」写进反代逻辑的国产 IDE 项目，和你已有的 Trae 签到/积分模块天然同源，改造成本最低。

---

## 二、Vibe Coding 工具完整表格

### 2.1 海外工具

| 名称 | 官网 | 公司 | 免费额度机制 | 有无积分/额度系统 | 鉴权方式 | 官方 API 端点 | WebSocket/私有协议 |
|---|---|---|---|---|---|---|---|
| **Cursor** | cursor.com | Anysphere | Hobby 免费：2,000 completions/月 + 50 slow requests/月；14 天 Pro 试用；学生 .edu 验证免费 Pro | **有**，2025-06 起改为订阅价等值 credits 池（Pro $20→$20 credits），已废弃固定请求数 | 账号密码 / Google / GitHub SSO | **未查到公开模型列表/用量 API**（额度只能从 IDE dashboard 或 `/api/auth/full_stripe_usage` 类内部端点取） | 有，内部协议（aiserver.vscode-sonnet / cursor.sh 私有 RPC），多家反代在逆向 |
| **Windsurf / Devin Desktop** | windsurf.com → devin.ai | Codeium → Cognition（2026 收购） | Free 25 prompt credits/月；新用户 2 周 Pro 试用 100 credits；**Tab 补全全套餐无限** | **有**，Flex credits；Pro $15-20/500 credits；加购 $10/250 | 账号密码 / Google / GitHub | **未查到公开接口**（仅 Enterprise 有限管理 API） | 有，`a0ai-api-sg.byteintlapi.com` / `server.codeium.com` 类私有端点 |
| **Anysphere** | any sphere.com | Anysphere（Cursor 母公司） | 同 Cursor | 同 Cursor | 同上 | 未查到公开接口 | 同上 |
| **Sourcegraph Cody** | sourcegraph.com/cody | Sourcegraph | Free 档有限额；Pro $20/月 | 有（请求数） | 账号密码 / SSO / API Key | **未查到公开模型 API**（Cody 走自有 Gateway） | 有 |
| **Cline** | cline.bot | Cline（Bosu + Anthropic） | **无额度限制**，插件本身 Apache-2.0 全开源；成本只来自你接的模型 | **无积分系统** | 本地存 API Key（OpenRouter / Anthropic / Ollama） | 无（纯客户端） | 无（直连上游） |
| **Roo Code** | roocode.com | Roo Code | 同 Cline | **已失效** —— 2026-05-15 **关停，repo 已 archived**，社区 fork 名为 ZooCode | — | — | — |
| **Kilo Code** | kilocode.ai | Kilo Code | 网关上有免费模型 | 有（Kilo Pass $19/月） | API Key / 账号 | 有网关，`openrouter.ai/kilo` 兼容 | 无 |
| **Continue** | continue.dev | Continue | 插件开源免费 | 无积分 | API Key / OAuth | 无（直连） | 无。**注意**：2026-06 发布 Final 2.0.0 后 repo 转只读 |
| **Zed** | zed.dev | Zed Industries | Free：2,000 edit predictions/月，**自带 key 时 agent 无限用** | 有（次数） | 账号密码 / GitHub / API Key | 未查到公开模型 API | 有（agent 端点） |
| **Aider** | aider.chat | Aider（Aider-AI） | 开源免费，成本=模型 | 无积分 | API Key / OAuth | 无（直连上游） | 无 |
| **OpenHands** | all-hands.dev | All Hands AI | 开源免费（MIT） | 无积分 | API Key / OAuth | 无 | 无 |
| **OpenCode** | opencode.ai | SST（Anomaly） | **Zen 免费模型**：多来源口径（每日 50 次 / 每日 100 次 / 每 5 小时 150-300 次），UTC 0 点（北京 8:00）重置，月封顶 1000 次；Go 计划 $10/月 | **有（次数）**，不是积分 | 账号密码 / `/connect` 接 Copilot、Gemini、Qwen | **有**：`opencode.ai/zen`（Zen 网关），另有 `opencode.ai/zen/go/v1`（Go 专用，**强制要求 `x-opencode-session` 头，否则 400 MissingSessionID**） | 有（Zen 私有协议） |
| **SWE-agent** | swe-agent.com | Princeton NLP | 开源免费 | 无 | API Key | 无 | 无 |
| **Devin** | devin.ai | Cognition | Free 档「light quota」（未公布具体数值） | 有 | 账号密码 | 未查到公开接口 | 有 |
| **Factory Droid** | factory.ai | Factory | 有免费试用 | 有 | 账号密码 / OAuth | 未查到公开接口 | 有 |
| **Jules** | jules.google | Google | 免费（Gemini 额度内） | 无独立积分，并入 Google 账号 | Google 账号 OAuth | 未查到公开接口 | 有 |
| **Augment** | augmentcode.com | Augment | **注册送 30,000 credits 试用（需绑卡）**，无永久免费档；Indie $20/月 40,000 credits | **有，纯积分制** | 账号密码 / SSO | 未查到公开接口 | 有 |
| **Kiro** | kiro.dev | **Amazon AWS** | Free **50 credits/月（永久）**；新用户 **+500 bonus credits（30 天有效）**；学生 1,000 credits/月免费一年；首次升级 $20 抵扣；**老版本（IDE<0.11.133 / CLI<1.28.2）将于 2026-11-09 停止连接** | **有，纯积分制**，$0.04/credit 加购 | **AWS Builder ID / Google / GitHub OAuth** | 未查到公开接口（走 Amazon Bedrock） | 有（AWS 内部协议），多个 Kiro 代理项目在逆向 |
| **Qodo** | qodo.ai | Qodo（原 Codium） | 有免费档 | 有 | 账号密码 | 未查到公开接口 | 有（见 `xllm-go/bypass`） |
| **Codebuff** | codebuff.com | Codebuff | 有免费额度 | 有 | 账号密码 | 未查到公开接口 | 未查到 |
| **Warp** | warp.dev | Warp | Free - PRO 活动（`ghondar/warp.dev_account_manager` 在做自动化） | 有 | 账号密码 | 未查到公开接口 | 有 |
| **Bolt** | bolt.new | StackBlitz | 有免费额度 | 有（token） | 账号密码 | 未查到公开接口 | 有（WebSocket） |
| **Lovable** | lovable.dev | Lovable | 有免费额度 | 有（credits） | 账号密码 / Google | 未查到公开接口 | 有 |
| **v0** | v0.dev | Vercel | 有免费额度 | 有 | 账号密码 / GitHub | **有**，v0 官方 API（但仅面向付费企业） | 有 |
| **Mistral Vibe** | mistral.ai | Mistral | 有限次session + $10 API credit | 有 | 账号密码 | 有（La Plateforme） | 无 |
| **Amazon Q Developer** | — | Amazon | **2026-05-15 起停止新免费注册**，已引导用户转 Kiro | — | AWS Builder ID | — | — |
| **GitHub Copilot** | github.com/features/copilot | GitHub/Microsoft | Free 2,000 completions + 50 premium requests/月；学生与 OSS maintainer 免费 Pro；2026-06-01 起全套餐改 AI Credits（1 credit = $0.01）计量 | **有**，2026-06 起为 credits 池（Pro $10→$10 credits）；overflow $0.04/request | GitHub 账号 / PAT / OAuth | **未查到公开的模型列表/用量 API**（Copilot CLI 有，但内部端点如 `api.githubcopilot.com` 需 token 换取） | 有，`api.githubcopilot.com` 私有协议，多个项目在逆向 |
| **Google Jules** | jules.google | Google | 免费 | 无 | Google OAuth | 未查到 | 有 |
| **OpenAI Codex** | openai.com/codex | OpenAI | ChatGPT Free 用户仅桌面版；**CLI 需 Plus** | 有（5h/周配额） | ChatGPT OAuth / API Key | **有**，`chatgpt.com` 后端 + Responses API | **有**，Codex Responses WebSocket / sideband |
| **Claude Code** | claude.com/claude-code | Anthropic | 无免费额度（需 Pro 或 API 付费）；社区有 Claude Code Router 类免费方案 | 有（5h 窗口） | **Anthropic OAuth（authorization code + PKCE）** / API Key | **有**，`/v1/messages` | 有（Claude Messages SSE） |
| **Gemini CLI** | github.com/google-gemini/gemini-cli | Google | **2026-06-18 起停止服务免费用户和 AI Pro/Ultra 用户**，免费用户被导向 Antigravity CLI；付费走 Google AI Studio Code Assist | 无（已取消） | Google OAuth / API Key | 有（Code Assist / Generative Language API） | 有 |
| **Qwen Code** | github.com/QwenLM/qwen-code | Alibaba | **Qwen OAuth 免费层已于 2026-04-15 结束** | 已取消 | Qwen Chat OAuth（device flow） | 有（DashScope） | 有 |
| **Devin Desktop** | devin.ai | Cognition | 同 Windsurf | 同 Windsurf | 同 Windsurf | 未查到 | 同 Windsurf |

### 2.2 国内工具

| 名称 | 官网 | 公司 | 免费额度机制 | 有无积分/额度系统 | 鉴权方式 | 官方 API 端点 | WebSocket/私有协议 |
|---|---|---|---|---|---|---|---|
| **Trae / Trae CN / TRAE SOLO** | trae.cn / trae.com.cn | 字节跳动 | 已有你的网关覆盖（签到/权益/积分/兑换码） | **有** | 账号密码 / OAuth | 你已接入 api.trae.cn | 有。上游端点：`trae-api-cn.mchost.guru`（CN，tc 加密）、`a0ai-api-sg.byteintlapi.com`（SG，明文 JSON） |
| **豆包编程版 / MarsCode** | marscode.cn | 字节跳动 | 未查到独立签到机制 | 未查到公开接口 | 账号密码 / 抖音扫码 | **未查到公开接口** | 有（MarsCode IDE 私有协议）。注意 MarsCode 搜索结果里多为 2020-2022 的同名无关项目 |
| **CodeBuddy 国际 / 国内** | codebuddy.ai / codebuddy.cn | 腾讯 | 已有你的网关覆盖 | **有** | **OAuth（web login）** | api.codebuddy.*（你已接入） | 有。**CLIProxyAPI 原生支持 `CodeBuddy CN` + `CodeBuddy Intl` OAuth** |
| **Qoder 国际版** | qoder.com | Alibaba | **每日 10:00（UTC+8）可领 100 Credits**，需桌面端手动领取，错过不可补领、不累计；每笔 30 天有效；新邮箱+**邀请码**注册 → 600 Credits（试用 300 + 邀请码加赠 300，**必须先输码再下载**）；夜间 22:00-08:00 错峰折扣（倍率 0.5X→0.2X） | **有，纯 Credits 制** | 邮箱注册（免绑卡）/ Google / GitHub | 官网 Settings > Usage 查余额（**未查到公开 API**） | 有。**CLIProxyAPI 原生支持 `Qoder` OAuth（device flow）** |
| **Qoder CN（原通义灵码）** | lingma.aliyun.com | Alibaba | 社区版免费 **100 credits/月** + 有限补全次数；Pro ¥59/月 2,000 credits | **有**，2026-05-20 起改 Credits 制 | 阿里云主账号 | 有阿里云 OpenAPI | 有 |
| **通义灵码 CodeGeeX** | codegeex.cn | 智谱 AI + 清华 | **云端版完全免费无额度**；开源自托管（CodeGeeX4-ALL-9B，MIT）；底层 300+ 语言 | **无积分系统** | 账号密码 | 有（开放平台） | 有 |
| **文心快码 Comate** | comate.baidu.com | 百度 | 兜底模型 150k tokens/小时 + 新用户 8,000 万 Tokens 包（9 款模型）；测试版限免可领 7-37 天不限量 | **有（tokens 计）** | 百度账号 | 有（千帆平台） | 有 |
| **讯飞 iFlyCode（星火飞码）** | iflycode.xfyun.cn | 科大讯飞 | — | — | 讯飞账号 | **已停服** —— 见下文失效清单 | 有（JetBrains 插件协议，已有逆向文档） |
| **Fitten Code** | — | 非十科技 | 代码补全无限免费；对话约 20 次/天 | **有（次数）** | 账号密码 | 未查到公开接口 | 有 |
| **CodeFun** | — | — | **未查到**（GitHub 上同名项目均为无关的 LeetCode 题解仓库） | 未查到 | 未查到 | 未查到 | 未查到 |

---

## 三、反代项目完整表格

> 说明：star / 更新时间 / 语言 / archived 均为 2026-10-04 通过 `api.github.com/repos/{owner}/{repo}` 实际读取。GitHub API 在调研后段触发了未认证限流（60次/小时），最后几个项目的数据来自 WebSearch 返回的仓库页面。

### 3.1 第一梯队：泛用/多厂商（优先接入）

| 项目 | star | 最近更新 | 语言 | 代理什么 | 接口协议 | 认证方式 | 是否活跃 | 部署难度 |
|---|---|---|---|---|---|---|---|---|
| **[router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI)** | **54,037** | 2026-10-03 | Go | Antigravity / Codex / Claude Code / Grok Build / Kimi / Devin / Muse Code / Qoder / CodeBuddy CN+Intl / Gemini CLI / Vertex / AI Studio | **OpenAI + Responses + Claude Messages + Gemini GenerateContent/Interactions + Codex 原生** | **OAuth 托管**（auth code+PKCE / device flow / web login）+ API Key + OpenAI 兼容上游 | ✅ 活跃（昨天有 commit） | 中：Go 单二进制或 Docker，默认端口 8317，有 Management API |
| **[musistudio/claude-code-router](https://github.com/musistudio/claude-code-router)** | **37,523** | 2026-09-26 | TypeScript | 多后端统一路由：OpenAI/Anthropic/Gemini/OpenRouter/DeepSeek/SiliconFlow/Moonshot/Kimi Code/Mistral/Z.AI/百炼 + OpenCode Zen/Go | OpenAI Chat + Responses、Anthropic Messages、Gemini | API Key（Providers 面板） | ✅ 活跃 | **低**：有 Windows EXE（v3.1.0），也有 Docker；默认 `127.0.0.1:3456` |
| **[QuantumNous/new-api](https://github.com/QuantumNous/new-api)** | 49,237 | 2026-10-01 | Go | 通用 LLM 网关。已有 `ChannelTypeCodex(57)` / `ChannelTypeSub2API(59)` / `ChannelTypeNewAPI(60)` 渠道类型 | OpenAI + Claude + Gemini 互转 | API Key + 渠道管理 | ✅ 活跃 | 中：Docker / 单二进制 + MySQL |
| **[Wei-Shaw/sub2api](https://github.com/Wei-Shaw/sub2api)** | 43,253 | 2026-10-03 | Go | Claude / OpenAI / Gemini / Grok / Antigravity 订阅账号池化 + 用户 Key 分发 | OpenAI 兼容 + 原生 Claude/Gemini | 账号池导入 + 分发 Key | ✅ 活跃（但 **open issues 3,547**，需谨慎评估） | 中：Go + Docker |
| **[lidge-jun/opencodex](https://github.com/lidge-jun/opencodex)** | 16,878 | 2026-10-04 | TypeScript | 通用 provider proxy for Codex & Claude Code → 任意 LLM | OpenAI / Anthropic | API Key | ✅ 活跃（今天有 commit，**但 open issues 117**） | 低 |
| **[justlovemaki/AIClient2API](https://github.com/justlovemaki/AIClient2API)** | 8,839 | 2026-09-30 | JavaScript | Antigravity / Codex / Grok / Kiro / OpenAI / Claude + 自定义 | OpenAI + Claude + Gemini 协议互转 | OAuth / API Key | ✅ 活跃 | 低-中 |
| **[badrisnarayanan/antigravity-claude-proxy](https://github.com/badrisnarayanan/antigravity-claude-proxy)** | 3,993 | 2026-09-06 | JavaScript | Antigravity 的 claude/gemini 模型 → Claude Code | Anthropic | OAuth（Antigravity） | ✅ 活跃 | 低 |
| **[1rgs/claude-code-proxy](https://github.com/1rgs/claude-code-proxy)** | 3,754 | 2026-06-23 | Python | Claude Code → OpenAI 模型 | OpenAI（基于 LiteLLM） | API Key | ⚠️ **3个月未更新**（open issues 70） | 低：pip install |
| **[fuergaosi233/claude-code-proxy](https://github.com/fuergaosi233/claude-code-proxy)** | 2,795 | 2026-03-12 | Python | Claude Code → OpenAI API | OpenAI | API Key | ⚠️ **近7个月未更新** | 低 |
| **[routatic/proxy](https://github.com/routatic/proxy)** | 975 | 2026-10-02 | Go | Claude Code → OpenCode Go / Zen / AWS Bedrock 多上游 | Anthropic | API Key | ✅ 活跃 | 低 |
| **[chainreactors/EvilProxy](https://github.com/chainreactors/EvilProxy)** | 44 | 2026-07-02 | Go | CLIProxyAPI 的 Go 精简 fork（Gemini CLI/Antigravity/Codex/Claude Code/Qwen Code/iFlow） | OpenAI/Gemini/Claude/Codex | OAuth | ⚠️ 3个月未更新 | 低 |

### 3.2 单厂商专项

| 项目 | star | 最近更新 | 语言 | 代理什么 | 接口协议 | 认证方式 | 是否活跃 | 部署难度 |
|---|---|---|---|---|---|---|---|---|
| **[dwgx/WindsurfAPI](https://github.com/dwgx/WindsurfAPI)**（又名 DevinAPI） | 3,058 | 2026-10-03 | JavaScript | Windsurf / Devin Desktop 的 100+ 模型 | **OpenAI Chat + Responses + Anthropic + Gemini 四套** | 会话 token 导入 | ✅ 活跃（零 npm 运行时依赖） | 低 |
| **[icebear0828/codex-proxy](https://github.com/icebear0828/codex-proxy)** | 1,800 | 2026-10-03 | TypeScript | ChatGPT Codex Responses API | OpenAI（含 Responses） | OAuth / ChatGPT 账号 | ✅ 活跃 | 低 |
| **[XxxXTeam/codex-proxy](https://github.com/XxxXTeam/codex-proxy)** | 163 | 2026-10-01 | Go | Codex API | OpenAI + Claude 双协议 | — | ✅ 活跃（中文社区） | 低 |
| **[autumnsentiment/Trae2api-cn](https://github.com/autumnsentiment/Trae2api-cn)** | 31 | **2026-10-01** | Python | Trae CN + Trae Solo CN | OpenAI Chat + Responses（`tools`/`tool_choice`/`parallel_tool_calls`）+ 工具桥接（Windows workspace/Shell/读写/编辑） | **网页 OAuth 登录** + 多账号 round-robin + **checkin 签到** | ✅ **活跃（3天前）** | 低-中：Python 或 Docker（端口 8000），有 `/v1/status` |
| **[connectedGraph/trae2api-web](https://github.com/connectedGraph/trae2api-web)** | 33 | 2026-08-25 | Go | **TRAE SOLO** | OpenAI 兼容 | 账号池 + 自动保活 + Web 面板 | ✅ 活跃 | 低 |
| **[laojichao/trae-local-api](https://github.com/laojichao/trae-local-api)** | 55 | 2026-07-24 | JavaScript | Trae CN | OpenAI + Anthropic | **自动解密 tc 加密 auth** | ✅ 活跃 | 低 |
| **[ZedeX/trae-local-api](https://github.com/ZedeX/trae-local-api)** | 69 | 2026-07-07 | JavaScript | Trae IDE | OpenAI + Anthropic | 本地 storage 解密 | ✅ 活跃 | 低 |
| **[A-23187/trae-api](https://github.com/A-23187/trae-api)** | 173 | 2025-10-08（commit） | Python | Trae 内置模型 | OpenAI | — | ❌ **近1年无 commit**（star 最高但已停更） | 低 |
| **[linguo2625469/workbuddy2api-panel](https://github.com/linguo2625469/workbuddy2api-panel)** | 1,798 | 2026-10-01 | Go | 腾讯 WorkBuddy/CodeBuddy 账号池 | OpenAI | 扫码纳管 + 自动完成积分任务 | ✅ 活跃 | 中：Next.js + FastAPI |
| **[ithtelab/workbuddy-manager](https://github.com/ithtelab/workbuddy-manager)** | 740 | 2026-10-03 | Python | 腾讯 CodeBuddy 账号池 | OpenAI | 扫码 + **定时签到与保活** + 密钥分组分发 + IP/模型白名单 | ✅ 活跃（3天前） | 中：Next.js 15 + FastAPI，**上游 workbuddy2api 源码随发布包分发（MIT）** |
| **[voidsteed/copilot-proxy-api](https://github.com/voidsteed/copilot-proxy-api)** | 48 | 2026-09-25 | TypeScript | GitHub Copilot → Claude Code / Codex CLI | OpenAI Responses + Anthropic Messages | Copilot token | ✅ 活跃 | 低 |
| **[7836246/cursor2api](https://github.com/7836246/cursor2api)** | 1,896 | 2026-06-01 | TypeScript | Cursor Web Docs 免费 API | OpenAI + Anthropic | — | ❌ **4个月无更新** | 低 |
| **[Xchat1/cursor2api-go](https://github.com/Xchat1/cursor2api-go)** | 1,056 | 2026-04-04 | JavaScript | Cursor（gemini-3-flash 免费） | OpenAI | 无头浏览器过 Vercel 验证 | ❌ **已死** —— README 自己写着「❌ 这次倒下啦」，最后 commit 是「回退无头浏览器方案，Vercel 安全验证可检测 headless Chrome」 | — |
| **[FakeOAI/tokens](https://github.com/FakeOAI/tokens)** | 432 | 2026-10-03 | Shell | 号池调度，把国外 AI 平台模型转 OpenAI/Anthropic/Gemini | 三套标准格式 | 号池 | ✅ 活跃 | 低（Shell） |
| **[xllm-go/bypass](https://github.com/xllm-go/bypass)** | 1,245 | 2026-06-24 | Go | openai-api / coze / deepseek / **cursor** / **windsurf** / **qodo** / blackbox / you / grok / bing 绘画 | OpenAI | cookie | ⚠️ 3个月未更新 | 低 |
| **[greenSheep999/cursor-proto](https://github.com/greenSheep999/cursor-proto)** | 3 | 2026-09-12 | Go | Cursor 3.10 逆向客户端 | OpenAI + Anthropic | 逆向协议 | ⚠️ 实验性，star 极低 | 高（需逆向） |

### 3.3 账号管理 / 配额监控 / 签到类（与你的面板最互补）

| 项目 | star | 最近更新 | 语言 | 覆盖范围 | 关键能力 | 是否活跃 |
|---|---|---|---|---|---|---|
| **[jlcodes99/cockpit-tools](https://github.com/jlcodes99/cockpit-tools)** | **18,612** | 2026-10-01 | Rust | Antigravity / Codex / Copilot / Windsurf / Kiro / Cursor / Grok CLI / CodeBuddy / CodeBuddy CN / Qoder / Trae / TRAE SOLO / Trae CN / TRAE SOLO CN / Zed / ZCode（**16 个平台**） | 一键切号、配额监控（自动刷新间隔可配 2-10 分钟）、**配额重置唤醒任务**、应用多开、插件联动、**内置 API 服务（集成 CLIProxyAPI）**、Codex 独立代理（Mihomo 内核） | ✅ 活跃（v1.3.65） |
| **[nguyenphutrong/quotio](https://github.com/nguyenphutrong/quotio)** | 4,878 | 2026-10-03 | Swift | Claude / Gemini / OpenAI / Qwen / Antigravity | 实时配额 + 自动故障转移 | ✅ 活跃，但 **仅 macOS** |
| **[cubezhao/ai-tools-mng](https://github.com/cubezhao/ai-tools-mng)** | 1,179 | 2026-09-10 | Rust | Augment / Antigravity / Windsurf / Cursor / OpenAI(Codex+API) / Claude Code + 订阅、书签、**邮箱管理** | Tauri 跨平台（含 Windows） | ✅ 活跃 |
| **[babygoton/WorkDaddy](https://github.com/babygoton/WorkDaddy)** | 1,665 | 2026-10-01 | JavaScript | WorkBuddy / CodeBuddy 桌面端 | **签到猫猫旅行成长计划查询**、多账号独立备份、点切即用、跨账号会话迁移 | ✅ 活跃 |
| **[changexbc/workbuddy-switch](https://github.com/changexbc/workbuddy-switch)** | 988 | 2026-10-03 | Rust | WorkBuddy / CodeBuddy | 账号切换、**积分到期监控**、积分统计、Token 统计、agent 状态悬浮窗 | ✅ 活跃 |
| **[linguo2625469/workbuddy2api-panel](https://github.com/linguo2625469/workbuddy2api-panel)** | 1,798 | 2026-10-01 | Go | WorkBuddy/CodeBuddy | 见 3.2 | ✅ 活跃 |
| **[qixing-jk/all-api-hub](https://github.com/qixing-jk/all-api-hub)** | 4,909 | 2026-10-03 | TypeScript | New-API / Sub2API 中转站 | 余额用量看板、**自动签到**、一键导 key、价格对比、健康检测 | ✅ 活跃 |
| **[cita-777/metapi](https://github.com/cita-777/metapi)** | 3,300 | 2026-09-06 | TypeScript | New API / One API / OneHub / DoneHub / Veloera / AnyRouter / Sub2API | 汇聚成一个 Key + 一个入口，自动发现模型、智能路由、成本最优 | ✅ 活跃 |
| **[slkiser/opencode-quota](https://github.com/slkiser/opencode-quota)** | 990 | 2026-10-03 | TypeScript | OpenCode Go / Cursor / GitHub Copilot | 配额与 token 用量，**零上下文窗口污染** | ✅ 活跃 |
| **[liaohch3/claude-tap](https://github.com/liaohch3/claude-tap)** | 3,259 | 2026-09-22 | Python | Claude Code / Codex CLI / Gemini CLI / **Cursor CLI** / OpenCode / Kimi / Pi / Hermes | 拦截并**检视** Coding Agent 流量，本地 trace viewer。**对你逆向/对接调试极有价值** | ✅ 活跃 |
| **[modelbus/one-api-pro](https://github.com/modelbus/one-api-pro)** | 1,001 | 2026-10-03 | Go | one-api 的企业级增强分支 | — | ✅ 活跃 |
| **[0xtbug/zero-limit](https://github.com/0xtbug/zero-limit)** | 234 | 2026-02-27 | TypeScript | CLIProxyAPI 跨平台 GUI | 配额监控 | ⚠️ 7个月未更新 |

### 3.4 部署难度速查

| 难度 | 项目 |
|---|---|
| **极低（单文件 / 桌面端）** | claude-code-router（有 Windows EXE）、WindsurfAPI（零 npm 依赖）、opencodex、antigravity-claude-proxy、1rgs/claude-code-proxy、fuergaosi233/claude-code-proxy、cockpit-tools（Rust 单文件 + 官方安装包）、quotio（macOS） |
| **低（Go/Node 单二进制 + Docker）** | CLIProxyAPI、sub2api、new-api、codex-proxy、trae2api-web |
| **中（需数据库 / 多组件）** | new-api（MySQL）、sub2api、workbuddy-manager（Next.js + FastAPI）、workbuddy2api-panel |
| **高（逆向协议，脆弱）** | cursor-proto、cursor2api 系列（依赖无头浏览器过验证，随时会被官方风控干掉） |

---

## 四、已失效 / 不推荐清单

| 项目 | 状态 | 失效原因（证据） |
|---|---|---|
| **讯飞 iFlyCode / 星火飞码** | ❌ **上游停服** | `vibe-coding-labs/iflycode-2api` 的 README 明确写着「**已归档 — iFlyCode 上游已停服，项目已无法使用**」。该组织仍保留协议逆向文档（`iflycodeReverseEngineering`）可供研究。 |
| **Roo Code** | ❌ **2026-05-15 关停，repo archived** | 社区 fork 为 ZooCode。 |
| **Continue** | ⚠️ **停止维护** | 2026-06 发布 Final 2.0.0 后 repo 转只读。 |
| **Amazon Q Developer** | ❌ **停止新免费注册** | 2026-05-15 起官方引导用户转 Kiro。 |
| **Gemini CLI 免费层** | ❌ **2026-06-18 终止** | 停止服务免费用户与 AI Pro/Ultra 用户，免费用户被导向 Antigravity CLI，改为付费 API Key / Code Assist Standard。 |
| **Qwen Code OAuth 免费层** | ❌ **2026-04-15 结束** | 免费 tier 终止，现仅走 DashScope 付费。 |
| **[Xchat1/cursor2api-go](https://github.com/Xchat1/cursor2api-go)** | ❌ **已死** | 1,056★ 但 README 自己写「cursor不倒我不倒🙏 ❌ 这次倒下啦」。最后 commit 2026-04-04：「回退无头浏览器方案，Vercel 安全验证可检测 headless Chrome」——官方风控已能识别 headless。 |
| **[A-23187/trae-api](https://github.com/A-23187/trae-api)** | ❌ **停更近1年** | 173★ 但最后 commit 停在 2025-10-08。同期活跃的替代品是 `autumnsentiment/Trae2api-cn`。 |
| **[linqiu919/trae2api](https://github.com/linqiu919/trae2api)** | ❌ **archived** | Go，39★，2025-06-27 后停止。 |
| **[mt-altman/cursor2api](https://github.com/mt-altman/cursor2api)** | ❌ **archived** | 56★，2024-12-23 停止。 |
| **[frieser/antigravity-proxy](https://github.com/frieser/antigravity-proxy)** | ❌ **archived** | 78★，TypeScript，2026-06-19 停止。 |
| **[elad12390/antigravity-proxy](https://github.com/elad12390/antigravity-proxy)** | ❌ **archived** | 57★，Python，2025-12-15 停止。 |
| **[arch3rPro/Trae-Proxy](https://github.com/arch3rPro/Trae-Proxy)** | ❌ **停更1年+** | 193★，Python，最后 commit 2025-08-12（与首次提交同日）。 |
| **[Sliverkiss/workbuddy2api](https://github.com/Sliverkiss/workbuddy2api)** | ⚠️ **上游 404** | 原仓库已不可访问（私有或删除）。**但 fork 存活**：`linguo2625469/workbuddy2api-panel`（1,798★）与 `ithtelab/workbuddy-manager`（740★，源码随发布包分发）。 |
| **[cbwinslow/CLIProxyAPI](https://github.com/cbwinslow/CLIProxyAPI)** | ⚠️ **0 star fork** | 最后 push 2025-12-11，是 luispater 原版的早期 fork，无价值。上游应直接用 `router-for-me`。 |
| **[luispater/CLIProxyAPI](https://github.com/luispater/CLIProxyAPI)** | ⚠️ **已 404** | 搜索结果中大量镜像站引用它，但仓库本身已不可访问。 |
| **one-api（songquanpeng）** | ⚠️ **事实停止维护** | 37,072★ 但最后 commit 2026-01-09（`.github` 目录），README 无停更声明。**不是 archived**，但实质停滞。**建议**：用 `QuantumNous/new-api`（活跃，49,237★）或企业向的 `modelbus/one-api-pro`（1,001★，2026-10-03）替代。 |
| **[1rgs/claude-code-proxy](https://github.com/1rgs/claude-code-proxy)** / **[fuergaosi233/claude-code-proxy](https://github.com/fuergaosi233/claude-code-proxy)** | ⚠️ **不活跃** | 分别 3.7k/2.8k★，但最后 commit 分别是 2026-06-23 与 2026-03-12，且 open issues 分别 70 / 23。功能已被 CLIProxyAPI 完全覆盖。 |
| **M365 Copilot 类** | ⚠️ 不适用 | `kuchris/m365-copilot-openai-proxy` 等为企业内部 Copilot 代理，与免费额度自动化无关。 |

---

## 五、待确认事项（本次未能查实）

1. **各 IDE 的官方额度/用量 API 端点** —— 除 OpenCode Zen（`opencode.ai/zen`）与 v0 企业 API 外，Cursor / Windsurf / Kiro / Augment / Qoder / Comate / CodeGeeX **均未查到官方公开的模型列表或用量查询接口**。这类数据只能通过抓包或参考 `liaohch3/claude-tap`、`jlcodes99/cockpit-tools` 的逆向实现来定位。
2. **Qoder 每日签到的自动化可行性** —— 官方文档（docs.qoder.com/events/100credits）明确「**仅限 Qoder 桌面端**」且「**需主动领取**」，但未公布 API 端点。CLIProxyAPI 支持 Qoder OAuth（device flow），是否顺带支持触发签到**未查到说明**。
3. **CLIProxyAPI 是否覆盖 Trae** —— 我实际读到的主线 README 里，`ufec/CLIProxyAPI` fork 明确列出 `CodeBuddy CN` / `CodeBuddy Intl`，但**未列出 Trae**。而 CLIProxyAPI 生态的 Quotio 移植版描述里提到管理池包含「Cursor, Trae, GLM」——Trae 支持可能来自第三方移植而非主线，需实测确认。
4. **CodeBuddy 签到任务的具体接口** —— `workbuddy2api-panel` 声称「自动完成任务中心全部任务」，但具体端点/签名算法需读源码确认。你已有 WorkBuddy 网关，可直接对照。
5. **Augment 的 30,000 credits 试用是否已取消绑卡要求** —— 多个二手评测（cursor-alternatives.com，2026-05）称需绑卡，未找到官方页面确认。
6. **Roo Code 的 ZooCode fork 是否值得用** —— 未查证其 star / 维护状态。
7. **`sub2api` 的 3,547 open issues** —— 是活跃开发导致的正常积压，还是架构性债务，未评估。
8. **Gitee 上的国产反代** —— 本次全部走 GitHub API，Gitee 侧未系统检索（国产项目在 Gitee 上可能有独立仓库）。
9. **本地 `nguyenphutrong/quotio` 的 Windows 移植版成熟度** —— `xiaocoss/quotio-desktop`（Rust, 59★, 2026-08-28）规模较小，未深入验证。
10. **Trae 反代的协议稳定性** —— `Trae2api-cn` 的 README 明确「依赖 Trae 上游协议和认证方式，账号、令牌、风控策略变化都可能影响可用性」。你已有 Trae 官方接入，反代作为冗余通道时需接受这条风险。

---

## 附录：本次访问过的 URL（节选）

**反代/网关项目**
- https://github.com/router-for-me/CLIProxyAPI（README via API + WebFetch）
- https://github.com/musistudio/claude-code-router（WebFetch）
- https://github.com/songquanpeng/one-api（WebFetch）
- https://github.com/QuantumNous/new-api
- https://github.com/jlcodes99/cockpit-tools（WebFetch）
- https://github.com/dwgx/WindsurfAPI
- https://github.com/justlovemaki/AIClient2API
- https://github.com/lidge-jun/opencodex
- https://github.com/Wei-Shaw/sub2api
- https://github.com/icebear0828/codex-proxy
- https://github.com/1rgs/claude-code-proxy
- https://github.com/fuergaosi233/claude-code-proxy
- https://github.com/badrisnarayanan/antigravity-claude-proxy
- https://github.com/autumnsentiment/Trae2api-cn
- https://github.com/connectedGraph/trae2api-web
- https://github.com/laojichao/trae-local-api
- https://github.com/ZedeX/trae-local-api
- https://github.com/A-23187/trae-api
- https://github.com/linguo2625469/workbuddy2api-panel
- https://github.com/ithtelab/workbuddy-manager
- https://github.com/voidsteed/copilot-proxy-api
- https://github.com/7836246/cursor2api
- https://github.com/Xchat1/cursor2api-go
- https://github.com/nguyenphutrong/quotio
- https://github.com/xllm-go/bypass
- https://github.com/cubezhao/ai-tools-mng
- https://github.com/qixing-jk/all-api-hub
- https://github.com/cita-777/metapi
- https://github.com/liaohch3/claude-tap
- https://github.com/babygoton/WorkDaddy
- https://github.com/vibe-coding-labs/iflycode-2api
- https://github.com/ufec/CLIProxyAPI（fork，README 列出 CodeBuddy/Qoder OAuth）

**官方文档 / 定价**
- https://kiro.dev/pricing
- https://docs.qoder.com/events/100credits（中文版 /events/100credits 同）
- https://cn.aliyun.com/notice/118264（Qoder CN 计费调整公告）
- https://socket.dev/npm/package/@casually/dsh-trae-api
- https://npmjs.com/package/dsh-trae-api
- https://deepwiki.com/QuantumNous/new-api/4-ai-provider-integrations
- https://toolradar.com/tools/github-copilot/pricing
- https://www.thetechbasket.com/free-ai-coding-agents
- https://aitoolsofficial.com/?p=1077（Windsurf）
- https://cursor-alternatives.com/blog/augment-code-faq（Augment）