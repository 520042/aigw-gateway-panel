# 网关项目 · 项目长期记忆

## 项目定位
把工作区里 7 个「免费 AI 额度本地化」网关程序（1 个 Go EXE + 6 个 Android APK）
和网上的公益中转站 / 官方免费额度平台，统一收进一个本地面板管理。

## 目录约定

```
网关项目/
├── base*.apk                        原始素材（6 个网关 APK）
├── workbuddy-gateway-windows-1.29.6.exe   网关主体（Go, x64, 默认 8317）
├── workbuddy-gateway-x86-1.29.6.zip      已损坏（%TSD-Header-###% 代理注入）
├── 工作区文件解析报告.md             APK/EXE 反解结果
├── 测试报告.txt                     35/35 自测记录
└── aigw-panel/                     面板源码 + dist/aigw-panel.exe
```

## 关键事实（实测，别再重复踩）

### 网关鉴权
- `POST /admin/api/setup {"username","password"}` → 首次初始化
- `POST /admin/api/login/key {"username","password"}` → 登录（**字段是 password，用 key 报 invalid_password**）
- 之后 `/admin/api/*` 带 Cookie；未登录 `login_required`，未初始化 `setup_required`
- `/v1/*` 走 Bearer，key 默认 `admin`

### 网关账号登录（二维码）—— 这才是"获取 token"的真正入口
```
POST /admin/api/login/start  {"edition":"cn"|"intl"}
  → {id, edition, siteLabel, status, message,
     qr:"data:image/png;base64,...",
     authUrl, startedAt, expiresAt, secondsLeft}
GET  /admin/api/login/poll?id=<id>   ← **仅 GET，POST 返回 405**
  → 同上，status: pending → success
```
- cn → `copilot.tencent.com/login?platform=VSCode&state=`，有效期 300s
- intl → `www.workbuddy.ai/login?platform=workbuddy-ai&state=`，有效期 900s
- 面板已完整驱动该流程（二维码内嵌显示 + 授权链接 + 2s 轮询），不要再让用户去敲 `workbuddy-gateway login`

### 网关内置调度
签到 09:00 / 成长 10:00 / 凭据热加载 5s / 模型目录刷新 60min / 用量保留 90 天
冷却：账号级 60s，模型级 600s。互斥体 `Local\WorkBuddyGatewayMutex`。

### 网关程序启动
```bash
workbuddy-gateway-windows-1.29.6.exe serve -addr 127.0.0.1 -port 8317 -api-key admin
workbuddy-gateway-windows-1.29.6.exe login          # 浏览器授权拿凭据
workbuddy-gateway-windows-1.29.6.exe login -intl   # 国际站
```

### 6 个 APK 一览
`aigw.app`(0.1.18, Trae, 功能最全) · `dev.doubao2api`(1.0.6, 豆包, 有文生图) ·
`com.joy4fire.wb2apimobile`(Go) · `com.joy4fire.workbuddy2api`(1.1.0, CodeBuddy 国际) ·
`dev.raccoon2api`(1.15, 小浣熊, 有登录送积分) · `dev.yuanbao2api`(1.2.0, 元宝)

### NewAPI 系签到接口
- `GET /api/user/checkin/status` → `data.stats.{checked_in_today,consecutive_days,total_days}`
- `POST /api/user/checkin` → `data.{consecutive_days,reward}`
- AnyRouter 路径不同：`/api/user/sign_in`
- 认证：Token + `new-api-user` 头 ＞ Cookie ＞ 账密 `POST /api/user/login`
- quota 换算：`500000 quota = $1`（按站点 `quota_per_unit`）

## 本项目踩过的坑

