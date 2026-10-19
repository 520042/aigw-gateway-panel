# 重构调研 · 2api 项目全集（26 仓库 + 本地项目盘点）

> 2026-10-16 调研。用户指令：拉取解析全部参考项目 + 本地项目，完整记录；提取前端/后端/算法，作为「全量 Rust 重构」依据。
> 方法：4 路并行调研子代理逐仓库抓取 README/文档并提炼；个别仓库 README 未暴露细节的标【待补】。

## 状态图例
★=作为 Rust 重写的协议基线 / 架构蓝本；✅=维护活跃；⚠=停更/单次提交（仅作算法参考）；【待补】=README 未暴露，需后续抓源码。

---

## 一、框架与 UI 标杆（A 组）

### Ebony-Vinyl/dsh-our-free-model ★✅
- 栈：JavaScript（零依赖、无构建）；dsh 插件 + 独立本地服务 + OpenAI/Anthropic 兼容网关
- 核心：免密车道（opencode zen `Bearer public`/Kilo isFree 池）；Cloudflare Worker 签名网关（时间戳+HMAC-SHA256 防重放）；EAC 凭据 AES-256-GCM 分片加密；三种线协议（OpenAI Chat/Responses、Anthropic Messages）按 body 形状嗅探归一为 StreamChunk；76 家提供商注册表；Ed25519 签名清单+原子替换升级；14 渠道白嫖池；combo 虚拟模型降级链；RTK 压缩 tool_result
- 借鉴：①按 body 形状而非 Content-Type 判流并回注已读字节；②Ed25519 pin 公钥的签名清单升级链；③流转发安全细节（timing-safe 比较、loopback 绑定、hop-marker 环路保护）
- 风险：免密车道依赖第三方网关政策，随时可能失效；迭代极活跃（v2.3.0，日更）

### xiaofan6ya/workbuddy2api ★✅
- 栈：Python（FastAPI+SQLAlchemy+Redis+MySQL）+ Node 辅助 + pywebview 桌面端/安卓 APK；服务+运营后台+安装器三合一
- 核心：读取桌面端 `.info` JWT 并自动刷新；注入 X-User-Id/X-Tenant-Id 及 Turing Shield SDK 风控头 `X-Device-Token`；三协议端点（Chat SSE/Responses/Anthropic）；积分到期紧迫度优先的多策略选号、会话粘性、错误分类熔断退避、WAF fail-fast；每日签到/成长计划任务自动化；纯 Python asar 解包器
- 借鉴：①号池调度全套策略（租约/粘性/冷却/退避/错误分类）；②设备风控 token 的进程内缓存与降级；③Responses 事件流合规细节（item_id、事件白名单、终止帧补发）
- 风险：强依赖腾讯桌面端协议与风控 SDK 版本，客户端更新即需跟进；维护极活跃（v1.0.2）

### CherryHQ/cherry-studio ✅
- 栈：TypeScript + Electron 桌面端；pnpm monorepo（aiCore/provider-registry 包）；better-sqlite3+Drizzle；AGPL-3.0
- 核心：300+ 模型提供商接入（provider-registry 统一管理）；MCP Server；本地模型（Ollama/LM Studio）；知识库/OCR/TTS/ASR（Sherpa-ONNX）；HTTP/SOCKS5 代理+PAC；多模型同聊；企业版私有部署
- 借鉴：①provider-registry 的统一提供商抽象与模型目录设计；②代理配置体系（认证/PAC/bypass/自定义 fetch 透传）；③桌面端工程化
- 风险：终端客户端而非网关，形态差异大；生态标杆（9600+ commits）。本地已有 `Downloads/cherry-studio.*.Admin.windows` 安装包

