# aigw2 · 本地 AI 额度聚合网关

把散落的**免费额度 / 官方 API / 客户端订阅 / 公益中转站**收进一个进程，
对外只暴露一个 **OpenAI / Anthropic 双协议端点**。

> 单个 Rust 二进制，`rust-embed` 内嵌前端，不需要 Node/Python/外部网关。
>
> **本目录是项目主线。** Python 版与第一代 Rust 版已归档在 `../legacy/`（只读）。

---

## 目录

- [核心能力](#核心能力)
- [快速开始](#快速开始)
- [环境变量](#环境变量)
- [跨平台部署](#跨平台部署)
- [协议入口](#协议入口)
- [声明式接入（providers/*.toml）](#声明式接入providers)
- [凭据加密](#凭据加密)
- [可观测性](#可观测性)
- [开发](#开发)
- [排障](#排障)

---

## 核心能力

| 能力 | 说明 |
|---|---|
| **单进程自洽** | 一个 `aigw` 二进制就是全部：HTTP 服务 + 面板 + `/v1` 端点。前端 `rust-embed` 内嵌，无外部静态资源 |
| **158 份接入源** | `providers/*.toml` 声明式描述上游（改配置不用改代码） |
| **四协议入口** | `/v1/chat/completions`、`/v1/messages`、`/v1/responses`、`/v1/search/messages` + `/v1/models` |
| **号池六件套** | InFlight 租约 / (账号,模型) 冷却 / WAF 账号级隔离 / RecentPick 防连挑 / 半开放行 / 忘记账号 |
| **失败换号** | 只在「账号自身」类错误上换号（401/403/429/402/5xx），一次请求最多换 N 个号 |
| **声明式协议适配** | `upstream_kind` / `signer` / `models_policy` 驱动，加平台只需一份 toml |
| **热更新** | `max_inflight` / 超时 / 重试次数 / 三档限流，面板改完立即生效，不断流 |
| **凭据加密** | Windows 用 DPAPI；其他平台用 AES-256-GCM + 独立密钥文件 |
| **可观测** | `/api/health` 汇总加密状态 / 后台任务 / 限流现场 / 落盘失败现场 / 构建自证 |

---

## 快速开始

### 从源码构建

```bash
cd aigw2

# 首次：装 Rust（stable 即可，1.75+）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

cargo build --release -p aigw
./target/release/aigw            # 默认 127.0.0.1:8800
```

> **Windows 注意**：必须用 **msvc** 工具链。
> `windows-sys 0.61+` 的 raw-dylib 在 gnu 工具链上要调 `dlltool.exe`：
> ```powershell
> rustup override set stable-x86_64-pc-windows-msvc
> ```
> 或直接用 `build-deploy.ps1`（已显式指定 msvc）。

### 首次运行

```bash
aigw                       # 起服，浏览器开 http://127.0.0.1:8800
aigw --port 8801           # 与旧版/另一个实例并行
aigw --doctor              # 体检：逐个源试拉模型清单，打印谁通谁挂
aigw --doctor --only wb    # 只体检 id 含 "wb" 的源
aigw --help                # 全部参数
```

### 客户端接入

```
base_url = http://127.0.0.1:8800/v1
api_key  = <面板「访问密钥」页生成的 key>     # 未配访问密钥时填任意非空串
model    = 模型名            # 见 /v1/models
         = 模型名@平台ID     # 强制指定平台
         = combo:xxx        # 模型链（跨平台回退）
```

---

## 环境变量

| 变量 | 默认 | 作用 |
|---|---|---|
| `AIGW_DATA_DIR` | 见下表 | **数据目录**。设了就当作独立档案（不做旧数据导入），可用于便携模式/测试实例 |
| `AIGW_KEY_DIR` | 见下表 | **凭据密钥目录**。设了就只认它，不回退 |
| `AIGW_SECRET_KEY` | 无 | **主密钥本身**（64 位 hex = 32 字节）。容器/k8s Secret 用，优先级最高 |
| `AIGW_PANEL_TOKEN` | 无 | 面板令牌。等价于 `--token`，优先级低于命令行 |
| `RUST_LOG` | `info` | 日志级别（`debug`/`trace` 排查时用） |

### 平台默认目录

| | Windows | Linux / macOS |
|---|---|---|
| 配置根 | `%LOCALAPPDATA%` | `$XDG_CONFIG_HOME` → `$HOME/.config` |
| **数据目录** | `%LOCALAPPDATA%\aigw2\data` | `~/.config/aigw2/data` |
| **密钥文件** | `%LOCALAPPDATA%\aigw2\secret.key` | `~/.config/aigw2/secret.key` |

> ★ **Windows 用 `%LOCALAPPDATA%` 而不是 `%APPDATA%`**：这是历史数据真正所在的位置。
> 改到 Roaming 看起来更「标准」，但会让升级后的服务读到一个空目录（账号/密钥/台账
> 全部「消失」），而旧数据仍躺在 Local 下。数据目录迁移必须带迁移代码，不能只改常量。

> 密钥**故意不在 `data/` 里**：`data/` 会被备份、`/api/backup/export`、整包拷走。
> 两者放在同一目录等于没加密。

---

## 跨平台部署

### Windows

**方式 A：绿色版（推荐）**

```powershell
cargo build --release -p aigw
# 产物 target\release\aigw.exe，连同 providers\ 一起拷走即可
.\aigw.exe --port 8800
```

**方式 B：自检安装**

```powershell
.\aigw.exe --install      # 装到 %LOCALAPPDATA%\aigw2 + 桌面/开始菜单快捷方式 + HKCU 自启
.\aigw.exe --uninstall    # 删快捷方式与自启（保留数据）
.\aigw.exe --purge        # 卸载 + 删除数据目录
```

**方式 C：计划任务（开机自启、无窗口）**

```powershell
schtasks /Create /TN Aigw2Service /TR '"%LOCALAPPDATA%\aigw2\aigw.exe" --port 8800' `
         /SC ONLOGON /RL LIMITED /F
schtasks /Run /TN Aigw2Service
```

### Linux（systemd）

```ini
# /etc/systemd/system/aigw.service
[Unit]
Description=aigw2 gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# 建议建专用用户：sudo useradd -r -s /usr/sbin/nologin -m -d /var/lib/aigw aigw
User=aigw
Group=aigw
WorkingDirectory=/opt/aigw
ExecStart=/opt/aigw/aigw --host 127.0.0.1 --port 8800
Restart=on-failure
RestartSec=3

# 数据目录（用默认则会落在 /var/lib/aigw/.config/aigw2/data）
Environment="AIGW_DATA_DIR=/var/lib/aigw/data"

# 加固（aigw 只需要读写自己的数据目录 + 出站网络）
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/var/lib/aigw
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

[Install]
WantedBy=multi-user.target
```

```bash
sudo install -d -o aigw -g aigw /opt/aigw /var/lib/aigw
sudo cp aigw /opt/aigw/ && sudo cp -r providers /opt/aigw/
sudo systemctl daemon-reload && sudo systemctl enable --now aigw
journalctl -u aigw -f        # 日志也同时落 <data>/logs/aigw.log（4 MiB 滚动 × 5）
```

> ★ **`ProtectHome=read-only` 与默认密钥位置冲突**：默认密钥在
> `~/.config/aigw2/secret.key`，上例把 home 设成只读会导致密钥无法生成。
> 三种解法任选：
> - 用上面的 `AIGW_DATA_DIR=/var/lib/aigw/data`（数据在 home 外，密钥也在 home 外）；
> - 显式设 `AIGW_KEY_DIR=/var/lib/aigw/key`；
> - 或直接给 `AIGW_SECRET_KEY`（由 systemd `LoadCredential` / k8s Secret 注入）。

### macOS（launchd）

```xml
<!-- ~/Library/LaunchAgents/com.aigw.gateway.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.aigw.gateway</string>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/aigw/aigw</string>
    <string>--host</string><string>127.0.0.1</string>
    <string>--port</string><string>8800</string>
  </array>
  <key>WorkingDirectory</key><string>/opt/aigw</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/aigw.out.log</string>
  <key>StandardErrorPath</key><string>/tmp/aigw.err.log</string>
</dict>
</plist>
```

```bash
launchctl load ~/Library/LaunchAgents/com.aigw.gateway.plist
launchctl list | grep aigw
```

> **macOS 首次运行**可能被 Gatekeeper 拦（未签名二进制）：
> `xattr -d com.apple.quarantine /opt/aigw/aigw`

### Docker

```dockerfile
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p aigw

FROM debian:stable-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/aigw /usr/local/bin/aigw
COPY --from=build /src/providers /opt/aigw/providers
ENV AIGW_DATA_DIR=/data
VOLUME ["/data"]
EXPOSE 8800
# ★ 容器里必须显式 --host 0.0.0.0，否则只绑回环、端口映射不生效
ENTRYPOINT ["aigw", "--host", "0.0.0.0", "--port", "8800"]
```

```bash
docker build -t aigw2 .

# 固定主密钥：容器重建后卷里的凭据还能解开
export AIGW_SECRET_KEY=$(openssl rand -hex 32)
echo "AIGW_SECRET_KEY=$AIGW_SECRET_KEY" >> .env      # 妥善保管，丢了凭据就解不开

docker run -d --name aigw -p 8800:8800 \
  -v aigw-data:/data \
  -e AIGW_SECRET_KEY \
  -e AIGW_PANEL_TOKEN   aigw2
```

> ★ **容器里必须给 `AIGW_SECRET_KEY`**：否则密钥文件随容器销毁，
> 卷里的凭据下次启动全部解不开（表现为「所有账号突然失效」）。
>
> ★ 对外监听（`--host 0.0.0.0`）时**务必设 `AIGW_PANEL_TOKEN`**，
> 否则未设令牌会自动生成一个并在日志里打印一次。

---

## 协议入口

| 路径 | 协议 | 说明 |
|---|---|---|
| `POST /v1/chat/completions` | OpenAI | 主入口，流式/非流式 |
| `POST /v1/messages` | Anthropic | Claude Code 等客户端；工具调用/thinking 块原样透传 |
| `POST /v1/messages/count_tokens` | Anthropic | token 计数（本地估算或上游透传） |
| `POST /v1/responses` | OpenAI Responses | Codex 等客户端；`finish_reason=length` 会正确映射成 `incomplete` |
| `POST /v1/search/messages` | 私有 | 搜索类模型 |
| `GET /v1/models` | OpenAI | 聚合可用平台的模型清单，支持 `?platform=` |
| `GET /v1/credits` | 私有 | 积分/余额总览 |

### 模型名写法

```
gpt-5                      # 按声明式清单模糊匹配到某个平台
glm-4.6@web-glm            # 强制指定平台
combo:u-a5261120ac9477b5   # 模型链（一条有序候选，跨平台/跨模型回退）
我的测试链                  # 链的显示名，只要唯一就也能直接调用
```

---

## 声明式接入（providers/*.toml）

一份 toml 描述一个上游。**加平台 = 加文件**，不需要改代码。

```toml
id = "web-glm"
name = "智谱清言（网页版）"
group = "web"                # ui 分组：api / web / apk / sub / free / client
base_url = "https://chatglm.cn"
enabled = true

upstream_kind = "glm"        # 协议适配器；缺省走通用 OpenAI 透传
signer        = "glm"        # 请求签名器（crypto crate）；none = 不签
models_policy = "static"     # live / static / none
format        = "openai"     # openai / anthropic

# 凭据怎么取
auth = "cookie"              # cookie / apikey / oauth / anon / device
```

**字段合法性由 `aigw-registry` 严格校验**：`upstream_kind` / `signer` /
`models_policy` 都是白名单枚举 —— 写错会让该 spec 被跳过（不是整个目录失败），
原因通过 `/api/health` 的 `skipped_specs` 长期可见。

> ★ 这是踩过的坑：早期任何一份 toml 非法就让**全表**变空（平台数从 158 掉到 47），
> 而且只在日志里说一句、重启就滚掉。现在改成 `load_dir_lenient` + 上报。

### 体检

上游改版/反代失效时，面板只能看到「近 24h 失败 N 次」，说不出是哪个端点变了：

```bash
aigw --doctor                 # 逐个源用**真实转发代码**去拉清单
aigw --doctor --only api-     # 只看 id 含 "api-" 的
```

判定口径与运行时一致：`401/403` = 端点活着只是没凭据（不算失效）；
其它 Err 才是真失败。海外源会先查 DNS，区分「域名没了」和「连不上」。

---

## 凭据加密

| 后端 | 何时使用 | 密文前缀 |
|---|---|---|
| **DPAPI** | Windows 且系统凭据子系统可用（优先） | `dpapi:` |
| **AES-256-GCM** | 非 Windows，或 DPAPI 不可用 | `aigw1:` |
| 明文 | 两个后端都起不来（**会报 error 级日志 + `/api/health` 告警**） | 无前缀 |

```
密文布局：[ nonce 12B ][ ciphertext + tag 16B ]
AAD = purpose（跨用途冒充在解密时直接失败）
```

**主密钥来源优先级**：

1. `AIGW_SECRET_KEY`（64 位 hex）
2. `AIGW_KEY_DIR/secret.key`
3. 平台默认：`%LOCALAPPDATA%\aigw2\secret.key` / `~/.config/aigw2/secret.key`

密钥首次使用时用 CSPRNG 生成并落盘（Unix 上文件 `0600`、目录 `0700`），
生成走 `create_new` 抢锁 —— 两个进程同时首次启动不会互相覆盖。

> ★ **不只看 `cfg!(windows)`**：`/api/health` 的 `encryption` 字段是真做一次
> 「密封 + 开封」自检得出的。Windows 上 DPAPI 也可能失败（无用户配置文件、
> 服务账户、受限容器），而那正是最容易被误判成「已加密」的情况。

**迁移到另一台机器**：`data/` 加 `secret.key` 一起拷（DPAPI 密文不行 ——
它绑定原用户+原机器；那种情况只能重新登录）。

---

## 可观测性

`GET /api/health` 里的关键字段：

| 字段 | 用途 |
|---|---|
| `encryption.backend` | `dpapi` / `aes-256-gcm` / `none`。**是 `none` 就说明凭据在明文落盘** |
| `tasks` | 后台任务的 `ok_runs` / `err_runs` / `last_err` / 距今秒数。「该跑的任务 3 天没动静」只有这里看得见 |
| `limits` | 被限流挡过几次、上次多久前、是全局还是每 IP 触顶 |
| `build.uptime_s` | 这次部署到底换没换成功（版本号一整天不变，看 uptime 是否归零） |
| `skipped_specs` | 哪些 `providers/*.toml` 被跳过、为什么 |
| `write_failure` | 最近一次落盘失败的**文件名 + 时间 + 原因**（见下） |

> **落盘失败为什么单独上报**：各 Store 的 `save_locked` 只在失败时 `warn!` 一句，
> 调用方拿不到任何信号 —— 面板回「已保存」、内存里也确实改了，但盘上还是旧内容。
> 重启后「刚停用的模型又活了、刚删的 key 又回来了」，而用户全程没看到一句提示。
> 现在在唯一的落盘出口登记，health 里可查。

日志：`<data>/logs/aigw.log`，4 MiB 滚动、留 5 份（约 24 MB 封顶）。
新文件带 UTF-8 BOM（Windows PowerShell 5.1 不加 BOM 会把中文按 ANSI 解成乱码）。

---

## 开发

```bash
cd aigw2

cargo check --workspace --all-targets   # 必须零 error 零 warning
cargo test  --workspace --no-fail-fast  # 855 项
cargo build --release -p aigw
```

### workspace 结构

| crate | 职责 |
|---|---|
| `aigw-core` | 错误分类（`ErrorClass`）、跨平台目录（`paths`） |
| `aigw-crypto` | 请求签名：glm / codearts / loomy / qoder / zcode / officeace |
| `aigw-registry` | `ProviderSpec` 声明式接入 + toml 加载与校验 |
| `aigw-store` | 凭据池 / 自定义 API / 公益站 / 开关 / 模型链 / 密钥 / 审计日志 / 原子落盘 / 凭据加密 |
| `aigw-pool` | 号池六件套（租约 / 冷却 / WAF / RecentPick / 半开 / forget） |
| `aigw-relay` | 出站 HTTP + 平台适配器 + 登录链路 + 代理 |
| `aigw-server` | axum 路由 + 门禁 + 协议转换 + rust-embed 前端 |
| `aigw` | 二进制入口（CLI / 装配 / 安装器 / 体检 / doctor） |

> ⚠️ `aigw-server/src/lib.rs` 目前 **13591 行**，其中约 54% 是内联 `mod tests`。
> 拆分是已识别的待办（见 `../项目优化清单.md`），不是"还没想到"。

### 改前端

`web/*` 通过 `rust-embed` 在**编译期**嵌入二进制：

```bash
cargo build -p aigw    # 改完前端必须重新编译
# 浏览器里 Ctrl+Shift+R 硬刷新
```

---

## 排障

| 现象 | 原因 / 处理 |
|---|---|
| `error: target tuple in channel name` | `rust-toolchain.toml` 被写成了具体三元组。本仓库已改成 `channel = "stable"`；Windows 用 `rustup override` 指定 msvc |
| `error calling dlltool 'dlltool.exe'` | Windows 上用了 gnu 工具链。`rustup override set stable-x86_64-pc-windows-msvc` |
| `0xC0000142` 弹窗（build script） | 构建目录被杀软扫描/只读属性拦（常见于 `Downloads\`）。`CARGO_TARGET_DIR` 指到 `%TEMP%` 或 `C:\aigw-target` |
| 所有账号突然失效 | 多半是密钥变了：容器重建丢了 `secret.key`。设 `AIGW_SECRET_KEY` 固定 |
| `/api/health` 显示 `encryption.backend = none` | 凭据在**明文**落盘。检查数据目录可写性，或设 `AIGW_SECRET_KEY` |
| `providers` 目录未找到 | 用了 `--providers` 但路径错；或发布包漏带 `providers/`。缺省依次找 `cwd/providers` → exe 同级 `providers/` |
| 面板中文乱码（PowerShell） | 是 `Invoke-WebRequest` 把 UTF-8 当 GBK 解，不是服务端问题。用 `curl.exe` / node fetch |
| `/v1/*` 返回 401 | 配了访问密钥但客户端没带 `Authorization: Bearer <key>` |
| `/v1/*` 返回 429 | 看 `/api/health` 的 `limits`：全局 / 每 IP / 每密钥哪一档触顶，以及 `Retry-After` |
| 上游 `403` 全是同一个账号 | WAF 账号级隔离（30 min）。换号或等隔离解除 |

---

## 相关文档

- `../README.md` —— 项目总览与风险提示
- `../legacy/README.md` —— 归档的 Python 版 / 第一代 Rust 版 / CLIProxyAPI（含取回方式）
- `../项目优化清单.md` —— 已识别但未做的优化项（含优先级与位置）