| 坑 | 现象 | 解法 |
|---|---|---|
| `os.path.abspath(__file__ + "/..")` | data 目录建错位置 | 必须先 `dirname(abspath(__file__))` |
| `APP["sched"]` 命名 | 与 `threading.Thread` 属性打架 | 改 `APP["scheduler"]` |
| `safe()` 忘了解包 tuple | `AttributeError: 'tuple' has no attribute 'get'` | 统一返回 `(body, status)`，另加 `safe_dict()` |
| 用 body 有无 `ok` 判成败 | 网关成功响应无 `ok` 字段 → 误报 502 | 按 HTTP 状态码判 |
| bash heredoc 放复杂正则 | `unterminated character set` | 写 `.py` 文件执行 |
| `re.finditer()` 返回 Match | `.decode()` 报 AttributeError | 用 `.group().decode()` |
| AXML 二进制 Manifest | 拿不到包名/组件 | 自写字符串池解析器（chunk 0x0001，UTF8 flag 分支） |
| PyInstaller 静态资源 | 打包后 404 | 候选列表探测 `HERE/app/static` 与 `_MEIPASS/app/static` |
| EXE 在 dist/ 找不到网关 | 显示「未找到」 | 向上 5 层 + 环境变量 `AIGW_GATEWAY_PATH` + 浅层 walk 三级搜索 |
| 调度器启动即补跑签到 | 刚开面板就签 | 45 秒静默观察期 |
| zip 首部 `%TSD-Header-###%` | BadZipFile | TSD 透明代理注入，用等价 exe |
| **JS 箭头函数漏括号** | `const line=key,max=>…` 解析成三个 const 声明，浏览器**静默拒绝整个文件** | 写成 `const line=(key,max)=>…`；**必须 `node --check app.js` 验语法** |
| 自测不查 JS 语法 | 页面卡在"正在加载"，接口全 200 也没发现 | `selftest.py` 已接入 `node --check` 作为第 40 项断言 |
| `node --check` 定位语法错 | 只报"第几行"不给原因 | 比浏览器 `eval` 精确得多，优先用 |
| `probe_target` 用 `self.endpoint` | Router 没有该属性，后台探测线程静默死掉 | 用 `t.endpoint`；且 worker 内必须 try/except |
| `Target.__slots__` 挡动态属性 | 测试里加 `capabilities` 报 no `__dict__` | 补进 `__slots__` + `__init__` 参数 |
| `probe_all` 按 endpoint 去重 | 只探第一个成员，其余 Target 永远无 EWMA | 分组探测后**回填指标给所有成员** |
| `fastest` 把无延迟数据排最前 | 刚配置的上游抢跑 | 无 EWMA 排最后 |
| 模块级代码顺序 | `NODE = … shutil_which_node()` 在函数定义前 → NameError | 函数先定义再调用 |
| `guard.acquired()` | 写成读属性，`acquired` 是 bool 字段不是方法 | 属性名别和方法名撞车 |
| **AES-GCM `_inc32` 掩码写窄** | 写成 `0xFFFFFFFF00000000`（64 位）会把 128 位计数器高 64 位清零 → 第 2 块起 keystream 全错 | 必须 96 位：`0xFFFFFFFFFFFFFFFFFFFFFFFF00000000` |
| **AES-GCM keystream 起点** | 从 `J0` 开始（错），空明文侥幸能过，非空全错 | 明文用 **`inc32(J0)`**，只有算 Tag 才用 `J0` |
| **`_gmul` 方向别乱改** | 曾误判成"位序错"改成左移版（R=0x87），**改反了** | 正确的是**右移版 R=0xE1<<120** 配 `int.from_bytes(big)`；aesgcm.py 里已加警告注释 |
| **自洽测试发现不了规范错误** | 自己加密自己解密，计数器写错照样通过 | 用 **Windows BCrypt(CNG)**（`bcrypt_ref.py`）做黄金参考交叉验证 |
| **凭记忆写测试向量** | 记错 NIST TC3 的 64 字节十六进制，误以为实现有 bug，白查很久 | 长向量别靠记忆；改用 OS 原生实现交叉验证 |
| **Chrome v20 app-bound 加密** | 磁盘 Cookie 全解不开 | 无解，改走 CDP（`Network.getAllCookies`）或手动粘贴 |
| **改控制台密码后登录静默失效** | `client_factory` 用启动时那份空密码 | 每次 `reconfigure()` 最新 settings（已修，改完无需重启） |
| **前端自动轮询会清空输入** | `render()` 重绘把 textarea 内容冲掉 | Cookie 类只在点击时 poll；仅二维码类自动轮询 |
| **ctypes 托盘六个坑** | 见下 | 纯 ctypes 实现，不用 pystray |

## ctypes 托盘（app/tray.py）踩坑

| 坑 | 现象 | 解法 |
|---|---|---|
| `ctypes.wintypes` 无 `WNDCLASSW` | AttributeError | 自己定义结构体 + `WNDPROC = WINFUNCTYPE(...)` |
| 窗口过程回调被 GC | 赋完 `lpfnWndProc` 立即失效 | `proc = WNDPROC(self._wndproc); self._proc = proc` 存引用 |
| `GetModuleHandleW` | user32 上找不到 | 在 **kernel32** |
| `ExtractIconExW` | user32 上找不到 | 在 **shell32** |
| `DefWindowProcW` 无 argtypes | `OverflowError: int too long` | `argtypes=[HWND,c_uint,WPARAM,LPARAM]`，restype `c_ssize_t` |
| `CloseHandle` 无 argtypes | HANDLE 截断，互斥体释放不掉 | `argtypes=[HANDLE]`；且 acquire 失败要立刻关句柄 |
| 气泡通知同步 sleep | 调用方阻塞 9 秒 | 立即返回，NID 数据函数返回前已拷走 |

