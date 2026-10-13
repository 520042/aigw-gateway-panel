# 网关项目 · 项目长期记忆

## 定位
把多个「免费 AI 额度 → 本地 OpenAI 端点」网关（1 个 Go EXE + 6 个 Android APK）和公益中转站 / 官方免费额度平台，统一收进一个本地面板管理。
**核心诉求：只启动一个软件，面板自己干完所有事，不依赖 workbuddy-gateway 进程。**
**网关 EXE 已彻底移除（build26），面板纯原生模式，自身即本地网关**（`/v1` 暴露 OpenAI 兼容端点：`/v1/chat/completions`、`/v1/models`、`/v1/messages` Anthropic 兼容）。

## ★★ WorkBuddy / CodeBuddy = 同一套接口，只分国内站 / 国际站
**用户明确纠正**：曾误当成两个独立产品。外部一手证据（4 处一致：dsh-router-codebuddy / workbuddy-gateway(Go) / readaitime / dsh-connect-workbuddy）。
- 国内站上游 `copilot.tencent.com`（/www.codebuddy.cn）= WorkBuddy 国内 / CodeBuddy 国内
- 国际站上游 `www.workbuddy.ai`（含 codebuddy.ai）= WorkBuddy 国际 / CodeBuddy 国际
- 凭据按站点隔离（edition），两站账号独立、积分共享；共用同一份实现（OAuth 轮询 + /v2/chat/completions + /billing/meter/* 签到）
- 模型目录两站都读 `/v3/config`（国内取不到才退回 `/console/enterprises/personal/models`）；区别只在客户端 UA
- 面板分组命名：`wb-gateway`=「copilot 接口 · 国内站」、`apk-codebuddy`=「copilot 接口 · 国际站」

### ★★★ 硬规则：UI 只显示「国内站 / 国际站」两行（2026-10-14 用户当面再纠正）
- WorkBuddy 与 CodeBuddy **是同一个东西**，合并成一张卡，**只分国内版 / 国际版 2 行**。
  绝不出现 4 行（不要给 `apk-codebuddy` / `apk-codebuddy-cn` 单独出行）。
- `apk-codebuddy` / `apk-codebuddy-cn` 是 `wb-gateway-intl` / `wb-gateway` 的**别名**，
  `gwextra.py:530` 明确「合并为国内站/国际站两个入口，去掉 apk-codebuddy/-cn 重复项」。
  → **不要给它们加 `gwlogin.PLATFORMS` 规格**（加了会多出卡/多出行，且违反合并要求）；
  它们不进 `api/login?action=platforms` 是**设计如此，不是遗漏**。
- ⚠️ 云端自动摘要里那句「WorkBuddy/CodeBuddy 国内/国际必须作为独立账号行展示，不能合并」
  是**过期错误记录**，与本项目 `MEMORY.md` 及用户本人口径冲突。**以本段为准**。
  （2026-10-14 我误信该摘要改成 4 行，被用户当面纠正并已回退。）
- 用户不需要 WorkBuddy Gateway（Go EXE）——已彻底移除，任何场景都不要再引入或提及它。

## 路由现状（auto + free 两模型）
- `auto`：strategy=fastest；`free`：strategy=free（优先 anon-zen / 豆包）
- 两者各挂 `native:anon-zen`（OpenCode Zen 公共免费池，Bearer public，免登录）作常驻兜底 + `native:apk-doubao`（豆包原生中继，free=True）
- `Target.free` 属性；`pick()` 支持 fastest/weight/priority/**free**；free 候选排前，非 free 兜底排后
- 降级：429/5xx/超时 熔断+1（最多 4）；4xx(除 429) 直接报错；连续 3 次→OPEN，60s 后可再选
- 会话亲和：请求体 `aigw_session` 复用上游；`aigw_strategy` 单次覆盖
- ⚠ 旧 EXE 不认新 `auto`/`free` 模型 → 改完路由必须重打包+重启面板

## 关键模块职责
| 文件 | 职责 |
|---|---|
| `app/tlogin.py` | ★ 原生登录链路（state/URL/换 token/拉倍率/合并模型），copilot 系 |
| `app/gwlogin.py` | 登录驱动（二维码/Cookie/文件/anon）；`verify` 实测校准 |
| `app/cdp.py` | WS+CDP+Browser；Cookie 明文；`cookie_header` 双向域匹配 |
| `app/gwextra.py` | ★ 平台动作注册表 / `_API_OFFICIAL`+`api_hint_models` |
| `app/model_extract.py` | ★ 模型清单实时提取 + 内置回退（豆包/元宝/通用响应形状） |
| `app/router.py` | 路由引擎（fastest/weight/priority/free + native 目标） |
| `app/doubao_relay.py` | 豆包 samantha 原生中继（协议逆向） |
| `app/native_relay.py` | 多平台原生中继（anon-zen 指纹头 + kilo 免凭据 + free_models 动态清单） |
| `app/web_relays.py` | 网页版反代（web-glm X-Sign / web-trae / web-deepseek POW via Node+WASM） |
| `app/acct_pool.py` | 通用账号池（轮询/冷却 soft 60s·hard 12h/请求级换号 3 次） |
| `app/accounts.py` | 账号池 `data/accounts.json` |
| `app/tray.py` | 纯 ctypes 托盘 + 单实例 + 开机自启 |
| `app/sources.py` | 分类 LOCAL/API/WEB + 本地反代档案 |
| `app/autocheckin.py` | 平台定时签到（数据驱动） |

## 平台接口基址（实测，别再猜）
| 平台 | 基址 | 备注 |
|---|---|---|
| apk-trae | `api.trae.cn` + `www.trae.com.cn`（拆两台） | 登录轮询在 `copilot.tencent.com` |
| apk-doubao | `www.doubao.com` | 真前缀 `/alice/*` + `/samantha/*`（aid=497858）；文生图 `/v1/images/generations` |
| apk-yuanbao | `yuanbao.tencent.com` | 对话 `/api/chat/completions`；模型 `/api/agent/model/list` |
| apk-raccoon | `xiaohuanxiong.com` | `/api/web/points/v1/{balance,bills}` |
| apk-codebuddy | `www.codebuddy.ai` | 国际站，同 copilot 系协议 |
| 6 个 APK | `aigw.app`(Trae)·`dev.doubao2api`(豆包)·`com.joy4fire.wb2apimobile`(Go)·`com.joy4fire.workbuddy2api`(国际)·`dev.raccoon2api`(小浣熊)·`dev.yuanbao2api`(元宝) | 内部模型清单已拆解 |

## 登录链路关键坑
- copilot 系原生扫码（免 EXE 免跳页）：`_start_native_qrcode` → 前端出码 → `_poll_native` 轮询 `/v2/plugin/auth/token?state=` → token 落账号池（secret=accessToken）
- poll 竞态：无 gw_session 时静默等下一轮（勿误报「缺少登录会话 id」打终态）
- **本地网关 EXE ≠ WorkBuddy 账号**：EXE（:8317）外部可选、默认不启动；WorkBuddy/CodeBuddy 国内/国际是独立账号（copilot.tencent.com 原生直连，不需要 EXE）
- anon-zen：method=anon 免登录，点接入即落池 secret="public"
- qwenwork.cn 是 JWT 鉴权：只认 `Authorization: Bearer` 或 cookie 名 `token=`

## 打包环境（重要）
- venv：`C:\Users\liang.zhao\.workbuddy\binaries\python\envs\default`（PyInstaller 6.22.3）
- **正规构建命令（唯一）**：`python build_appdata.py`
  —— 它把 `--workpath/--distpath` 都放在 `%LOCALAPPDATA%\aigw-panel-build`（豁免区），
  再同步到安装位 `%LOCALAPPDATA%\aigw-panel\`（保留 data/），并做产物完整性终检。
- ⚠️ **禁止**在项目目录里直接 `python -m PyInstaller ... build.spec`（如 `--distpath out2/out3`）：
  产物会被 TSD/沙箱干扰而损坏，表现为用户双击报
  **「Failed to start embedded python interpreter!」**（2026-10-14 实测踩坑）。
  判据：`build_appdata.py` 会打印「产物全部干净 / base_library.zip 合法」；
  手工构建没有这道检查，损坏不易察觉。
- 构建后必看：`已同步 N 个文件` + `OK：N 个文件全部干净，base_library.zip 合法`。
- **build.spec 绝不能 exclude `sqlite3`**（读本机浏览器 Cookie 依赖）
- 新模块函数内动态导入 → 显式写进 `hiddenimports`；新增静态子目录必须写进 datas
- 重打包前先杀面板进程（dist exe 被占用 PyInstaller 静默半失败），验 mtime
- Node（仅 JS 语法检查）：`C:\Users\liang.zhao\.workbuddy\binaries\node\versions\22.22.2-3\node.exe`

## 面板端口与自启（build41+）
- **固定默认端口 8800**（`main.py::DEFAULT_PANEL_PORT`）；`AIGW_PANEL_PORT` 可覆盖
  → 客户端 base URL 配一次：`http://127.0.0.1:8800/v1`
- **开机自启**：`app/tray.py::set_autostart()` 写 HKCU `Run\AigwPanel` = `"<dist\aigw-panel.exe>" --tray`
- ⚠ 改端口/自启后必须重打包，自启值指向新 EXE 真实路径

## ★ CLIProxyAPI 的去留（2026-10-07 定案：默认不内置）
- 反代 Kimi/Codex/Claude/Antigravity/Grok/Devin/Meta 的订阅 OAuth 车道，与原生中继不重叠；默认 `include_cliproxy=False`
- 原生替代已写好但未接线：`app/relays/`（kimi/codex/claude/xai/devin/meta/antigravity），接口 `start_device_flow/poll_token/refresh_token/chat_completions`（claude=`messages()`、antigravity=`generate_content()`），当前零引用

## 踩坑精编（高频）
| 坑 | 解法 |
|---|---|
| 改前端只 node --check | 必须跑 `node test_views.js`（运行时 ReferenceError 查不出） |
| JS 重名函数覆盖（loadRoute/acToggle 等） | 后者 hoist 覆盖前者 → 改完 `grep 函数名\|sort\|uniq -d` 验零重复 |
| 路由断言写旧模型名 | auto-fast/auto-weight/auto-priority 已废弃 → `auto` / `free` |
| `_gw_alive()` 返回 dict 永远真值 | 守卫必须 `.get("alive")`，且不能经 new_client()（递归爆栈） |
| ★ 单实例互斥体"幽灵残留" | 反复强杀后面板报"另一个实例在运行"但查无 aigw 进程、端口无监听，45s+ 不释放。仅影响沙箱反复强杀；**用户干净会话双击/开机自启不受影响**。验证别反复强杀 |
| 豆包真接口前缀 `/alice/`（非 `/api/v1/`） | 真验活 `GET /alice/user/config/pull` 未登录返 `code:710012001` |
| 「200 + 业务码」假成功 | 先判业务码 / 判 HTML，不只看 HTTP 状态 |
| relay_once 误传多余 kw（如 model=） | TypeError 直接 500/线程崩；签名不匹配先查传入参数 |
| anon-zen 公共池偶发挂起 | 真实探活 GET `/zen/v1/models` + relay_once timeout=25，避免 180s 默认超时 |
| ★ 自动路由（auto/free）**流式**返回 JSON 而非 SSE | ZCode/Cherry 默认 stream:true，收不到 `data:` 帧 → `empty_model_response`。router 分支必须同时处理非流式+流式（用 `curl -i` 看 Content-Type 是否 `text/event-stream`），补 `_sse_text_chunks` 回放 |
| ★ `_sse_text_chunks` 用 uuid 但 main.py 未 import | py_compile 只查语法，查不出运行时 NameError；后端改完必须起真实进程实测看 EXE stderr |
| ★ 前端看不到某平台模型 | 先分清：**没凭据** 还是 **代码走了依赖已移除 EXE 的死路**（`platform_models` 用原生直连 `tlogin` 而非 EXE /v1/models） |
| ★★ 两套模型接口**不共用**，改一处不够 | `platform_models`（接入源详情页）与 `grouped`（模型总表，分组硬编码）是两套独立逻辑。新增平台/修模型必须**两处都改**；排查 `curl .../api/models?action=grouped` 看 pid 在不在分组 |

## 设计原则
纯标准库（无 Flask/requests），前端零框架。侧边栏常驻「接入源 / 路由与模型」；可折叠「高级」分组放工具与集成 / 设置与日志 / 签到记录 / 成长任务 / 任务管理。
