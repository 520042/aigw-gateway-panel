# 登录机制全量整理（物料逆向汇总）

> 2026-10-14 整理。物料：7 个网关 EXE/APK + 项目源码（aigw-panel Python、LobsterAI2API、cli-proxy）+ GitHub 调研项目。
> 结论全部标注来源：`实测` = 本机验证过；`反解` = 二进制/源码逆向；`待测` = 本轮新增待用户验证。

## 一、WorkBuddy / CodeBuddy（同族同契约）★核心

### 历史链路（已死，勿再走）
| 链路 | 状态 | 证据 |
|---|---|---|
| 本地 state + `/login?platform=CLI&state=` + 轮询 `/v2/plugin/auth/token?state=` | **已死** | tencent.py L77：`/v2/plugin/auth/state` 实测 404 已删；gwextra L197"不要再加回来" |
| copilot CLI 授权页冷打开 | **已死** | 2026-10-14 实测渲染"登录失败"（截图） |
| EXE 内嵌 admin 登录 API（`/admin/api/login/start{edition}` → qr+authUrl → poll） | 依赖 EXE | 全量资源解析 §二；EXE 已按你要求移除 |

### 现行链路（auth-v2 · Keycloak realms/copilot）★当前唯一活路
```
1. 打开 https://www.workbuddy.cn/login/?platform=agents&state=0&redirect_uri=<回调>
   （state 随意，实测 state=0 可过；关键是 redirect_uri）
2. 用户登录 → 页面重定向 redirect_uri?code=...&iss=...&session_state=...
3. POST {origin}/auth-v2/realms/copilot/signin/session
   {code, issuer, redirect_uri}  → 会话/accessToken
4. token 作为 Bearer 调 /v2/chat/completions（国内 copilot.tencent.com / 国际 www.codebuddy.ai）
```
- **Rust 面板已实现**：`/auth/callback` 收 code+iss → 换 token → 入库 → 会话自动 success（`实测` 端点就绪；`待测`：redirect_uri 白名单是否放行 127.0.0.1）
- **保底**：登录 workbuddy.cn 后 F12 → 任意 `/v2/` 请求 → Authorization Bearer → 手动粘贴（`实测` 可用）
- 免费额度/积分端点（反解，verified）：`/v2/billing/meter/daily-checkin`（签到）、`get-user-resource`（积分）、`/console/enterprises/personal/models`（模型目录，也是验活探针）

### Cookie/本地浏览器（Chrome 127+ 已废）
- app-bound 加密（v20 前缀）+ DPAPI 绑定 → 普通进程解不开（实测 604 条全 v20）
- 只有 CDP（`--remote-debugging-port` → Network.getAllCookies）或老 Chromium v10/v11 直解

## 二、其余 6 个网关登录方式（全量资源解析 §二 实测补齐）

| 平台 | 登录方式 | Rust 面板现状 |
|---|---|---|
| 豆包 dev.doubao2api | Cookie（CDP/粘贴） | ✅ 粘贴 + verify 验活 + samantha 原生中继 |
| 元宝 dev.yuanbao2api | Cookie（CDP/粘贴） | ✅ 粘贴；原生中继待移植 |
| 小浣熊 dev.raccoon2api | Cookie + 登录送积分 | ✅ 粘贴；送积分待移植 |
| Trae aigw.app | 二维码（copilot 授权同族） | ✅ 粘贴 + 跳转登录页 |
| CodeBuddy 国际版 | 二维码（同族） | ✅ 粘贴 + 跳转登录页 |
| Go 原生网关 | 文件/Key 导入 | ✅ 粘贴即等价 |

## 三、GitHub 项目逐个解析（本地源码实拆 + 调研文档）

> 来源标注：`实拆` = 本地源码/二进制逐行读过；`调研` = 历史调研文档结论（VibeCoding与反代调研.md / 全量资源解析与总结.md §五，基于 GitHub API + 官方文档）。

### A. 登录机制直接相关（对我们的自动授权最有价值）

| 项目 | 登录/授权实现 | 来源 | 可借鉴 |
|---|---|---|---|
| **LobsterAI2API**（本地源码） | **实拆**：`{server}/login?source=electron&redirect_uri=http://127.0.0.1:{port}/auth/callback&state=` → 回调 `?code=&state=` → `POST /api/auth/exchange` → accessToken+**refreshToken** → 写 `auths/lobsterai-{uid}.json`；本地回调服务器 + **PKCE(S256)** + 10 分钟超时 | cmd/login/main.go | ★ 与 Rust 面板 `/auth/callback` 完全同构 → 证实 localhost 回调是这类项目标准做法；refreshToken 落库保活 |
| **CLIProxyAPI**（本地源码 cliproxy-src） | **实拆**：OAuth 凭据文件制——Management API 有完整 `auth_files_*_oauth` 系列（callback/devin/provider oauth + refresh 文件），codex 回调端口 1455；内置 CodeBuddy CN / CodeBuddy 国际 / Qoder / Kimi 的 OAuth 托管 | internal/api/handlers/management/ | 凭据文件热加载 + refresh 续期保活体系 |
| **workbuddy2api-panel**（1.8k★，Go） | **调研**：WorkBuddy/CodeBuddy 账号 → OpenAI 兼容网关 + 任务中心自动化 + Web 面板（账号池/积分任务/配置热更新） | VibeCoding调研 L21 | 任务自动化、账号池面板形态；登录细节待下轮下载源码深拆 |
| **Trae2api-cn**（Python） | **调研**：多账号 round-robin + **网页 OAuth 登录** + checkin 签到 + 并发控制 | 调研 L19 | Trae 系 OAuth 登录形态、签到 |
| **cockpit-tools**（18k★，Rust） | **调研**：Antigravity/Codex/Copilot/CodeBuddy/Trae 等 14 家切号 + 配额监控 + 自动唤醒，内置 API 服务（集成 CLIProxyAPI） | 调研 L18 | 切号与配额监控 UI 范式 |
| **claude-code-router**（37k★） | **调研**：`127.0.0.1:3456` 单端点接管 Claude Code/Codex/**WorkBuddy**/Kimi 等 CLI，桌面版 EXE | 调研 L17 | 路由接管 + 成本估算 |