### seakee/CPA-Manager-Plus ✅【待补实现语言】
- 栈：文档站（VitePress）；自托管 CLIProxyAPI 管理面板+可观测性工具，Docker/原生包部署（本地已有 `cli-proxy/`、`cliproxy-src/` 克隆）
- 核心：网关管理、请求历史持久化与失败定位、Token/成本/延迟/调用方分析、Codex/Claude/xAI 账号配额健康巡检；轻量面板（零新增服务/端口）与完整模式两种形态；备份恢复与按症状排障文档
- 借鉴：①请求持久化→成本归因的可观测性数据模型；②账号配额/健康巡检的运维视角；③「轻量替换面板 vs 完整模式」分层部署思路
- 风险：文档首页未暴露实现语言与协议细节【待补】，需查子页面或本地 cliproxy-src 源码

### Wei-Shaw/sub2api ✅
- 栈：Go 1.27（Gin+Ent+Wire）+ Vue3 + PostgreSQL/Redis；完整 SaaS 网关服务，LGPL-3.0
- 核心：OAuth 订阅账号（xAI PKCE）+API Key 混合号池；粘性会话+并发限制（Redis 分布式租约）+熔断 failover；Anthropic↔OpenAI↔Responses 多协议转换；OpenAI Responses WebSocket 入口桥接上游 SSE；复合组路由；Token 级计费+支付；一键脚本/Docker/在线升级安装器
- 借鉴：①Go 网关整体架构（Ent+Wire 依赖注入、embed 前端单二进制）最接近 Rust 重写蓝本；②WS 入口桥接 HTTP/SSE 的 http_bridge 模式；③Simple 模式跳过计费的开关设计
- 风险：LGPL-3.0 且禁止商业运营——**只参考机制，不复制代码**

### xiaoY233/Chat2API ⚠【待补】
- 栈：Electron + React + TypeScript + Koa 桌面端（README 渲染信息有限）
- 核心：利用 DeepSeek/GLM/Kimi/MiniMax/Qwen/Z.ai 官方网页 UI 零成本调用，转 OpenAI 兼容接口【待补：账号池与协议转换细节】
- 借鉴：①官方 Web UI 逆向会话接入思路；②Electron+Koa 桌面壳+本地服务组合形态
- 风险：逆向网页协议极易随官方改版失效，时效性风险最高

### justlovemaki/AIClient2API ★✅
- 栈：Node.js ≥20 服务（Web UI 控制台）+ Go TLS Sidecar（uTLS）；Docker；GPLv3
- 核心：Antigravity/Codex/Grok/Kiro 等 OAuth 流（各自回调端口）+Cookie/SSO；OpenAI/Claude/Gemini 三协议策略模式互转；ProviderPoolManager 账号池（轮询/异步刷新/429 冷却/健康检查）；跨类型 fallback 链；Go uTLS 模拟 Chrome JA3/JA4 指纹绕 Cloudflare 403；PBKDF2 管理员密码；跨平台安装脚本+自动更新
- 借鉴：①策略+适配器的多协议互转架构；②Go uTLS TLS 指纹 Sidecar 思路（Rust 用 rquest/rustls+自定义指纹实现）；③fallback 链仅在协议兼容类型间降级的约束设计
- 风险：OAuth 端点与风控随官方 CLI 更新漂移；维护极活跃（v3.5.8）

---

## 二、GLM / CodeBuddy / 华为 / Qoder 系（B 组）

### lzA6/GLM-2api ✅
- 栈：Python/FastAPI+Granian，API 服务（Z.AI→OpenAI）
- 核心：token 池 Round-Robin+失败计数停用/恢复；匿名模式自动取临时 token；动态 UA；SSE 解析器+SSEToolHandler 状态机（正则+JSON 增量）拼装工具调用；Nginx 多实例负载
- 借鉴：工具调用流式状态机；token 健康度管理；多实例方案
- 风险：寄生 Z.AI 非公开 API，协议一变即失效