## 为什么无窗口的 exe 任务栏没图标

`console=False` → PE Subsystem=2（GUI 子系统）→ **没有主窗口** →
Windows 不分配任务栏位置。解法两条：
1. `SetCurrentProcessExplicitAppUserModelID(APP_ID)` 让任务栏正确分组
2. 自建消息窗口 + `Shell_NotifyIconW` 挂托盘，有了持续窗口图标就不消失

## 倍率（credits）—— 权威来源与三源合并

**别再去猜接口，倍率表在 APK 里**：
`base(3).apk → assets/codebuddy-international-models.json`（18286 B），
标注 `@tencent-ai/codebuddy-code@2.150.0` / `2026-09-13`，
**36 个模型，35 个带 `credits`**，另有 `maxInputTokens` / `maxOutputTokens` /
`supportsImages` / `supportsToolCall` / `supportsReasoning` / `vendor`。
用 `gen_bundled_models.py` 生成 `app/bundled_models.py`（生成物，勿手改）。

三源优先级（`main.py::_merge_model_rates`）：
1. 网关实测 `cost` —— **只有数字才算**，`"未观测"` 和 `0` 都不算
2. 在线目录（登录后拉）`/console/enterprises/personal/models`
3. 内置倍率表（免登录，恒可用）

两套 id 体系必须映射：内置表是 APP 槽位名（`default-model`），
网关是上游真实名（`default`）。四级匹配 = 精确 → 别名(`_ALIASES`) → 归一化 → 同族前缀。
实测 31 个网关模型命中 18 个；未命中的 13 个是国内站独有（`glm-4.7`/`hunyuan-chat`/`minimax-m2.5`…），
内置表是国际站的覆盖不到，只能靠在线目录补。

## 平台接口基址（实测，别再猜）

| 平台 | 基址 | 备注 |
|---|---|---|
| apk-trae | `api.trae.cn` **和** `www.trae.com.cn` | **拆两台主机**！签到/权益/用量在 `api`（挂 `/trae` 前缀），积分/用户/配额/手机登录在 `www`。ACTIONS 用 `base` 字段做 per-action 覆盖 |
| apk-codebuddy | `www.codebuddy.ai` | 登录轮询 `/v2/plugin/auth/token` 在 **`copilot.tencent.com`**；`auth/state` 已下线 404 |
| apk-raccoon | `xiaohuanxiong.com` | `/api/web/points/v1/{balance,bills}`、`/api/web/auth/v1/entitlement_info`、`/api/web/office/v3/setting_info` |
| apk-doubao | `www.doubao.com` | 有文生图 `/v1/images/generations`、对话 `/samantha/chat/completion` |
| apk-yuanbao | `yuanbao.tencent.com` | 对话是 `/api/chat/completions`（**不是** `/api/chat`） |
| apk-go | — | `copilot.tencent.com` 是官网，`/v1/*` 全 404，走本地网关无公开 REST |

Trae 的 `points/activation`、`redemption-codes/redeem`、`team-points/balance`
是 **APP 本地网关路由**（公网连 `www.trae.ai` 也 404），标 `local: True` 前端灰掉。

## 面板启动与测试的坑

| 坑 | 解法 |
|---|---|
| `--port 8801` **不被解析** | 端口只认环境变量 `AIGW_PANEL_PORT`；`--no-tray` 也不认（只认 `--tray`） |
| bash 后台进程被回收 | 用 `run_panel_test.py` 里 subprocess 拉起 + 测完 terminate |
| `tasklist` 输出是 **GBK** | `subprocess(text=True, encoding="gbk", errors="replace")`，否则正则匹配不上导致 taskkill 没执行 |
| 网关限流 `too_many_attempts` | 网关登录失败计数是**内存态**，`taskkill` + 重启即清 |
| EXE 残留锁单实例 | 测 EXE 前先 `taskkill /F /IM aigw-panel.exe` |
| 自测顺序导致 502 | selftest 开头加「前置·拉起网关」 |

