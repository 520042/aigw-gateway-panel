# 网关项目 · 长期记忆

## 定位
免费 AI 额度 → 本地 OpenAI 端点（`/v1` 暴露 chat/completions、models）的聚合面板。单 EXE 自洽，无外部网关。

**主力 = `aigw2/`（Rust workspace，9 crates + 75 份 `providers/*.toml`）**，自 2026-10-16 起持续演进。
`aigw-panel-rs/`（第一代 Rust）与 `aigw-panel/`（Python 原版）都是历史实现，只作对照，不再加功能。

## 硬规则（被纠正过，必须遵守）
- **UI 只显示「国内站 / 国际站」两行**（WorkBuddy/CodeBuddy 合并卡）。`apk-codebuddy`/`apk-codebuddy-cn` 是 `wb-gateway-intl`/`wb-gateway` 别名，不加 `gwlogin.PLATFORMS` 规格，不进 `api/login?action=platforms`。★ 云端旧摘要「4 行独立」是过期错误记录，2026-10-14 被当面纠正，以本段为准。
- 绝不引入/提及 WorkBuddy Gateway（Go EXE，已彻底移除）。
- 数据目录 = `%LOCALAPPDATA%\aigw2\data\`（aigw2 专用；`aigw-panel-rs\data`、`aigw-panel\data` 都是陈旧位，只在 import_legacy 时读）。
- 安装位 `%LOCALAPPDATA%\aigw2\`（非 Downloads，火绒会拦）。
- aigw2 铁律：**禁扫码 / 禁无头浏览器 / 禁 CDP·Chromium**。登录只允许「跳转链接+手动粘贴回调」「设备码」「API Key」「Cookie 粘贴」四种。

## 构建 / 部署
- 构建（aigw2；workdir 必须是 `aigw2/`）：`$env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH"; $env:CARGO_TARGET_DIR='C:/aigw-target'; cargo build --target x86_64-pc-windows-msvc -p aigw`
- 产物 `C:\aigw-target\x86_64-pc-windows-msvc\debug\aigw.exe`（★ 不是 `C:\aigw-target\debug\aigw.exe`，那是旧版）
- 部署：`taskkill /IM aigw.exe /F` → 拷 exe + `providers\*.toml` 到 `%LOCALAPPDATA%\aigw2\` → `schtasks /Run /TN Aigw2Service`（计划任务 BootTrigger + InteractiveToken，参数 `--port 8800`）
- **改前端 `aigw2/web/app.js` 必须重新 cargo build**（rust-embed 内嵌），再 Ctrl+Shift+R
- 测试：`cargo test --target x86_64-pc-windows-msvc`（2026-10-20 实测 **127 项全绿**：core 3 / crypto 20 / pool 16 / registry 10 / relay 19 / server 38 / store 21）
- 实测接口一律用 node fetch：PowerShell `Invoke-WebRequest` 会把 UTF-8 当 GBK 解，中文全乱码（不是服务端 bug）

## 模块地图（aigw2，当前主力）
| crate / 文件 | 职责 |
|---|---|
| `crates/aigw-core` | `ErrorClass::from_status` / `refine_biz` 错误分类（StreamChunk 仍是死代码） |
| `crates/aigw-registry` | `ProviderSpec` 声明式接入 + `load_dir_lenient` 加载 75 份 toml（坏文件只跳过不炸全表）+ `apply_settings` 解 `@占位符` |
| `crates/aigw-crypto` | 签名：glm X-Sign / codearts SDK-HMAC-SHA256 / loomy / qoder / zcode |
| `crates/aigw-store` | `accounts.rs` 凭据池 / `custom.rs` 自定义 API / `free.rs` 公益站 / `flags.rs` 逐模型·逐平台停用 / `atomic.rs` tmp+fsync+rename / `lib.rs` SQLite usage（DB 不可用降级 JSON 并读回历史） |
| `crates/aigw-pool` | 号池六件套：InFlight 租约(≤2) / (账号,模型)冷却 / WafIpGate(401·403→账号级 30min) / RecentPick / 半开 / forget_account |
| `crates/aigw-relay` | 出站 HTTP + 平台反代：`lib.rs` 通用 OpenAI 透传与流式 `tap_passthrough`；`doubao/yuanbao/kuku/metaso/glm/loomy/codearts` 专用；`auth_login.rs` 全部登录链路；`wb.rs` 每日签到；`proxy.rs` 出站代理 |
| `crates/aigw-server` | axum 路由 + `guard` 门禁 + `AppState`（`spec_for_model`/`resolve_spec`、模型缓存、备份、usage API、rust-embed 前端） |
| `crates/aigw` | bin：CLI（host/port/token/providers/install/purge）+ 启动装配 + `warm_models` 预热 |
| `web/app.js` + `index.html` + `style.css` | 单页前端（5 视图：总览/接入源/账号/用量/关于），rust-embed 内嵌 |
| `providers/*.toml` | 75 份平台规格（free-* 公益站 / api-* / apk-* / sub-* / web-* 等） |

### aigw2 关键机制（改代码前必读）
- **门禁** `guard`：Host 白名单 → Origin/Referer 白名单 → `/api/*` 令牌（`/v1/*` 不校验，SDK 直连）。回环监听时默认 token=None。
- **路由** `spec_for_model`：`模型@平台` 显式 → 静态 models → 整表缓存索引 → 每平台 live 缓存兜底；**已取消「未知模型打到第一个启用平台」的静默兜底**，未知模型一律 503。
- **凭据可用性** `platform_secret` / `platform_routable` / `platform_supported` / `platform_usable`：没凭据的平台模型不进 `/v1/models`。
- **超时**：connect 10s / read 300s（kilo 流式实测首包 134s）/ 非流式总时长 180s / 拉模型清单 12s；`network_hint()` 把超时翻成人话。
- **流式记账**：`tap_passthrough` 用 tokio mpsc 回传统计（旧版 std mpsc 阻塞 worker 会饿死线程池）；中途出错补发 error 帧 + `[DONE]`；下游断开记失败不记成功。

### 历史实现：aigw-panel-rs（第一代 Rust，只作对照）
| 文件 | 职责 |
|---|---|
| `handlers/api.rs` | barrel：`pub mod routers; pub use routers::*;` |
| `handlers/api/routers/*.rs` | 14 域：util/overview/models/login/tencent/gwextra/accounts/router/catalog/settings/proxies/automation/wb/data（备份恢复） |
| `handlers/v1.rs` | `/v1/chat/completions`+`/v1/messages`；`pooled_attempt!` 宏选号（非 async fn+闭包，Rust 无 lending closure） |
| `install.rs` | ★ B1 自检安装（`--install/--uninstall/--purge`）：复制到 `%LOCALAPPDATA%\aigw-panel-rs\` + 桌面/开始菜单 `.lnk` + HKCU 自启 + MessageBox 反馈（对齐对方 NSIS setup.exe） |
| `handlers/api/routers/data.rs` | ★ D 数据备份/恢复：`/api/data/backup`(POST) / `/api/data/backups`(GET 列) / `/api/data/restore`(POST ?ts=，防穿越校验) |
| `server/pool.rs` | ★ S3 号池治理 |
| `server/db.rs` | ★ S5 SQLite 存储 usage_events（WAL+NORMAL+busy_timeout，JSON 合并幂等，DB 不可用降级 JSON） |
| `server/usage.rs` | UsageRecorder：DB 单条 INSERT，每 500 条批量 prune；DB 不可用回退 JSON 全量重写 |
| `server/credits.rs` | 积分到期快照 + urgency_rank |
| `server/balance.rs` | 数据驱动通用余额；`fetch_raw` 返原 JSON |
| `server/login.rs` | 登录会话+账号池；`raw_rows()` 拿未脱敏 secret |
| `server/tencent.rs`/`autocheckin.rs`/`cdp.rs` | 腾讯直连 / APP 自动签到 / CDP 浏览器(9333) |
| `relay/*.rs` | 原生中继+网页反代；`relay::out_client()` 超时四项分设 |
| `router/mod.rs` | 模型→上游 熔断/EWMA/亲和 |
| `tray/mod.rs` | windows-sys 托盘；HKCU Run `AigwPanel` |

## ★ S3 号池治理（server/pool.rs）
`pooled_attempt!` 唯一选号入口。InFlight 租约(同(平台,账号,模型)≤2并发) / (账号,模型)冷却(min(30s·2^n,900s)×抖动[0.5,1.5)) / WafIpGate(401/403→账号级拉黑30min) / RecentPick(60s窗口选≥2次让位) / 半开(全挡放行最紧迫) / `forget_account`(删号清幽灵)。`classify`：401/403→Auth，429→Rate，其它→Transient。
★ Loomy：HTTP 200 + `{"code":"100002"}` 假成功 → `loomy.rs::biz_err()` 识别(登录/失效/授权/token→401)。

## ★ 超时四项（relay::out_client）
connect(10s)+read(90s,覆盖TTFB与包间idle)+流式不设总时长+非流式总时长180s。`web.rs` 仍 no_proxy。

## ★ 余额/积分/urgency_rank
- 响应多是资源包列表：对 `Accounts[]` 按 `CapacityUnit==credits` 求和（单键递归只取首条必错）。
- 到期优先级 CycleEndTime>ExpiredTime>DeductionEndTime；分桶 expired/d1/d3/d7/d14/d30/later/none；60s 缓存。
- `urgency_score`：有到期日→days*1000；无到期日有余额→10^7-余额；都无→i64::MAX。只落盘不实时算。

## ★ 模型清单
实时拉+缓存，顺序：专用分支(wb/loomy/yuanbao)→通用实时→缓存→静态。静态不许覆盖 live。元宝模型：`POST {"agentId":"naQivTmsDa"}`+头 `X-Exp-Params: enableNewPcStyle=2`。

## 前端（app/static 三处同步：源 / dist / 安装位）
`default_static_dir()`：`<exe>/app/static`→`../../aigw-panel/app/static`→`../app/static`→`AIGW_STATIC_DIR`。路由 `/app.js`。
改前端 bump `index.html ?v=`；★ CSS 注释禁止 `*/` 提前闭合（写完校验 `/*`==`*/` 计数+brace平衡）；JS `node --check` 查不出运行时错，需 `node test_views.js` + headless Edge 截图。

## 关键坑（高频）
- `list_accounts()` secret 脱敏 → 后台必须用 `login.raw_rows()`。
- `now_local()` 须 +8h。
- `/api/route` 契约须对象映射(api.rs describe_models)，不能 Vec<String>。
- 分层：`api.rs`+`api/`子目录挂子模块；`routers/mod.rs` 的 `pub use` 是兼容层别删。
- 单测断言别写死随机量上限（如 waf_left 只断言下限）。
- 改前端 `node --check` 不够 → `node test_views.js`；JS 重名函数覆盖 → grep 验零重复。

## 登录链路
copilot 原生扫码：`_start_native_qrcode`→`_poll_native`(轮询 `/v2/plugin/auth/token?state=`)→token 落池(secret=accessToken)。poll 竞态无 gw_session 静默等下轮。anon-zen：method=anon 免登录。

## 参考框架
xiaofan6ya/workbuddy2api：① 按领域拆路由 ✅ ② pool 号池治理 ✅ ③ 积分到期面板 ✅。不照搬 MySQL/Redis、Python 栈、读桌面端 .info。

## ★ v2 全量 Rust 重构（2026-10-16 立项，M0 已落地）
- 用户指令：26 个 2api 仓库+本地项目全部拉取解析记录；**不要现有框架，全部用 Rust 重构**；先计划后执行。4 项决策已按推荐确认：A 静态 JS+rust-embed / 新建 aigw2 / 核心 15 平台优先 / 成本归因进 M6、订阅通道 backlog。
- 文档：`网关项目/重构调研-2api项目全集.md`（26 仓库摘要+32 项算法提取矩阵+本地盘点+红黑榜）、`网关项目/重构计划-v2.md`（aigw2 workspace 9 crates + providers/*.toml 声明式接入 + M0-M6）。
- **M0 骨架 ✅（2026-10-16 15:00）**：`aigw2/` workspace（aigw-core 错误分类+StreamChunk / aigw-registry 声明式 spec / aigw-server axum+rust-embed / aigw bin CLI）；providers/anon-zen.toml 首份 spec；19 测试全过；8801 起服端到端验证通过（health/models 契约/chat 501 占位/首页）。
- **M1 引擎核 ✅（2026-10-16 15:40）**：7 crates（+store/pool/relay），35 测试全过；SQLite+旧数据只读幂等导入（28 条）；号池六件套（租约/冷却/WAF/RecentPick/半开/forget）；relay 超时四项+字节透传+SSE 归一解析；**真实对话打通**（pollinations 免密车道非流式+流式，anon-zen 已被上游收紧 403 FreeTierError 只能当错误路径用）。
- **M2 声明式接入 ✅（2026-10-16 16:05）**：8 crates（+crypto），45 测试全过；registry 全字段（models_path/models_filter/upstream_kind/signer/biz_err）；glm 签名（SECRET=8a1317a7468aa3ad86e997d08f3f31cb，校验位算法）+chatglm adapter（parts 快照差分转 delta，assistant_id=65940acff94777010aa6b796）；AccountStore 导入旧 accounts.json（8 账号，明文 secret 后台专用）；**kilo 免密真实对话 ✓**（isFree 过滤 15 模型，cost:0）；web-glm 无凭据 401 门控 ✓（等用户登录 chatglm.cn）；4 spec：anon-zen/pollinations/kilo/web-glm。
- **M3 透传批次 ✅（2026-10-16 16:35）**：11 spec 全载；`模型id@平台` 后缀路由（旧版语义）；settings 占位符（@lobster→lobster_server，旧 settings.json 只读导入）；Cookie 注入（AuthKind::Cookie→Cookie 头）；ModelsPolicy::None（无清单纯后缀路由）。元宝透传真实打到上游（405=需补 Referer/Origin，链路全通）；web-lobster 占位符门控 502。**批2 OAuth/批3 签名类/批4 wb+deepseek POW+豆包 CDP 待后续**（状态总表在 daily log）。
- **M4 管理面 + M5 安装器 ✅（2026-10-16 17:45）**：/api/platforms+accounts+pool 三端点；web/ 四视图 SPA（总览/平台/账号/用量，玻璃基座，headless 6/6 命中）；bin --install/--uninstall/--purge（%LOCALAPPDATA%\aigw2 + .lnk + HKCU Run\Aigw2 直写注册表）。45 测试全过。
- **★ v2 收官（2026-10-16 18:45）**：12 平台 spec（11 启用）+ 62 测试 + 单 EXE。**唯一剩余=wb-gateway 客户端仿真专项**（spec 已铺 enabled=false；WAF 要求完整头族：X-Agent-Purpose/risk_headers/X-Machine-ID/X-Device-Token(Turing Shield 缺失=11128)/会话头族 B3；workbuddy2api-master 一手全文在 _refs/；池内 JWT 27 天有效）。kilo 免费池上游波动属常态（错误透传已验证）。
- **★★ 10-17~10-20 巨变（daily log 全天细节，此处蒸馏）**：
  - **铁律反转（10-19 拍板）**：v2 禁扫码/禁无头浏览器/CDP/Chromium（10-16 的 cdp.rs/browser_dl.rs 已删）。全部登录=跳转（用户自己浏览器）+手动粘贴，或设备码/API Key。起因：擅自建 browser_dl 被面斥。
  - **接入源 12→122**：gen_providers.py/2 从旧 catalog 批量生成（官方 API 24 / 订阅 8 / 客户端 27 / 公益站 64）；公益站=bubblevv 聚合源 JSON-LD、FreeStore 每日刷新已添加不丢；自定义 API=CustomStore+CherryStudio 式弹窗（OpenAI/Anthropic 双协议）。
  - **★ 坑**：load_dir「任一非法即 Multi 空表」——新增 upstream_kind(doubao/yuanbao/kuku/metaso)/signer(loomy) 必须同步 registry 白名单，否则全表静默丢失（10-20 踩过，平台数从 122 掉到 47）。
  - **登录引擎**（auth_login.rs，一手全落地）：设备码=wb族/MiniMax/Kimi(RFC8628 auth.kimi.com X-Msh 头)/Grok(OIDC)/Meta Muse/Cline(WorkOS)；PKCE 回调=Codex(1455)/Claude(54545)/Antigravity(51121)/Devin(headless)/Trae(回调直传 refreshToken→ExchangeToken)；跳转轮询=CodeArts(华为旧 ticket 1s×120→AK:SK)/Qoder(设备码+PKCE S256)/Lobster(OAuth)；save 即验活。
  - **adapter**：doubao(samantha 旧端点)/metaso(匿名13发/IP 走代理可用)/yuanbao(建会话+仅流式)/kuku(BDUSS+STOKEN errno 11002=Quota)/codearts(SDK-HMAC-SHA256)/wb(每日签到)/proxy(出站代理三模式+CN直连热更新)；cline WorkOS 401 刷新双通道自愈；SSE 建连后错误发流内 error 帧。
  - **参考项目全量解析**（10-19，3523 行/29 万字符，27 项目在 _refs/ 不入库）：Part C 能力总表→5 批落地（StickyRouter 粘性 1800s FNV-1a 兜底最后一条 user/donut+面积图/CodeArts/WorkOS 自愈/wb 签到）。sub2api 纠偏=OAuth 订阅号池化售卖网关，非机场转换。
  - **真实端到端（10-20）**：秘塔 ✅（匿名+代理）、豆包 ✅（真实 Cookie 旧端点）；元宝 ❌ Cookie 过期需重登；WPS/纳米 ⚠ 仅验活不可对话。
  - **运维**：构建产物在 **C:\aigw-target\debug\aigw.exe**（CARGO_TARGET_DIR 已改）；部署=taskkill→拷exe→同步providers→schtasks /Run Aigw2Service；前端改后必须 cargo build（rust-embed）+ Ctrl+Shift+R；代理 global 7890；accounts.json 明文勿泄。
- **批2 登录引擎 ✅ + M6 可观测 ✅（2026-10-16 18:00）**：wb 族设备授权（/v2/plugin/auth/state→token，三头缺一被 APISIX 拦）+ MiniMax PKCE S256；AccountStore 双源持久化（v2 own 权威，add_account 落池）；/api/auth start/poll 真实拿到 copilot.tencent.com 服务端 state ✓；/api/requests 历史+平台归因。
- **批3 签名器 ✅ + 批4 CDP 桥 ✅（2026-10-16 18:20，62 测试全过）**：ghfast zip 拉三仓库源码提全常量 → crypto/qoder.rs（COSY：指纹派生+自定义Base64+RSA 包裹+AES-CBC info+md5 五段签名，4 测）/loomy.rs（HMAC-SHA1 九行签名+AK 2thryby66wxi53sk，5 测）/zcode.rs（enc:v1 AES-256-GCM SHA256 派生，4 测）；relay/cdp.rs（tungstenite 版受控浏览器桥，Storage.getCookies+host 过滤，真实拉起 Edge ✓）；/api/auth 支持 spec.login_url 平台的 CDP 会话。**批3 踩坑**：rsa 0.9 加密方向 key.encrypt(rng,pad,data)、from_public_key_pem 需 pkcs8::DecodePublicKey、tempKey 是 16 字符 hex 非 32、json compact None→""。
- **★ v2 构建三坑（写代码前必看）**：① 本机 rustup default=**windows-gnu**，新 workspace 从零编 windows-sys 0.61+（raw-dylib）报 dlltool not found → 项目根必须放 `rust-toolchain.toml` 固定 `stable-x86_64-pc-windows-msvc`（旧项目靠 C:/aigw-target 热缓存躲过，从零重编同样会挂）；② rust-embed 插值 folder 需 `features=["interpolate-folder-path"]`；③ `cargo test` 不产 bin，端到端前要 `cargo build`。
- 协议基线（活跃）：Sark1tama/glm2api、qoder2api-hub、wangqi233/trae2api、Trae2api-cn、lobsterai2api、luawei1/cline2api、xingluoyuankong/loomy2api、dengyie/zcode2api、grok2api、AIClient2API、dsh、workbuddy2api、sub2api、doubao2api、TecentHelp2Api、ops120/zcode2api。
- 关键提取：cline2api WorkOS 自愈+uTLS(rquest)；lobster 错误分类→冷却映射；dengyie 账号状态机；qoder COSY 签名+设备指纹派生；dsh 按 body 形状判流+Ed25519 升级；sub2api WS↔SSE 桥；TecentHelp 会话粘定 userid。
- 本地有 LobsterAI2API 与 cli-proxy/cliproxy-src 克隆（CPA 生态）。
- License 纪律：sub2api=LGPL、cherry-studio&dengyie=AGPL、AIClient2API=GPLv3 → 只参考机制不复制代码。
- 2026-10-16 15:00 观察：旧版 aigw-panel.exe 进程已不存在（8800 空闲，非本次操作所致，taskkill 仅精确匹配 aigw.exe 验证实例）。

## ★ UI 玻璃拟态设计系统（2026-10-16 起）
- 主参考 dsh-our-free-model（毛玻璃+短动效+数据可视化）；用户判定原 UI 太朴素，已确认「原生重做设计系统」路线 + 试点先行。
- 基座在 style.css 末尾段：`--glass-bg/--glass-bd/--glass-shadow/--blob1/2`（亮暗双份）+ `body` 双 radial 渐变光斑(fixed) + `.glass` 工具类（**定义必须在 .card/.mini/.todo-i 之后**，同特异性后者胜，加 `glass` 类即换肤）+ `.stat` KPI 玻璃卡 + `.pill` 药丸 + `.about-hero`/`.timeline`/`.tbl` + `viewIn` 入场动画（reduced-motion 全关）。
- 试点 总览+关于 ✅（`?v=20261016f`）；其余 8 页待用户确认视觉方向后铺开（积分到期→donut 风险分布、用量→面积图/热力图）。
- ★ 坑：给 section 加包裹层(.ov-anim/.about)后 `#view>section{margin-bottom:20px}` 选择器失效，需为 `.ov-anim>section` 单独补 margin；`.about>section` 不补（卡片自身 16px margin 已够）。