### Sark1tama/glm2api ★✅
- 栈：Python 3.14+/uv，本地代理服务（ChatGLM 网页端→OpenAI/Responses/Anthropic 三方言）
- 核心：refresh_token 换 access_token 自动续期并写回；游客模式并发槽位；DSML 工具协议（文本桥接序列化/解析）；本地流式匹配 stop 序列；输出 token 预算；usage 保守估算
- 借鉴：DSML 工具块解析与截断；统一内部模型+三协议适配器分层；上游忙碌重试与并发槽
- 风险：网页协议非官方，字段变动需同步适配；活跃（2026-09 更新）

### WR-Class/glm2api ⚠
- 栈：Python/FastAPI，代理服务（chatglm.cn→OpenAI）
- 核心：浏览器 LocalStorage 取 chatglm_refresh_token；access_token 过期自动刷新；token.txt 多账号失败自动轮换；模型名自动扩展 -think/-search 变体（78 模型）；SSE 流式转 OpenAI delta
- 借鉴：模型变体自动映射（-think/-search 后缀路由）；busy 重试参数化（30 次/2s）；refresh_token 写回持久化
- 风险：签名细节藏在源码未披露；2026-08 后停更

### zhaoyuan020/codebuddy2api ⚠
- 栈：Python/FastAPI+前端 Web UI，API 服务（CodeBuddy 官方 API→OpenAI）
- 核心：OAuth2 自动认证路由（Web UI 生成登录链接自动抓凭证）；Bearer Token 凭证目录轮换（每 N 次请求切换）；上游仅流式，代理层包装非流式
- 借鉴：OAuth 设备码式自动取凭证流；流转非流包装；凭证统计/Web 管理面板
- 风险：依赖 CodeBuddy 官方端点；2025-09 后停更，协议可能过期

### shuishuipingan/qoder2api-hub ★✅
- 栈：纯 Python 标准库零依赖（自实现 AES/RSA/GCM/DPAPI），网关服务+Web 看板
- 核心：COSY 签名=RSA 包 AES 会话密钥+MD5 请求签名+自定义 Base64 体；签名只覆盖 path（可换主机）；DPAPI/AES 解密本地凭证；OAuth 设备流 PKCE S256；UID 哈希派生设备指纹防风控；SSE 心跳 `:ping`；双区账号池冷却调度
- 借鉴：COSY 签名算法完整参考；设备指纹稳定派生（identity=f(种子,DMI,容器证据)）；账号池冷却+签到调度器；零依赖思路对 Rust 移植友好
- 风险：最活跃（2026-10-15 更新，1028 测试），但绑定桌面端版本号 1.1.64

### Johnsheng1/codearts2api ⚠
- 栈：Python 单文件服务（server.py+benefit.py），代理（华为云 CodeArts→OpenAI）
- 核心：华为云 SDK-HMAC-SHA256 签名（AK/SK）；benefit.py 签到领免费额度；5 并发限制需客户端侧控制
- 借鉴：HMAC-SHA256 请求签名实现参考；签到任务保活额度模式
- 风险：极简两文件项目，AK/SK 权限过高；2026-09 后基本静止

### xiaoyutx94/2api_python ⚠
- 栈：Python/FastAPI+HTTPX，多提供商代理服务（Z.AI/K2Think/LongCat）
- 核心：Z.AI token 池按 role 字段验证（user 收/guest 拒）；匿名回退单向；临时 token 匿名模式；token 池 HTTP 管理 API（status/health-check/update）；模型→上游 ID 映射（0727-360B-API）
- 借鉴：token 池动态更新/去重/类型校验 API；多 Provider Router 分层；auth 失败回退匿名策略
- 风险：Z.AI 主仓 fork，2025-09 后停更，协议老旧

**GLM 三实现对比**：lzA6 走 Z.AI 官方 API 伪装（token 池最强）；Sark1tama 走 ChatGLM 网页端（DSML 工具协议+三方言最完整、最活跃）；WR-Class 同源但偏 token 轮换与模型变体。**Rust 重写以 Sark1tama 分层架构（api/adapters→core→glm/client）为蓝本，叠加 qoder 的签名/指纹细节。**

