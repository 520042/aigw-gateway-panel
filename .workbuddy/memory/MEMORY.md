# 网关项目 · 长期记忆（2026-11-05 蒸馏版）

> 这里只放**跨会话必须知道**的结论、规则和坑。
> 逐日过程、调试实录、中间数据一律看 `memory/YYYY-MM-DD.md`。
> 旧版 228KB 全文已归档为 `MEMORY.full-2026-11-05.bak`（含 10-26 起 127 轮改造记录，需要细节时再查）。

## 定位

免费 AI 额度 → 本地 OpenAI 端点（`/v1` 暴露 chat/completions、models）的聚合面板。
单 EXE 自洽，无外部网关。

**主力 = `aigw2/`（Rust workspace，9 crates + 百份 `providers/*.toml`）**，2026-10-16 起持续演进。
`aigw-panel-rs/`（第一代 Rust）与 `aigw-panel/`（Python 原版）是历史实现，只作对照，不再加功能。

## 硬规则（被当面纠正过）

- **UI 只显示「国内站 / 国际站」两行**（WorkBuddy/CodeBuddy 合并卡）。`apk-codebuddy`/`apk-codebuddy-cn` 是 `wb-gateway-intl`/`wb-gateway` 别名，不加 `gwlogin.PLATFORMS` 规格、不进 `api/login?action=platforms`。
- 绝不引入/提及 WorkBuddy Gateway（Go EXE，已彻底移除）。
- 数据目录 = `%LOCALAPPDATA%\aigw2\data\`。安装位 `%LOCALAPPDATA%\aigw2\`（Downloads 会被火绒拦）。
- **aigw2 铁律：禁扫码 / 禁无头浏览器 / 禁 CDP·Chromium**。登录只允许「跳转链接+手动粘贴回调」「设备码」「API Key」「Cookie 粘贴」四种。
- 结论先行 + 表格优先；不猜硬件/规格参数；存疑单列「待确认」。

## 构建 / 部署

```powershell
# workdir 必须是 aigw2/
$env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:CARGO_TARGET_DIR='C:/aigw-target'
cargo build --target x86_64-pc-windows-msvc -p aigw
```

- 产物：`C:\aigw-target\x86_64-pc-windows-msvc\debug\aigw.exe`（**不是** `C:\aigw-target\debug\aigw.exe`，那是旧位）
- 部署：`taskkill /IM aigw.exe /F` → 拷 exe + `providers\*.toml` 到 `%LOCALAPPDATA%\aigw2\` → `schtasks /Run /TN Aigw2Service`（BootTrigger + InteractiveToken，`--port 8800`）
- 改前端 `aigw2/web/app.js` **必须重新 cargo build**（rust-embed 内嵌），再 Ctrl+Shift+R
- 测试：`cargo test --target x86_64-pc-windows-msvc`（2026-10-20 基线 **127 项全绿**）
- cargo 一律加 `--offline`：git 全局 `http.proxy=127.0.0.1:7890`（Clash 常不在跑）会让 crates.io 拉取全失败
- 实测接口用 **node fetch**：PowerShell `Invoke-WebRequest` 把 UTF-8 当 GBK，中文全乱码（不是服务端 bug）

## 模块地图（aigw2）

| crate / 文件 | 职责 |
|---|---|
| `crates/aigw-core` | `ErrorClass::from_status` / `refine_biz` 错误分类 |
| `crates/aigw-registry` | `ProviderSpec` 声明式接入；`load_dir_lenient`（坏文件只跳过）+ `apply_settings` 解 `@占位符` |
| `crates/aigw-crypto` | 签名：glm X-Sign / codearts SDK-HMAC-SHA256 / loomy / qoder / zcode |
| `crates/aigw-store` | `accounts` 凭据池 / `custom` 自定义 API / `free` 公益站 / `flags` 逐模型停用 / `atomic` tmp+fsync+rename / `lib` SQLite usage（DB 挂了降级 JSON） |
| `crates/aigw-pool` | 号池六件套（见下） |
| `crates/aigw-relay` | 出站 HTTP + 平台反代：`lib.rs` 通用透传与流式 `tap_passthrough`；`doubao/yuanbao/kuku/metaso/glm/loomy/codearts/deepseek/coze/devin/orcaterm` 专用；`auth_login.rs` 登录链路；`wb.rs` 签到；`proxy.rs` 出站代理 |
| `crates/aigw-server` | axum 路由 + `guard` 门禁 + `AppState`（`spec_for_model`/`resolve_spec`、模型缓存、备份、usage API、rust-embed 前端） |
| `crates/aigw` | bin：CLI（host/port/token/providers/install/purge）+ 启动装配 + `warm_models` |
| `web/`（app.js/index.html/style.css） | 单页前端（总览/接入源/账号/用量/关于），rust-embed 内嵌 |
| `providers/*.toml` | 平台规格：free-* 公益站 / api-* / apk-* / sub-* / web-* 等 |

### aigw2 关键机制（改代码前必读）

- **门禁 `guard`**：Host 白名单 → Origin/Referer 白名单 → `/api/*` 令牌（`/v1/*` 不校验）。回环监听默认 token=None。
- **路由 `spec_for_model`**：`模型@平台` 显式 → 静态 models → 整表缓存索引 → 每平台 live 缓存；**未知模型一律 503**（已取消「打到第一个启用平台」的静默兜底）。
- **可用性**：`platform_secret` / `platform_routable` / `platform_supported` / `platform_usable`；没凭据的平台模型不进 `/v1/models`。
- **超时**：connect 10s / read 300s / 非流式总时长 180s / 拉清单 12s；`network_hint()` 翻人话。
- **流式记账**：`tap_passthrough` 用 tokio mpsc 回传统计（std mpsc 会饿死线程池）；中途出错补发 error 帧 + `[DONE]`；下游断开记失败。

## 号池 / 余额 / 清单 / 前端 / 登录（合并速查）

- **号池六件套**（`aigw-pool`）：InFlight 租约（同账号+模型 ≤2）/ (账号,模型) 冷却 `min(30s·2^n,900s)×抖动` / WafIpGate（401·403→账号级 30min）/ RecentPick（60s 内选过 ≥2 次让位）/ 半开（全挡时放行最紧迫）/ `forget_account`（删号清幽灵）。冷却键必须用**上游真名**。
- **错误语义**：Auth 401 / Quota 402 / Rate 429 / Rejected 400 / Waf 403 / Transient 502（`status_for_class`）。
- **余额/积分**：响应多是资源包列表，对 `Accounts[]` 按 `CapacityUnit==credits` 求和（只取首条必错）；到期优先级 `CycleEndTime>ExpiredTime>DeductionEndTime`；分桶 expired/d1/d3/d7/d14/d30/later；60s 缓存；死号要清余额快照，否则加权反噬。
- **模型清单**：顺序 = 专用分支（wb/loomy/yuanbao/orcaterm）→ 通用实时 → 缓存 → 静态；**静态不许覆盖 live**。元宝模型：`POST {"agentId":"naQivTmsDa"}` + 头 `X-Exp-Params: enableNewPcStyle=2`。
- **前端**：改完 `node --check` 不够，要 `node test_views.js` + headless 截图；`index.html ?v=` 已删，版本号只看 `app.js` 的 `BUILD`；CSS 注释禁 `*/` 提前闭合（校验 `/*`==`*/` 计数 + brace 平衡）。
- **登录**：设备码 = wb 族 / MiniMax / Kimi(RFC8628) / Grok(OIDC) / Meta Muse / Cline(WorkOS)；PKCE 回调 = Codex(1455) / Claude(54545) / Antigravity(51121) / Devin / Trae（回调直传 refreshToken→ExchangeToken）；跳转轮询 = CodeArts(华为 ticket 1s×120→AK:SK) / Qoder / Lobster。save 即验活。
- **验活三态**：ok / auth / unknown。网络抖动不许判成凭据失效；公开 `/models` 会掩盖错 key（59 个假阳性教训）。
- **Token 限制器**：从「事后冷却」改「事前控制」，`available()` 必须同时看限制器，否则粘性路由/搜索兜底会绕过（踩过两次）。

## 高频坑

| 坑 | 现象 | 解法 |
|---|---|---|
| `load_dir` 白名单不同步 | 新增 `upstream_kind`/`signer` 后**全表静默丢失**（122→47 平台） | 改 registry 白名单必须同步 |
| 旧数据 secret 脱敏 | 后台拿到 `***` | 用 `login.raw_rows()`，不用 `list_accounts()` |
| 时区 | 时间戳差 8 小时 | `now_local()` 必须 +8h |
| `/api/route` 契约 | 前端拿不到映射 | 必须是对象映射，不能 `Vec<String>` |
| 单测写死随机量上限 | 偶发失败 | 只断言下限 |
| `list_accounts()` 空字段 | 前端 XSS 审计时的坑 | 输出一律转义 |
| **设计令牌写在 light 选择器里** | 切暗色后 `var(--sp-*)`/`var(--r-*)` 全部失效，卡片紧贴无内边距，整版塌成横条（2026-11-12 实测） | 非颜色令牌（间距/字号/圆角/控件高/字体/动效）必须放 `:root`，主题块只放颜色 |
| `/api/logs` 参数名漂移 | 前端发 `lv`/`g`，后端读 `level`/`grep` → 级别与关键词筛选「能选能打但全量返回」 | 参数名以后端 `logs_view` 为准 |
| `/api/requests` 的 recent 字段 | 是 `tin`/`tout`，不是 `total_tokens`/`tokensOut` → 趋势图取值恒 0，画出一条贴底直线 | 读 `tout` |
| 主题 `loadPref` 白名单 | 循环到「跟随系统」后 localStorage 存 `auto`，白名单没有 → 刷新回落 light，档位与实际不符 | allowed 必须含所有档位 |
| `/api/health` 现在带 `data_dir` | 数据目录是本地信息，挂在 `/api/update-check`（要打 GitHub）上会一直「读取中」 | 本地信息不要依赖远端检查 |

## 里程碑

| 日期 | 事 |
|---|---|
| 10-13~10-15 | aigw-panel（Python→Rust 第一代），S1~S6 + B1 安装 + D 备份恢复 |
| 10-16 | **aigw2 立项并当日收官**：M0 骨架→M1 引擎核→M2 声明式接入→M3 透传→M4 管理面→M5 安装器；62 测试 |
| 10-17~10-20 | 接入源 12→122；**铁律反转：禁扫码/禁 CDP**；登录引擎全落地（设备码/PKCE/跳转轮询）；参考项目全量解析（27 项目 / 29 万字）→ 5 批能力落地 |
| 10-21~10-22 | 账号健康可见；错误类别→HTTP 码；web-deepseek 原生 adapter（wasm POW）；249 测试 |
| 10-26 | 仓库治理（90 份未跟踪 toml 入库）；模块拆分 20+ 轮；doctor 体检工具；Token 限制器雏形 |
| 10-28 | 127 轮收尾；验活三态化；国际版漏网修复一批；积分/倍率透明化 |
| 10-31~11-04 | 新 adapter：monkeycode / cloudcode / kiro / mimo / opencode / metaso / orcaterm 打磨 |

## 反复验证后的工程教训

1. **同一个判据要在所有入口成立**（chat / responses / anthropic / 后台 / CLI）——历史上被漏过 5 次。
2. **静默失效最贵**：限流参数改了不生效、poll 缺 platform、验活漏 adapter，都要靠「自查上一轮埋的雷」补。
3. **上游 2xx 不等于成功**：业务码在 body 里（loomy `100002`、ima `1411/600001`、orcaterm `10050000`），必须 `refine_biz`。
4. **错误帧不是 JSON**：orcaterm `data: [ERROR]: [LLM_ERROR]`、SSE 建连后错误要发流内 error 帧，否则失败被当成「上游什么都没说」。
5. **UUID / 时间戳这类「形状对但位不对」的字段**最容易误判成内容问题（orcaterm `input.id` 非 v4 → `Invalid inputId`）。
6. 大文件分段写必须 append（`writeFileSync` 会覆盖前段）；测试内联 toml 必须有 `stream_dialect`。
7. 代理相关：「开了系统代理」要复检全部海外源；`curl --noproxy '*'` 会把 `-x` 也排除掉，测代理时别加。

## 未决 / backlog

- `wb-gateway` 客户端仿真专项（WAF 头族：X-Agent-Purpose / risk_headers / X-Machine-ID / X-Device-Token(Turing Shield=11128) / 会话头族 B3；池内 JWT 27 天）
- A 源接入声明化重构（B2 文档 P0，未写代码）
- 远程扫码 qrscan、tasks.json 持久化、app/relays 原生替代
- NSIS 安装器（本机无 makensis，仅有备用脚本）

## 参考

- 一手参考项目在 `网关项目/_refs/`（27 个仓库，不入库）
- 文档：`重构调研-2api项目全集.md`、`重构计划-v2.md`、`参考项目全量解析-2026-10-19.md`
- License 纪律：sub2api=LGPL、cherry-studio/dengyie=AGPL、AIClient2API=GPLv3 → 只参考机制不复制代码