## 进度
S1 分层 ✅ / S2 积分+urgency ✅ / S3 号池 ✅ / S4 UI 9项 ✅ / S5 SQLite+响应体积(credits 分页) ✅ / S6 前端自动校验 ✅。
B1 自检安装 ✅ / B2 双项目对标文档 ✅ / C 关于页 ✅ / D 数据备份恢复 ✅（均 2026-10-16 上午落地）。
UI 玻璃拟态试点（总览+关于）✅，其余 8 页待确认后铺开。
未做：远程扫码 qrscan；tasks.json 持久化；app/relays 原生替代未接线；**A 源接入声明化重构（B2 文档 P0，尚未实施代码）**；NSIS 安装器本机无 makensis 未编译（仅备用脚本）。

## B1 安装模式要点（install.rs）
- 本机无 makensis → 用 Rust 二进制 `--install` 自检安装（复制 exe+app/static+webpow 到 `%LOCALAPPDATA%\aigw-panel-rs\`、建桌面/开始菜单 `.lnk`(PowerShell COM WScript.Shell+BOM)、写 HKCU `Run\AigwPanel` 指向安装位 `--tray`、MessageBox 反馈后启动已装 exe）。
- `--uninstall` 删快捷方式+自启（保留 data）；`--purge` 卸载+延迟 `cmd` 删整目录。
- `set_autostart_for(exe_path, enable)` 抽出自 `tray/mod.rs`，供安装流程复用；值为 `"<exe>" --tray`。
- 数据目录天然落在安装位 `data/`（重装/升级不丢数据）。
- 可选 `installer/aigw-panel.nsi`（Unicode/用户级/装到 `$LOCALAPPDATA\aigw-panel-rs`）仅备用，本机无法编译验证。

## D 数据备份恢复（data.rs）
- `DATA_FILES` 8 文件：settings/accounts/panel.db/disabled_models/sites/checkins/tasks/notify。
- 备份=目录快照复制到 `data/backups/<UTC时间戳>/`+`manifest.json`（零压缩依赖，避免损坏）；恢复=`is_safe_name` 防 `../` 穿越后拷回 `data/`，返回 `need_restart:true` 前端调 `/api/settings?action=shutdown` 重启。

## 2026-10-21 关键变化（详见 memory/2026-10-21.md）
- **账号健康可见**：号池 `Pool.health`（内存）记录每个账号最近一次真实请求结论（ok/auth/quota/rate），`/api/accounts` 每行带 `health`，`POST /api/accounts {action:verify}` 立即验活；前端账号表有「健康」列 + 「检查」按钮。面板终于能一眼看出哪个号过期。
- **错误类别 → HTTP 状态码**：`aigw_relay::status_for_class()`（Auth 401 / Quota 402 / Rate 429 / Rejected 400 / Waf 403 / Transient 502），修正 metaso/kuku 把「限流」包成 502 导致 SDK 不退避的问题。
- **同名模型择优路由**：`spec_for_model` 先用 `platform_usable`（可转发）再回落 `platform_routable`，避免 `@lobster` 这类占位地址平台抢到同名模型后返回 503。
- **usage_prune 按 rowid 保留最近 N 条**（ts 是 REAL 秒，同秒多条会整组误删）；清理失败改为 `tracing::warn!`。
- 测试基线 **150 项全绿**（core 3 / pool 21 / registry 18 / relay 24 / server 47 / store 23 / crypto）。

## 2026-10-22 关键变化（详见 memory/2026-10-22.md）
- **web-deepseek 原生 adapter 上线**：此前是「声明式空壳」，现在是 `upstream_kind="deepseek-web"` + `aigw2/crates/aigw-relay/src/deepseek.rs`（内嵌 `deepseek_sha3.wasm` + wasmi 解 POW）。协议 5 步：users/current 取 token → chat_session/create → create_pow_challenge → `X-Ds-Pow-Response`（base64 JSON，**answer 必须是整数**）→ chat/completion 的补丁式 SSE（`{v:…}` 快照 / `{p,o:APPEND,v}` / 无 p 的纯增量 / BATCH 取 accumulated_token_usage）。
- 真机实测：非流式 2.58s 出「你好」；流式 reasoning_content 正常。测试基线 **215 项全绿**；exe 20,112,384 字节；/api/platforms 127 行。
- 三个只在真机暴露的坑：① wasmi 参数要用 `wasmi::core::F64`（不是 f64）② POW answer 用 f64 序列化成 `69573.0` → 上游 HTTP 200 返回 `40301 INVALID_POW_RESPONSE`（2xx 无正文必须把原始 body 截进错误信息）③ 求解 1~2s 必须 `spawn_blocking`。
- 构建提速：`[profile.dev.package.wasmi]`/`wasmi_core` 的 `opt-level = 3` 把 POW 从 14,855ms 降到 1,195ms（Node 21ms 是 JIT 待遇）。
- cargo 一律用 `--offline`：git 全局 `http.proxy=127.0.0.1:7890`（Clash 未跑）会让 crates.io 拉取全失败。

## 2026-10-22 补齐（详见 memory/2026-10-22.md 三节）
- 「都做」收官：`apk-qwenwork` 是**路径写错**（OpenAI 形状，chat_path=/api/v1/chat/completions）；`apk-coze` 新增 coze adapter（v3 chat、模型名=bot_id、写 `<bot_id>@apk-coze`）；`sub-devin` 新增 devin adapter（会话型 Agent：POST /v1/sessions + 轮询 status_enum 到 finished，取最后 devin_message；每次请求消耗额度，需显式 `devin@sub-devin`）；`sub-meta` 本机不可达（curl 000）只能诚实标注需代理。
- 数字：workspace **249 tests**、exe **21.47MB**、`/api/platforms` **127**。
- 写文件教训：分段写大文件必须 `appendFileSync`（`writeFileSync` 会把已写好的段覆盖）；测试内联 toml 必须有 `stream_dialect`。

## 2026-10-26 仓库治理与模块拆分
- **源码入库**：此前约 90 份 `providers/*.toml`、`aigw-relay` 十个 adapter、`aigw-store` 六个模块（atomic/secret/keys/combos/rotlog/archive）全是未跟踪文件，只在工作区里存在。已一次性提交。`.gitignore` 新增 `_fuzz/` `_qr/` `_axtest/` `aigw2/_probe/` 与根目录探针碎片（`_t*.json` `r1.txt` `p.json` 等）。删掉过期的 `accounts.rs.bak`。
- **spec 去重**：`api-bailian` 与 `api-dashscope` 同端点同 Key 同清单 → 删除前者（老配置 `@api-bailian` 要改 `@api-dashscope`），`gen_providers.py` 分支同步收紧。剩余 6 组同 base_url 但**语义不同**（官方 Key vs 订阅反代），不合并：codex/sub-codex、kimi-code/sub-kimi、apk-trae/web-trae、apk-qoder/web-qoder、apk-antigravity/sub-antigravity、wb-gateway/wb-gateway-intl。
- **前端版本号**：`index.html` 的 `?v=` 已删（服务端本来就发 `Cache-Control: no-cache`），`app.js` 的 `BUILD` 是唯一版本号，现为 `v20261026a`。
- **★ 新建模块**（aigw-server）：`net.rs`（host/origin/SSRF 判定纯函数，9 测）、`ratelimit.rs`（三档限流，7 测）。`lib.rs` 仍 16074 行，其余拆分（协议互转 / 流式状态机 / 路由域）尚未做。
- **guard 性能**：每个 /v1 请求原本两次线性扫 KeyStore，现合成一次。
- **测试基线**：workspace **335** 全绿（core 3 / crypto 21 / pool 29 / registry 13 / relay 151 / server 232 / store 82）；`cargo clippy` 未安装（需 `rustup component add clippy`），目前只能用 `cargo check` 当告警闸。
- 环境：本机 `pwsh` 偶发启动失败（exit 3221225794），文件操作用 Node fs 更稳。

## 2026-10-26 持续优化（第二轮）
- **新增模块**：`dialect.rs`（三方协议互转 + 9 条用例）。`lib.rs` 16074 → 15421 行。
- **/v1/models 三处过滤改 Cow**：无链/无 key/无 ?platform= 时零分配；白名单匹配不再逐条 `format!("{id}@{owner}")`。
- **Retry-After 感知**：`RelayError::retry_after_hint()` 解析 JSON/文本两种形态，夹 [1,3600]；`Pool::report_with_hint` 采纳并夹 [1,900]，有提示不涨 strikes。
- **在途已满换号**：`chat_core` 从「死等同一账号 15s」改为「等 1.5s → sticky.unbind + tried.push + 重选」，全平台无别号才退回 15s 预算。★ 换号必须解绑粘性，否则死循环。
- 测试基线 **352**（core 3 / crypto 21 / pool 33 / registry 13 / relay 153 / server 243 / store 82）。
- ★ 提交信息必须用 `[System.IO.File]::WriteAllText(..., UTF8Encoding($false))`；Out-File utf8 会带 BOM 把中文打成问号。

## 2026-10-26 第三轮（推理内容 / 并发 / 缓存）
- **★ 上游 reasoning_content 之前被整体丢弃**（DeepSeek/GLM/Kimi/MiniMax 通用字段）：
  - Anthropic 侧：新增 thinking 块（`thinking_index`/`thinking_open`），块顺序由 open_text_block 保证「先想再说」；
  - Responses 侧：新增 `reasoning` item；★ 必须在**第一条正文 delta 之前** flush（`flush_reasoning` 幂等），否则正文已占 output_index 0，再插会让客户端 index 错乱；
  - 非流式 `openai_to_anthropic`/`openai_to_responses` 同步支持（dialect.rs）。
- **整表模型缓存改 `Arc<Value>`**：命中路径锁内只做引用计数 +1，过滤/序列化全在锁外（原实现在锁内深拷贝整表）。
- 测试基线 **354**（core 3 / crypto 21 / pool 33 / registry 13 / relay 153 / server 247 / store 82）。