## 关键文件（v1.2）
| 文件 | 职责 |
|---|---|
| `app/tray.py` | 纯 ctypes 托盘 + 单实例 + 开机自启（HKCU Run） |
| `make_icon.py` | 自绘 PNG → 自拼多尺寸 ICO（不用 Pillow） |
| `test_tray.py` | 19 项托盘测试 |
| `build.spec` | 含 `icon='app/static/aigw.ico'` |

## 中转与路由（v1.1 新增）

### 三个自动模型
| 模型 | 策略 | 规则 |
|---|---|---|
| `auto-fast` | fastest | EWMA 最低优先，无延迟数据排最后 |
| `auto-weight` | weight | 站点权重高优先，同权重比延迟 |
| `auto-priority` | priority | 优先级数值升序，失败才降级 |

### 降级规则
- 2xx → 记录延迟
- **429 / 5xx / 超时** → 熔断计数 +1，自动降级下一个（最多 4 个）
- **4xx（除 429）** → 直接报错不降级
- 熔断：连续 3 次 → OPEN，60s 后可再选（懒判断，state 到下次成功/失败才改）

### 会话亲和
请求体带 `aigw_session` → 同 ID 复用上次上游，避免多轮对话上游乱跳。
策略可单次覆盖：`aigw_strategy: fastest|weight|priority`。

### 能力过滤
请求带 `tools` → 排除 `capabilities.tools === false`；带图片 → 排除 `vision === false`。

## 关键文件
| 文件 | 职责 |
|---|---|
| `app/upstreams.py` | 38 个上游结构化档案（7 网关 + 17 公益站 + 14 官方 + 8 工具） |
| `app/router.py` | 路由引擎：探测 / EWMA / 熔断 / 选路 / 降级 |
| `app/catalog.py` | 面板导航用的展示数据 |
| `app/gwlogin.py` | **8 个平台的账号登录驱动**（二维码 / Cookie / 文件），状态机 + LoginManager |
| `app/accounts.py` | 账号池：落库 data/accounts.json，列表掩码 |
| `app/gwextra.py` | 补齐 APP 网关内部能力（Trae 签到/任务/积分/兑换、小浣熊送积分等） |
| `app/aesgcm.py` | **纯标准库 AES-GCM**（解 Chrome Cookie 用），22 项自检 |
| `app/browser_cookie.py` | 本机 Chrome/Edge Cookie 读取（DPAPI + AES-GCM）+ diagnose() 诊断 |
| `app/cdp.py` | 最小 WebSocket + CDP 客户端，绕过 Chrome v20 加密取明文 Cookie |
| `bcrypt_ref.py` | **仅测试用**：Windows BCrypt 原生 AES-GCM，作交叉验证黄金参考 |
| `test_router.py` | 路由引擎单元测试（21 项，起假上游） |
| `test_login.py` | 登录模块单测（75 项，含 BCrypt 交叉验证、WS 帧编解码） |
| `test_view.js` + `test_view_body.js` | 前端渲染测试（20 项，Node + DOM stub，含 XSS 转义） |
| `selftest.py` | 接口回归（51 项，含 node --check JS 语法 + 登录全流程） |

## 本机的浏览器 Cookie 现状（实测 2026-10-04）
- Chrome/Default 604 条、Edge/Profile 3 1 条 —— **全部是 v20**（Chrome 127+ app-bound 加密）
- v20 **无解**（密钥绑定系统级 DPAPI，官方刻意封死），只能走 CDP 或手动粘贴
- 老版本 v10/v11 的磁盘直解代码保留着，换机器/旧浏览器时能用

## 打包环境
- venv：`C:\Users\liang.zhao\.workbuddy\binaries\python\envs\default`
- PyInstaller 6.22.3 已装
- Node（仅用于 JS 语法检查）：`C:\Users\liang.zhao\.workbuddy\binaries\node\versions\22.22.2-3\node.exe`
- 打包命令：`python -m PyInstaller --clean --noconfirm build.spec`
- 产物：约 10.3 MB 单文件 PE32+ x64（放开 sqlite3 + 新增 6 个模块后从 8.8MB 涨上来）
- **`build.spec` 绝不能 exclude `sqlite3`** —— 读本机浏览器 Cookie 依赖它
- 新模块多在函数内动态导入，已显式写进 `hiddenimports`

## 面板设计原则
- 纯标准库（无 Flask/requests），打包干净
- 前端零框架零构建，SVG 手绘曲线
- 凭据只存本机 `data/`，不外传
- 调度错峰：网关自己 09:00 签到，面板 09:05 跑外部站点，不撞车