---

## 三、Trae / Lobster / Cline / Loomy 系（C 组）

### JeffHu0912/trae2api ⚠
- 栈：Go 1.22 纯标准库，反代服务（Docker）
- 核心：Web 登录闭环+OAuth 本地回调（18080）换 Token；多账号池按剩余积分降序调度；加权防惊群、inFlight 租约；1005/429/401/5xx 分类冷却轮转；每日自动签到+Token 过期前 24h 预刷新原子落盘；含 frida 目录（客户端 Hook 取凭证）；支持 Work 通道私有协议
- 借鉴：①错误阈值/冷却参数化（threshold=3, cooldown=300s）②Web 登录回调免抓包 ③防惊群加权选号
- 风险：2026-09 单次提交仓库，无迭代

### wangqi233/trae2api ★✅
- 栈：Node.js 22.5+（内置 node:sqlite 持久化），本地网关服务
- 核心：双区域（cn/sg）同池，按模型名自动选区；自动扫描本机 Trae 客户端数据目录热导入凭证（storage.json）；OpenAI+Anthropic 双协议含 SSE/tools/thinking；`ExchangeToken` 会轮换令牌族导致其他客户端掉线，故默认关闭自刷新；reasoning 前缀注入 `think_effort`；自动续写截断输出
- 借鉴：①`docs/PROTOCOL.md` 协议实测文档（错误码语义/区域差异/两套模型目录）②零配置收池（扫客户端目录）③「自刷新会踢客户端」关键坑
- 风险：2026-09 单版本移植仓库，待观察

### autumnsentiment/Trae2api-cn ✅
- 栈：未明示（配置项极多，推断 Python/Go 服务），Trae CN 中继
- 核心：五种认证源（auto 解密 storage.json / env / 抓包 JWT / CLI 子进程 / web-login）；上游多模式（solo/ide/work-agent/cli/web/remote）带回退链；签到带 9074 业务码指数退避+错峰重试、per-account 设备 ID 映射；历史消息/工具 schema 字符预算裁剪省积分；自动路由（带 tools 走 IDE Agent，否则 Remote）
- 借鉴：①多认证源抽象 ②输入预算裁剪降积分消耗 ③签到退避矩阵（9074 专用）
- 风险：持续更新（2026-10 仍发版 v1.0.5），三者中最新但配置复杂度爆炸

### xinxinshuhao-create/lobsterai2api ★✅（本地已有克隆）
- 栈：Go 纯标准库，OpenAI 兼容桥接服务（网易有道 LobsterAI）
- 核心：OAuth 本地回调登录；JWT 过期解析+到期前 10min 主动刷新；按剩余积分选号；错误分类精细（硬性积分耗尽冷却 12h、429 软冷却 60s、连续错误 3 次冷却 10min、刷新被拒禁用）；请求内最多换号 3 次；200 包裹错误帧的归类修复；上游域名全部环境变量化
- 借鉴：①错误分类枚举→差异化冷却时长映射（**最成熟的参考**）②mid-stream SSE 错误无法改状态码的已知限制记录 ③动态模型列表缓存 1h+静态回退
- 风险：2026-10 活跃，质量高，但上游域名需自备

### luawei1/cline2api ★✅
- 栈：Go 1.25 + Wails v2 桌面端（WebView2/WebKit），服务+桌面双形态
- 核心：OAuth 设备授权+WorkOS 自愈刷新链（refresh 400→WorkOS authenticate→回 /auth/register）；双协议+`/v1/responses`；多策略轮询（round_robin/fill/random）；uTLS Chrome 指纹+HTTP/2+出口代理池；429/5xx 按 provider 5min 冷却、402 账号级 30min；请求级 fallback 链；JSON 原子写+损坏备份；上下文超限自动摘要压缩
- 借鉴：①WorkOS 令牌刷新自愈链（唯一处理此类失败的实现）②uTLS 指纹伪装 ③fallback 链与熔断组合——**功能最全、最值得整体抄**
- 风险：2026-10-12 仍密集提交+响应 issue/PR，维护最佳

