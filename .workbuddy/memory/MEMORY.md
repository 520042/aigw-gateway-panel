# 网关项目 · 长期记忆

## 定位
免费 AI 额度 → 本地 OpenAI 端点（`/v1` 暴露 chat/completions、models、messages）的聚合面板。单 EXE 自洽，无外部网关。Rust 版 `aigw-panel-rs` 为主力（2026-10-15 起）；Python 版 `aigw-panel` 次要。

## 硬规则（被纠正过，必须遵守）
- **UI 只显示「国内站 / 国际站」两行**（WorkBuddy/CodeBuddy 合并卡）。`apk-codebuddy`/`apk-codebuddy-cn` 是 `wb-gateway-intl`/`wb-gateway` 别名，不加 `gwlogin.PLATFORMS` 规格，不进 `api/login?action=platforms`。★ 云端旧摘要「4 行独立」是过期错误记录，2026-10-14 被当面纠正，以本段为准。
- 绝不引入/提及 WorkBuddy Gateway（Go EXE，已彻底移除）。
- 数据目录 = `%LOCALAPPDATA%\aigw-panel-rs\data\`（非 `aigw-panel\data` 陈旧位）。
- 安装位 `%LOCALAPPDATA%\aigw-panel-rs\`（非 Downloads，火绒会拦）。

## 构建 / 部署
- 构建：`export PATH=/c/Users/liang.zhao/.cargo/bin:$PATH; export CARGO_TARGET_DIR=C:/aigw-target; cd aigw-panel-rs && cargo build --target x86_64-pc-windows-msvc`
- 产物 copy 到安装位 + dist（`C:/aigw-target/aigw-panel-dist/`）；deepseek POW 需拷 `app/webpow/`
- 启停 PowerShell `Stop-Process`/`Start-Process`；探活 `curl --noproxy 127.0.0.1 http://127.0.0.1:8800/`；固定端口 8800
- 数据源单一：`catalog_bundled.json` 由 `gen_catalog.py` 从 Python 一手模块导出
- 测试：`cargo test --target x86_64-pc-windows-msvc`（当前 29 项：lib 18 含 pool 7/db 3/data 2 + server_smoke 11；1 doctest ignored）

## 模块地图
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