### B. 公益站/中转层（登录=API Key，无 OAuth）

| 项目 | 实现 | 来源 |
|---|---|---|
| New-API / One API | 自建中转 + 签到系统；`NEWAPI_BASE_URL`+`ACCESS_TOKEN`+`USER_ID` 三件套 | 调研/§五 |
| All API Hub / newapi-ai-check-in / One API Hub | NewAPI 系站点余额/签到自动化（浏览器插件/Python/Node） | §五 |
| sub2api（43k★） | 订阅池化 + 用户级 Key 分发；new-api 已内置为渠道类型 | 调研 L20 |
| free-newapi | 公益站清单（本项目 17 站数据源） | §五 |

### C. 反代参考（登录机制与豆包同族=注入式）

| 项目 | 实现 | 来源 |
|---|---|---|
| doubao-free-api / yuanbao-chat2api / agent2api | Cookie 注入池 + 轮询 + 429 冷却 | 历史调研 |
| FreeLLMAPI | 14 家免费额度聚合单端点 | §五 |
| WindsurfAPI / codex-proxy | Windsurf/Devin、Codex 反代（四套标准 API） | 调研 L22-23 |
| dsh-router-codebuddy / dsh-connect-workbuddy | WorkBuddy 同族 token 管理（`workbuddy-status.json` 产物；本地 EXE 反解未发现明文 auth 端点，疑加壳） | 调研 + 实拆 |

### D. 登录机制横向结论（对 Rust 面板的最终决策）

1. **localhost 回调 + code 换 token 是社区标准范式**（LobsterAI2API 实拆同构 + CLIProxyAPI 同款 callback 体系）→ Rust 面板 `/auth/callback` 方向正确，WB auth-v2 放行概率高（待用户实测确认）
2. **refreshToken 必须一起落库**（Lobster/CLIProxyAPI 均如此）→ 面板凭据结构已留位（`refresh_token` 字段在 Python 侧存在，Rust 账号池需补）
3. **PKCE 是授权前由登录页完成**的（page JS 里走），回调方只需 code+issuer+redirect_uri → Rust 实现无需额外 PKCE
4. Cookie 类平台全部是「注入池 + 冷却」模式，Rust 已对齐（豆包已实测）
5. workbuddy2api-panel 与 Trae2api-cn 的登录实现**待下轮下载源码深拆**（本地无源码，调研文档未记登录细节）

## 四、Rust 面板登录能力现状（2026-10-14）

| 能力 | 状态 |
|---|---|
| 跳转登录（前端 window.open，同浏览器必前台） | ✅ 实测 |
| 会话 start/poll/submit/cancel + 抽屉局部刷新（不闪） | ✅ 实测 |
| auth-v2 回调自动授权 | ✅ 实现，redirect_uri 白名单 `待测` |
| 手动粘贴 Bearer/Cookie + verify_all 逐账号验活 | ✅ 实测 |
| 豆包 samantha 原生中继（含流式/thinking） | ✅ 实测（真对话返回） |
| wb-gateway 族 Bearer 直打 /v2/chat/completions（force-stream→聚合） | ✅ 实测（hy3@wb-gateway 真回复） |
| 元宝 / Trae / 小浣熊 原生中继 | ✅ 已移植（元宝多步私有协议 / Trae Cloud-IDE-JWT / 小浣熊 cookie；无凭据返回 need_login，有凭据走真协议） |
| wb 模型目录在线拉取 + 签到/积分/成长任务执行 | ✅ 实测（目录 32 模型带 credits；签到 HTTP 200） |
| web 反代族（glm 签名 / trae 直连 / deepseek POW） | ✅ 已移植（web.rs + webpow 随包分发；无凭据返回 need_login，有凭据走真协议） |
| `/api/tencent` 腾讯直连 7 action（status/quota/catalog/checkin/login/poll/trae_local） | ✅ 实测（status logged=true verify 200；quota 500 credits；catalog 32 模型；login→poll 服务端 state 11217） |
| 定时调度器（APP 平台自动签到，幂等+重试） | ✅ 已接线（autocheckin.rs + tokio 后台任务，60s 轮询） |
| 逐模型连接测试（test_model） | ✅ 实测（wb/doubao 真回复 + 延迟） |
| gwextra 动作执行 + 平台能力表 | ✅ 实测（数据驱动自 catalog.gwextra） |
| credentials/catalog/localproxy 小端点 | ✅ 实测（catalog 六大清单；localproxy 无二进制时如实 installed=false） |
| Loomy 对话中继（模型网关，OpenAI 兼容 + traceparent） | ✅ 已移植（relay/loomy.rs；无 session 返回 need_login，`platform_models` 有 session 时在线拉模型） |
| 系统托盘（打开面板 / 开机自启动 / 退出） | ✅ 实测（tray/mod.rs，windows-sys；注册表自启写入 `"<exe>" --tray` 并回读校验） |
| `settings?action=shutdown` / `autostart` | ✅ 实测（shutdown 真退进程；autostart 写/删 HKCU Run） |