### Patrick130306/loomy2api ⚠
- 栈：Python 3.9 纯标准库，网关服务（讯飞 Loomy）
- 核心：服务端手机号+密码登录（RSA，rcode 为服务器 nonce 非 CAPTCHA）+HMAC-SHA1 签名（客户端内置 AK）；14 天会话后台线程自动续期（余 3 天触发）；per-account 设备身份绑定（devid+promotions id）；balance/round_robin/lru 三策略；仅 401/403/402/429 算账号故障，5xx 不冷却；Electron 客户端 `useSessionAuth:true` 无 API key、session 即 bearer；`docs/PROTOCOL.md` 完整协议逆向
- 借鉴：①PROTOCOL.md 的签名串/错误指纹/配额台账写法 ②「5xx 不怪账号」的故障归因原则 ③密闭测试（fake upstream，115 个测试）
- 风险：2026-09-30 后停滞，工程纪律最好的 Python 版

### xingluoyuankong/loomy2api ★✅
- 栈：Python 标准库，网关服务+Web 面板（讯飞星火 Loomy）
- 核心：面板内短信/微信扫码/密码登录全自动入池；weighted 轮询+Top-N+粘性会话（session_key 取最后一条 user 消息修复永久粘死）+429 有界指数退避+熔断；防风控逐字节对齐官方客户端（devid/ua/traceid）+批量随机抖动+per-account 出口代理；非流式长任务 504→内部转流式再聚合；用量台账 JSONL 落盘+五档统计窗口；计费逆向实测文档
- 借鉴：①粘性会话键派生 bug 的教训（勿用首条 user）②防风控对齐清单 ③流式聚合解 504——防风控与运维细节比 Patrick 版丰富
- 风险：2026-10 批次更新，活跃，但私有 fork 演进分叉

**横向结论**：Trae 以 **wangqi 的 PROTOCOL.md 为协议基线、JeffHu 的池调度为架构蓝本、autumn 的多认证源为输入层**；Loomy 以 **xingluoyuankong 版为准**（防风控+面板+长任务全面），Patrick 版补测试纪律与故障归因原则；**cline2api（luawei1）是整体工程质量天花板**（WorkOS 自愈+uTLS 指纹），作账号池/网关骨架参考实现。

---

## 四、豆包 / Grok / 元宝 / ZCode 系（D 组）

### wangchuxiaoji-oss/doubao2api ✅
- 栈：Python 3.10+/FastAPI，库+服务双形态，Playwright(Chromium) 持久化浏览器，单 Session 无账号池
- 核心：QR 扫码登录获取 sessionid/ttwid；借页面内 fetch hook 自动注入 a_bogus+msToken 绕过签名；SSE 增量 delta（event_type 2001 补全/2002 ACK/2003 FIN，block_type 10040 分隔 thinking）；文件上传走 AWS SigV4+frontierSign+CRC32；验证码错误码 710022004，无 POW
- 借鉴：浏览器内 fetch 桥接 SSE 可解 a_bogus 难题（对应本项目 cdp.rs 9333 桥）；SSE 事件状态机（ACK/FIN/ERR）；Upload 走 ImageX SigV4 流程
- 风险：依赖浏览器指纹与前端 JS hook，豆包前端改版即失效

### chenyme/grok2api ✅
- 栈：Go 1.26 / React 19 控制台 / 服务型网关，SQLite/Postgres + Redis，Docker amd64/arm64
- 核心：三渠道 Provider（Build 走 OAuth Device + refresh token 轮换、Web/Console 走 SSO token）；无 ssig/x-gorgon 签名；OpenAI+Anthropic 双协议 SSE 增量流，Build 侧翻译为 Responses 协议保留 reasoning/工具；独立账号池按凭据/配额/健康隔离，≥3000 账号分段选择器；无 POW 但有 Egress Quality Guard 主动探测+流内无 reasoning 检测重放（≤6 次，fail_closed 503）
- 借鉴：Provider 级账号池隔离+粘性会话路由；质量守卫 sidecar 探测降级出口；credentialEncryptionKey 加密存储
- 风险：依赖 Grok 非官方渠道 OAuth/token，维护活跃（1600+ 提交）但渠道随时可变

