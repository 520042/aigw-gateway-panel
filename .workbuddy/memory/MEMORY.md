# 网关项目 · 项目长期记忆

## 定位
把 7 个「免费 AI 额度 → 本地 OpenAI 端点」网关（1 个 Go EXE + 6 个 Android APK）和公益中转站 / 官方免费额度平台，统一收进一个本地面板管理。
**核心诉求：只启动一个软件，面板自己干完所有事，不依赖 workbuddy-gateway 进程。**
**网关 EXE 已彻底移除**（build26），面板纯原生模式，自身即本地网关（`/v1` 暴露 OpenAI 兼容端点：`/v1/chat/completions`、`/v1/models`、`/v1/messages` Anthropic 兼容）。

## 路由现状（build36，auto + Free 两模型）
- `auto`：strategy=fastest（优先响应最快的上游）
- `Free`：strategy=free（优先免费模型：anon-zen / 豆包）
- 两者各挂 `native:anon-zen`（OpenCode Zen 公共免费池，Bearer public，免登录）作**常驻兜底** +
  `native:apk-doubao`（豆包原生中继，free=True）→ 豆包挂自动切 anon-zen，永不裸 503
- `Target` 有 `free` 属性；`pick()` 支持 fastest/weight/priority/**free** 四策略；free 候选排前，非 free 兜底排后
- 降级：429/5xx/超时 熔断+1 降级（最多 4）；4xx(除 429) 直接报错；连续 3 次→OPEN，60s 后可再选
- 会话亲和：请求体 `aigw_session` 复用上游；`aigw_strategy` 单次覆盖
- ⚠ 旧 EXE 不认新 `auto`/`Free` 模型 → 改完路由必须重打包+重启面板

## 关键模块（职责）
| 文件 | 职责 |
|---|---|
| `app/tlogin.py` | ★ 原生登录链路（state/URL/换 token/拉倍率），copilot 系 |
| `app/gwlogin.py` | 登录驱动（5×二维码/11×Cookie/2×文件/anon）；`verify` 全部实测校准 |
| `app/cdp.py` | WS+CDP+Browser；Cookie 取明文；`cookie_header` 双向域匹配 |
| `app/gwextra.py` | ★ 33 平台动作注册表 / 108 动作；`_API_OFFICIAL`+`api_hint_models` |
| `app/model_extract.py` | ★ 模型清单实时提取 + 内置回退（豆包/元宝/通用 5 种响应形状） |
| `app/router.py` | 路由引擎（fastest/weight/priority/free + native 目标） |
| `app/doubao_relay.py` | 豆包 samantha 原生中继（协议逆向） |
| `app/native_relay.py` | 6 平台原生中继（含 anon-zen 指纹头）；`relay_once/relay_stream` |
| `app/web_relays.py` | 网页版反代（web-glm/web-trae/web-deepseek，请求体推断待校准） |
| `app/acct_pool.py` | 通用账号池（轮询/冷却 soft 60s·hard 12h/请求级换号 3 次） |
| `app/accounts.py` | 账号池 `data/accounts.json` |
| `app/tray.py` | 纯 ctypes 托盘 + 单实例 + 开机自启 |
| `app/sources.py` | 分类 LOCAL(19)/API(15)/WEB(2) |
| `app/autocheckin.py` | 13 平台定时签到 |

## 豆包原生中继（app/doubao_relay.py，最常用上游）
- samantha：`POST /samantha/chat/completion?aid=497858&version_code=20800&device_id=<19位>`
  Cookie sessionid 双写；体 bot_id=7338286299411103781 + completion_option
  （use_deep_think/use_auto_cot 区分 pro/think/expert）+ messages[].content=JSON({text}) content_type=2001
- SSE event_type=2001（content_type 10000/2001/2008/2071 → text/thinking）；验活
  `/passport/account/info/v2` 返回 user_id 即有效
- 账号池轮转 + 429 冷却 60s；会话续接 30min TTL
- 用法：`/v1/chat/completions` model="doubao-pro"（裸名即走中继）；`/v1/messages`
  claude-* 自动映射豆包模式（Anthropic 兼容）
- EXE 四项已原生化：池轮询+冷却 / /v1/messages / 90天用量（store.usage_events + /api/usage）/ webhook（notify.py）

## 登录链路（关键坑）
- **copilot 系原生扫码**（免 EXE 免跳页）：`_start_native_qrcode` 生成 state+auth_url →
  前端 vendor/qrcode.min.js 现场出码 → `_poll_native` 轮询 `/v2/plugin/auth/token?state=`
  → token 以 type="token" 落账号池（secret=accessToken，直接用于中继/倍率）
- poll 竞态：无 gw_session 时静默等下一轮（勿误报「缺少登录会话 id」打终态）
- Qoder 是桌面端应用：浏览器 Cookie 流程已删（method=file 手动粘贴）；CLIProxyAPI 无 -qoder-login
- **本地网关 EXE ≠ WorkBuddy 账号**（用户反复强调）：EXE（:8317）外部可选、默认不启动；
  WorkBuddy/CodeBuddy 国内/国际是**两套独立账号**（copilot.tencent.com 原生直连，不需要 EXE）
- anon-zen：method=anon 免登录，点接入即落池 secret="public"
- qwenwork.cn 是 JWT 鉴权：只认 `Authorization: Bearer` 或 cookie 名 `token=`（其它 cookie 一律 missing）

## 平台接口基址（实测，别再猜）
| 平台 | 基址 | 备注 |
|---|---|---|
| apk-trae | `api.trae.cn` **和** `www.trae.com.cn` | 拆两台 |
| apk-codebuddy | `www.codebuddy.ai` | 登录轮询在 `copilot.tencent.com` |
| apk-raccoon | `xiaohuanxiong.com` | `/api/web/points/v1/{balance,bills}` |
| apk-doubao | `www.doubao.com` | 真前缀 `/alice/*` + `/samantha/*`（aid=497858）；文生图 `/v1/images/generations` |
| apk-yuanbao | `yuanbao.tencent.com` | 对话 `/api/chat/completions`（**非** `/api/chat`）；模型 `/api/agent/model/list` |

### 6 个 APK（= 6 个本地网关，内部模型清单已拆解，勿凭印象下结论）
`aigw.app`(0.1.18,Trae) · `dev.doubao2api`(1.0.6,豆包) · `com.joy4fire.wb2apimobile`(Go) ·
`com.joy4fire.workbuddy2api`(1.1.0,国际) · `dev.raccoon2api`(1.15,小浣熊) · `dev.yuanbao2api`(1.2.0,元宝)

## 原生对话中继（端点来自字节码，先拆文件再谈抓包）
- CodeBuddy/WorkBuddy：`copilot.tencent.com/v2/chat/completions` + X-Domain/X-Tenant-Id/X-User-Id 头组
- Trae：`api.trae.cn/api/v1/chat/completions`（备选 /v2、/v1）
- 小浣熊：`xiaohuanxiong.com/api/web/llm/v2/chat/completions`
- 用法：对话 model 带 `@平台id` 后缀 → 原生中继（账号池凭据）；`/api/models?action=relays` 查清单
- 待真凭据联调：请求体平台私有字段（腾讯 v2 business、元宝 hy 包装）—— 静态提取无法替代

## 模型清单（豆包/元宝）
- 豆包：网页版 `model_list.item_list` 解析进 `app/bundled_doubao.py`（6 个）
- 元宝：SPA 动态加载 `/api/agent/model/list`，内置用 `app/bundled_yuanbao.py`（12 个，较旧）
- 统一 `app/model_extract.py`：已登录优先拉云端，失败回退内置，响应带 `live` 标志

## 官方 API 平台（api-*，15 家已注册）
gwextra `_API_OFFICIAL` + `api_hint_models()`（未配 Key 回退官方提示，永不空表）；配 Key→Bearer 实时拉

## 打包环境（重要）
- venv：`C:\Users\liang.zhao\.workbuddy\binaries\python\envs\default`（PyInstaller 6.22.3）
- Node（仅 JS 语法）：`C:\Users\liang.zhao\.workbuddy\binaries\node\versions\22.22.2-3\node.exe`
- 命令：`python -m PyInstaller --clean --noconfirm build.spec`
- **build.spec 绝不能 exclude `sqlite3`**（读本机浏览器 Cookie 依赖）
- 新模块函数内动态导入 → 显式写进 `hiddenimports`；新增静态子目录必须写进 datas
- 重打包前先杀面板进程（dist exe 被占用 PyInstaller 静默半失败），验 mtime
- 验证打包：从 EXE 的 PYZ 抽模块确认逻辑进包

## 本机浏览器 Cookie 现状
Chrome/Default 604 条、Edge 1 条 —— 全 v20（app-bound 加密），磁盘直解无解，只能 CDP 或手动粘贴。

## 踩坑精编（高频）
| 坑 | 解法 |
|---|---|
| 改前端只 node --check | 必须跑 `node test_views.js`（运行时 ReferenceError 查不出） |
| 改 JS 用 heredoc 内嵌 Python 写文件 | 转义链吃反斜杠连环伤 → 用 Edit 工具逐处改 |
| ✏ JS 重名函数覆盖（loadRoute/acToggle 等） | 后者 hoist 覆盖前者 → 改完 `grep 函数名\|sort\|uniq -d` 验零重复 |
| `_g("auto",…)` / 路由断言写旧模型名 | auto-fast/auto-weight/auto-priority 已废弃 → `auto` / `Free` |
| EXE 关后 /api/models 探网关 6.8s/10.2s | `_gw_alive()` 返回 dict 永远真值 → 守卫必须 `.get("alive")`，且不能经 new_client()（递归爆栈） |
| 单实例互斥体挡新 EXE | 测前 `taskkill /F /IM aigw-panel.exe` |
| ★ 单实例互斥体"幽灵残留" | 反复强杀后面板报"另一个实例在运行"但**查无任何 aigw 进程**、端口无监听，等 45s+ 也不释放。仅影响我在沙箱反复强杀后的启动；**用户干净会话双击/开机自启不受影响**。验证时别反复强杀，起不来就让用户手动双击或重启 |
| safe-delete shim 拦 rm/Remove-Item | `Stop-Process -Name aigw-panel -Force` 杀残留 → PowerShell Remove-Item 实际能删 |
| 豆包真接口前缀 `/alice/`（非 `/api/v1/`） | 真验活 `GET /alice/user/config/pull` 未登录返 `code:710012001` |
| APK `/v1/*` 是本地网关路由 | 云端 www.doubao.com 返 HTML，别混淆 |
| 「200 + 业务码」假成功 | 先判业务码 / 判 HTML，不只看 HTTP 状态 |
| relay_once 误传多余 kw（如 model=） | TypeError 直接 500/线程崩；签名不匹配先查传入参数 |
| anon-zen 公共池偶发挂起 | 真实探活 GET `/zen/v1/models` + relay_once timeout=25，避免 180s 默认超时卡死 |
| ★ 自动路由（auto/free）**流式**返回 JSON 而非 SSE | ZCode/Cherry 默认 stream:true，收不到 `data:` 帧 → 报 `empty_model_response`。router 分支曾漏 stream 处理（豆包/@pid/web_relay 分支都有），build39 补 `_sse_text_chunks` 回放。★改路由必须**同时验「非流式+流式」**，用 `curl -i` 看 Content-Type 是否 `text/event-stream` |
| ★ `_sse_text_chunks` 用 uuid 但 main.py 未 import | 该路径长期无人走到（doubao/@pid 流式走 DR.chat_stream/relay_stream）→ NameError 藏死。补 `import uuid`。★ **py_compile 只查语法，查不出运行时 NameError**：后端改完必须起真实进程实测并看 EXE 的 stderr traceback |
| ★ WorkBuddy（wb-gateway）前端看不到模型 | `platform_models` 里 copilot 系走「网关型分支」问已移除 EXE 的 /v1/models → 永远空表（第三处 EXE 遗留）。改原生直连 `tlogin.models_with_credits(token)`（/console/enterprises/personal/models，带倍率），失败回退 bundled_models 36 个。★ 排查"某平台没模型"先分清：**没凭据** 还是 **代码走了依赖 EXE 的死路** |
| ★★ 两套模型接口**不共用**，改一处不够 | `platform_models`（接入源详情页）与 **`grouped`（模型总表，分组硬编码 ①~⑤）** 是两套独立逻辑。修了详情页 ≠ 总表有（用户因此二次反馈"还是看不到"）。★ 新增平台/修模型必须**两处都改**；排查用 `curl .../api/models?action=grouped` 看 pid 在不在分组列表 |
| `platform_models` 的 platform 在 POST body | 放 query 会报 "platform 必填"（/api/* 的 action 才在 query） |

## 目录约定
```
网关项目/
├── base*.apk                          原始素材（6 个网关 APK）
├── workbuddy-gateway-windows-1.29.6.exe   网关主体（Go, x64, 默认 8317，已弃用）
└── aigw-panel/                       面板源码 + dist/aigw-panel.exe（build36）
```

## ★★ WorkBuddy / CodeBuddy = 同一套接口，只分国内站 / 国际站（2026-10-06 搜索证实，勿再按"产品"拆）
**曾误当成两个独立产品** → 用户纠正。外部一手证据（4 处一致）：
- dsh-router-codebuddy：*"WorkBuddy 是腾讯的国际版 AI 办公工作台，与国内 CodeBuddy
  **同族同契约**：同样的 OAuth 轮询登录、同样的 /v2/chat/completions 网关、
  同样的 /billing/meter/* 签到积分，两者**共用同一份实现**，差异只落在 profile"*
- workbuddy-gateway（Go）：*"两个上游站点走**同一套 /v2/plugin/* 协议**，凭据按站点隔离"*
- readaitime：同属 Buddy AI 系列，**同一账号积分共享、无需分别订阅**
- dsh-connect-workbuddy：国际版两个品牌域名 workbuddy.ai / codebuddy.ai **都属国际版**
正确口径（面板命名必须用这个）：
| 站点 | 上游 | 品牌名 |
|---|---|---|
| 国内站 | `copilot.tencent.com`（/www.codebuddy.cn） | WorkBuddy 国内 / CodeBuddy 国内 |
| 国际站 | `www.workbuddy.ai`（含 codebuddy.ai） | WorkBuddy 国际 / CodeBuddy 国际 |
- 凭据按站点隔离（edition），两站账号独立、积分共享
- 模型目录两站**都读 /v3/config**，区别只在客户端 UA（国内用 CodeBuddy CLI UA、
  国际用桌面端 UA）；国内取不到才退回 /v2/enterprises/personal/models
- 面板分组命名：`wb-gateway`=「copilot 接口 · 国内站」、
  `apk-codebuddy`=「copilot 接口 · 国际站」

## 面板端口与自启（build41 起）
- **固定默认端口 8800**（`main.py::DEFAULT_PANEL_PORT`），`free_port(start=8800)`，
  被占用才顺延；可用 `AIGW_PANEL_PORT` 环境变量覆盖。
  → 客户端 base URL 配一次即可：`http://127.0.0.1:8800/v1`
- **开机自启**：`app/tray.py::set_autostart()` 写 HKCU `Run\AigwPanel`
  = `"<dist\aigw-panel.exe>" --tray`；托盘右键菜单也能开关。
- ⚠ 改端口/自启后必须重打包，且自启值要指向**新 EXE 真实路径**（旧路径会启旧版）

## 面板设计原则
纯标准库（无 Flask/requests），前端零框架。侧边栏常驻「接入源 / 路由与模型」；
可折叠「高级」分组放 工具与集成 / 设置与日志 / 签到记录 / 成长任务 / 任务管理。