### VincentZyu233/TecentHelp2Api ✅
- 栈：Node.js（proxy.js 单文件）/ 轻量 HTTP 服务，默认端口 60809，Docker/systemd
- 核心：代理腾讯元宝 SSE 端点 /kfbackend/api/getYuanBaoAnswer；默认不鉴权；按设备 IP+X-Session-Id 固定元宝 userid 实现多轮会话，30 分钟过期；query 超 6000 字符按 encodeURIComponent 截断防 414（保留首尾、截中间历史）；支持 /v1/responses（codex）与 /v1/chat/completions；README 未见签名算法/账号池/POW
- 借鉴：会话粘定上游 userid 的多轮方案；长 query 截断保护（中文编码膨胀 9 字节/字）；路径遍历与 body 大小防护
- 风险：单文件个人项目（2026-08 仍更新），元宝接口无官方保障

### ops120/zcode2api ✅
- 栈：Node.js 20+ 零依赖 ESM / 本地 HTTP 服务，监听 127.0.0.1:8931
- 核心：读取 ~/.zcode/v2/credentials.json（AES-256-GCM，enc:v1 信封），密钥由「平台+家目录+用户名」本地派生；注入 15 个实测 ZCode 指纹头伪装客户端；OpenAI/Anthropic 双协议四象限互通，同协议逐字节透传（保留 thinking 签名/cache_control），SSE 增量 delta；延迟写 200 头使上游错误可透传；10s SSE ping 保活；绕过 zcode.z.ai 的 Client Request Signing V4（X-Client-Sig/Pow/Ts 未实现），改走 open.bigmodel.cn/api/anthropic；无账号池、无 POW
- 借鉴：延迟 writeHead 技巧（上游错误可按状态码重试）；不伪造 finish_reason 的流终止原则；凭据信封解密思路（若做 ZCode 通道）
- 风险：签名链缺失靠端点替换绕行，上游封堵即失效；单机凭据绑定，换机失效

### dengyie/zcode2api ★✅
- 栈：Python 3.11+/FastAPI+httpx / 服务（AGPL-3.0），SQLite(WAL) 持久化，Node+jsdom 子进程求解验证码
- 核心：JWT/API Key 账号池轮询；每账号独立 UUIDv4 device_mid + 加权桌面 SKU 指纹 + 官方首启安装序仿真（install_id 持久化）；JWT 通道需阿里云无痕验证码，jsdom 免浏览器求解 X-Aliyun-Captcha-Verify-Param（预热池+45s 缓存）；阶梯容灾：429 按 Retry-After 等待不冷却、5xx 熔断 300s、3012/405 风控即 DISABLED、401/403 标 INVALID；单账号并发上限 2；套餐自动领取轮；深度清洗 x-forwarded-*/cf-* 头；OAuth poll_token 轮询
- 借鉴：**完整账号状态机（ACTIVE/EXHAUSTED/COOLING/INVALID/DISABLED）**；指纹与首启仿真反关联设计；验证码子进程+预热池架构
- 风险：AGPL-3.0 许可注意传染性；协议跟随官方 3.11.2，风控对抗持续升级

---

## 五、横向算法提取矩阵（仓库 → 机制 → Rust 重构去向）

| # | 机制 | 来源仓库 | 去向（新架构 crate） |
|---|---|---|---|
| 1 | 账号池全套：租约/粘性/冷却/退避/错误分类/半开 | workbuddy2api + JeffHu/trae + 本项目 S3 沉淀 | `aigw-pool` |
| 2 | 完整账号状态机 ACTIVE/EXHAUSTED/COOLING/INVALID/DISABLED | dengyie/zcode2api | `aigw-pool` |
| 3 | 错误分类→差异化冷却映射（12h/60s/10min/禁用） | lobsterai2api | `aigw-pool` |
| 4 | 「5xx 不怪账号」故障归因原则 | Patrick/loomy2api | `aigw-pool` |
| 5 | 防惊群加权选号 + inFlight 租约 | JeffHu/trae2api | `aigw-pool` |
| 6 | 粘性会话键=最后一条 user 消息（勿用首条） | xingluoyuankong/loomy2api | `aigw-pool` |
| 7 | WorkOS 刷新自愈链（refresh 400→authenticate→register） | luawei1/cline2api | `aigw-relay::auth` |
| 8 | OAuth 设备码/PKCE S256/poll_token | AIClient2API + qoder + dengyie + grok2api | `aigw-relay::auth` |
| 9 | uTLS 浏览器指纹（JA3/JA4）+ HTTP/2 + 出口代理池 | AIClient2API(Go sidecar) + cline2api | `aigw-relay::transport`（rquest） |
| 10 | GLM build_sign 签名（毫秒 ts+校验位+md5） | glm2api 系列（本项目已实测） | `aigw-crypto::sign` |
| 11 | COSY 签名（RSA 包 AES 会话密钥+MD5+自定义 Base64） | qoder2api-hub | `aigw-crypto::sign` |
| 12 | 华为 SDK-HMAC-SHA256（AK/SK） | codearts2api | `aigw-crypto::sign` |
| 13 | HMAC-SHA1（Loomy 客户端内置 AK）+ RSA 登录 | Patrick/loomy2api | `aigw-crypto::sign` |
| 14 | AES-256-GCM 凭据信封（enc:v1，本地派生密钥） | ops120/zcode2api + dsh EAC | `aigw-crypto::envelope` |
| 15 | Ed25519 签名清单+原子替换升级链 | dsh-our-free-model | `aigw-installer`/升级 |
| 16 | 设备指纹稳定派生 f(种子,DMI,容器证据)+首启仿真 | qoder2api-hub + dengyie/zcode2api | `aigw-relay::fingerprint` |
| 17 | 按 body 形状嗅探判流 + StreamChunk 统一中间流 | dsh-our-free-model | `aigw-relay::stream` |
| 18 | SSE 三方言互转（OpenAI/Anthropic/Responses）+ 工具调用流式状态机 | sub2api + AIClient2API + lzA6/GLM-2api | `aigw-relay::protocol` |
| 19 | WS 入口桥接上游 SSE（http_bridge） | sub2api | `aigw-relay::protocol` |
| 20 | 延迟写 200 头（上游错误可按状态码重试）+ 不伪造 finish_reason | ops120/zcode2api | `aigw-relay::stream` |
| 21 | 非流式长任务 504 → 内部转流式再聚合 | xingluoyuankong/loomy2api | `aigw-relay::stream` |
| 22 | DSML 工具协议（文本桥接序列化/解析+截断） | Sark1tama/glm2api | `aigw-relay::tools` |
| 23 | 输入预算裁剪（历史消息/工具 schema 字符预算省积分） | Trae2api-cn | `aigw-relay::budget` |
| 24 | combo 虚拟模型降级链 + 模型变体自动映射（-think/-search） | dsh + WR-Class/glm2api | `aigw-registry` |
| 25 | 声明式 provider 注册表（76 家 schema） | dsh + cherry-studio provider-registry | `aigw-registry`（providers/*.toml） |
| 26 | 请求历史持久化→成本归因、配额健康巡检 | CPA-Manager-Plus | `aigw-store` + 前端可观测页 |
| 27 | 质量守卫（主动探测出口+流内无 reasoning 检测重放） | grok2api | `aigw-pool::quality`（P2） |
| 28 | 验证码子进程求解+预热池 | dengyie/zcode2api | `aigw-relay::captcha`（按需） |
| 29 | 浏览器内 fetch 桥解 a_bogus/msToken | doubao2api | `aigw-relay::cdp`（移植现有 cdp.rs） |
| 30 | 会话粘定上游 userid + 长 query 截断保护 | TecentHelp2Api | `aigw-relay`（元宝 adapter） |
| 31 | Go 网关整体架构（DI、embed 前端单二进制） | sub2api + cline2api | workspace 总体组织 |
| 32 | 零依赖思路（自实现 AES/RSA/GCM/DPAPI） | qoder2api-hub | 依赖精简哲学（能用 RustCrypto 就不拉重依赖） |

---

## 六、本地项目盘点（C:/Users/liang.zhao/Downloads/网关项目/）

| 项目/文件 | 说明 | 重构角色 |
|---|---|---|
| `aigw-panel-rs/` | Rust 版主力（Axum，S1-S6 完成，29 测试，B1 安装/D 备份/C 关于/玻璃 UI 试点） | 算法与经验的来源（pool/credits/超时四项/号池治理全部移植） |
| `aigw-panel/` | Python 版（PyInstaller，42 平台一手协议模块、gen_catalog.py） | **协议一手知识来源**；catalog 生成逻辑 Rust 化（xtask）后归档 |
| `LobsterAI2API/` | xinxinshuhao-create/lobsterai2api 本地克隆 | Lobster 通道协议蓝本（错误分类映射） |
| `cli-proxy/` + `cliproxy-src/` | CLIProxyAPI 本地克隆（CPA-Manager-Plus 生态） | 可观测性数据模型参考 |
| `base*.apk` ×6 | 豆包/Trae/元宝等 APK 逆向素材 | dex/so 协议取证 |
| `workbuddy-gateway-*` | 已移除的 Go EXE 残留 | 不再使用（硬规则：绝不引入） |
| `供应商调研.md`/`全量资源解析与总结.md`/`办公AI调研.md`/`对话平台与工具调用调研.md`/`VibeCoding与反代调研.md`/`项目梳理与重构规划.md`/`工作区文件解析报告.md` | 既有调研文档 | 本次调研的先行材料，重构计划需与之对齐 |
| `_dump_loomy_dex.py`/`_probe*` | loomy dex dump 与探测脚本 | 协议取证工具 |
| `wb-models-cache.json`/`wb-request-log.jsonl` | WorkBuddy 模型缓存与请求日志 | 契约测试 fixture 素材 |

其他本地相关：`Downloads/cherry-studio.*.Admin.windows/`（cherry-studio 安装包）；r3g-build（Padavan 构建，与本项目无关）；X-WRT/clnc/EasyTether（路由器项目，无关）。

---

## 七、维护状态红黑榜（协议基线选择）

**以这些为基线（活跃+质量高）**：
Sark1tama/glm2api、qoder2api-hub、wangqi233/trae2api、Trae2api-cn、lobsterai2api、luawei1/cline2api、xingluoyuankong/loomy2api、dengyie/zcode2api、grok2api、AIClient2API、dsh-our-free-model、workbuddy2api、sub2api、doubao2api、TecentHelp2Api、ops120/zcode2api

**仅作算法参考（停更/单次提交）**：
WR-Class/glm2api(2026-08)、zhaoyuan020/codebuddy2api(2025-09)、Johnsheng1/codearts2api(2026-09)、xiaoyutx94/2api_python(2025-09)、Patrick130306/loomy2api(2026-09)、JeffHu0912/trae2api(2026-09 单次)、xiaoY233/Chat2API(细节【待补】)

**License 注意**：sub2api=LGPL-3.0、cherry-studio=AGPL-3.0、dengyie/zcode2api=AGPL-3.0、AIClient2API=GPLv3 —— 本项目为本地私有使用不分发，法律风险低；纪律：**只参考机制与协议，不复制代码**。
