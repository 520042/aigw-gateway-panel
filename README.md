# AIGW 网关 · 免费 AI 额度聚合

把散落在各处的 AI 额度（官方 API、客户端订阅、网页版逆向、公益中转站）
统一收进一个本地进程，对外只暴露**一个 OpenAI / Anthropic 双协议端点**。

> **主线 = [`aigw2/`](aigw2/)**（Rust workspace，8 crates + 158 份声明式接入源）。
> 单个二进制，前端内嵌，无 Node / Python / 外部网关依赖。
>
> 历史实现在 [`legacy/`](legacy/)（Python 版、第一代 Rust 版、CLIProxyAPI 集成）
> —— **只读归档，保留 git 历史与取回方式**，不参与构建。

---

## ⚠️ 重要风险提示（请务必先读）

### 1. 逆向工程与服务条款风险

本项目部分接入方式基于对第三方平台**客户端/网页版的逆向分析**实现，
通过模拟官方请求提取会话凭据、调用私有接口。

**使用本项目可能导致：**

- 你的平台账号被封禁、限制功能或永久封号
- 平台对你的 IP 进行限流或封禁
- 平台依据服务条款对你提起法律诉讼

> **风险等级：高** —— 逆向调用第三方 API 通常明确违反平台用户协议。

### 2. 数据安全

凭据（Cookie、Refresh Token、API Key）保存在本机数据目录，
**默认加密落盘**：

| 平台 | 加密方式 |
|---|---|
| Windows | DPAPI（绑定当前用户 + 本机） |
| Linux / macOS | AES-256-GCM + 独立密钥文件（`~/.config/aigw2/secret.key`） |

> 但要清楚边界：**这是「防磁盘被顺手拷走」，不是「防本机被攻破」**——
> 服务运行期间凭据必然以明文存在于进程内存。
>
> `/api/health` 的 `encryption.backend` 会如实上报当前后端；
> 如果显示 `none`，说明两个后端都起不来、**凭据正在明文落盘**，请立即处理。

### 3. 法律合规

本项目仅供**个人学习与技术研究**使用。

**禁止用于：**

- 商业用途或盈利目的
- 批量爬取或滥用平台资源
- 侵犯第三方知识产权或服务权益
- 任何违反法律法规的行为

### 4. 服务稳定性

- 第三方平台随时可能更新接口、改签名算法或加强风控，导致功能失效
- 不保证 100% 可用，也不承诺持续维护
- 部分功能可能因平台策略调整而随时下线

---

## 📜 免责声明

**本项目及贡献者明确声明：**

1. **仅供学习研究**：仅用于技术学习与研究目的，不得用于任何商业或非法用途。

2. **风险自担**：使用本项目产生的一切风险（包括但不限于账号封禁、数据丢失、
   法律纠纷、财产损失）由使用者自行承担，项目开发者不承担任何责任。

3. **与平台无关**：本项目与智谱 AI、字节跳动、腾讯、百度、阿里等任何第三方
   平台无任何关联，未获官方授权或认可。所有品牌名称、商标均归其各自所有者。

4. **无担保**：按"现状"提供，不提供任何明示或暗示的担保，包括但不限于适销性、
   特定用途适用性、不侵权的担保。

5. **及时下架**：若第三方平台认为本项目侵犯其权益，请联系开发者，我们将在
   收到通知后及时下架相关内容。

---

## 快速开始

### 环境要求

- Rust **stable**（1.75+）
- Windows 需 **msvc** 工具链（`rustup override set stable-x86_64-pc-windows-msvc`）

### 构建与运行

```bash
git clone https://github.com/520042/aigw-gateway-panel.git
cd aigw-gateway-panel/aigw2

cargo build --release -p aigw
./target/release/aigw          # → http://127.0.0.1:8800
```

Windows PowerShell：

```powershell
cd aigw2
cargo build --release -p aigw
.\target\release\aigw.exe --port 8800
```

启动后浏览器打开 `http://127.0.0.1:8800`，数据全部留在本机。

### 客户端接入

```
base_url = http://127.0.0.1:8800/v1
api_key  = <面板「访问密钥」页生成>     # 未配访问密钥时填任意非空串
model    = 模型名 | 模型名@平台ID | combo:xxx
```

支持四种协议入口：`/v1/chat/completions`（OpenAI）、`/v1/messages`（Anthropic）、
`/v1/responses`（OpenAI Responses）、`/v1/search/messages`。

---

## 部署平台

| 平台 | 支持 | 说明 |
|---|---|---|
| **Windows** | ✅ | 绿色版 / `--install` 自检安装（快捷方式 + 自启）/ 计划任务 |
| **Linux** | ✅ | systemd 单元示例 + 加固建议（`ProtectSystem`、专用用户） |
| **macOS** | ✅ | launchd plist 示例；未签名二进制需 `xattr -d com.apple.quarantine` |
| **Docker** | ✅ | 多阶段构建；必须固定 `AIGW_SECRET_KEY`，否则重建容器后凭据全解不开 |

完整部署步骤、systemd/launchd 单元、Dockerfile 见 **[`aigw2/README.md`](aigw2/README.md#跨平台部署)**。

### 数据与密钥位置

| | Windows | Linux / macOS |
|---|---|---|
| 数据目录 | `%LOCALAPPDATA%\aigw2\data` | `~/.config/aigw2/data` |
| 密钥文件 | `%LOCALAPPDATA%\aigw2\secret.key` | `~/.config/aigw2/secret.key` |

### 环境变量

| 变量 | 作用 |
|---|---|
| `AIGW_DATA_DIR` | 数据目录（设了就当作独立档案，不做旧数据导入） |
| `AIGW_KEY_DIR` | 凭据密钥目录（设了就只认它） |
| `AIGW_SECRET_KEY` | 主密钥本身（64 位 hex）；容器/k8s Secret 用 |
| `AIGW_PANEL_TOKEN` | 面板令牌（等价 `--token`） |
| `RUST_LOG` | 日志级别，默认 `info` |

---

## 功能特性

### 核心能力

- 🔐 **多源账号池** —— 158 份声明式接入（官方 API / 客户端订阅 / 网页版 / 公益站）
- 🔄 **统一双协议端点** —— OpenAI + Anthropic，客户端只填一个地址
- 🧠 **号池治理六件套** —— 租约 / 冷却 / WAF 隔离 / RecentPick / 半开 / forget
- 📉 **失败换号** —— 只在账号自身类错误（401/403/429/402/5xx）上换号
- ⚙️ **热更新** —— 并发/超时/重试/三档限流，面板改完立即生效，不断流
- 📊 **可观测** —— 加密状态 / 后台任务 / 限流现场 / 落盘失败 / 构建自证
- 🩺 **接入源体检** —— `aigw --doctor` 逐个源用真实代码试拉清单

### 支持范围

接入源按 `providers/*.toml` 里的 `group` 分组：

| 分组 | 数量级 | 例子 |
|---|---|---|
| `api` | 官方 API Key | Gemini / Mistral / OpenRouter / 智谱 / 百炼 / 火山 |
| `sub` | 客户端订阅（设备码 / PKCE / OAuth） | Codex / Claude / Antigravity / Devin / Qoder |
| `web` | 网页版逆向（Cookie） | 智谱清言 / DeepSeek / 元宝 / MonkeyCode |
| `apk` | 移动端逆向 | 豆包 / 小浣熊 / 秘塔 / WorkBuddy / OfficeAce |
| `free` | 公益中转站（NewAPI/OneAPI 系） | 自建清单，面板内可增删 |
| `client` | 自定义 OpenAI/Anthropic 兼容端点 | 任意第三方网关 |

> 具体清单以 `aigw2/providers/*.toml` 为准（`enabled = false` 的源暂不启用，
> 原因写在各文件的 `notes` 里）。

---

## 仓库结构

```
.
├── aigw2/                     ★ 主线（Rust workspace）
│   ├── crates/
│   │   ├── aigw-core/         错误分类 · 跨平台目录
│   │   ├── aigw-crypto/       请求签名（glm/codearts/loomy/qoder/zcode/officeace）
│   │   ├── aigw-registry/     ProviderSpec 声明式接入
│   │   ├── aigw-store/        凭据池 · 模型链 · 原子落盘 · 凭据加密
│   │   ├── aigw-pool/         号池六件套
│   │   ├── aigw-relay/        出站 HTTP · 平台适配器 · 登录链路
│   │   ├── aigw-server/       axum 路由 · 门禁 · 协议转换 · 内嵌前端
│   │   └── aigw/              二进制入口（CLI/装配/安装器/doctor）
│   ├── providers/             158 份声明式接入源（*.toml）
│   ├── web/                   单页前端（rust-embed 编译期内嵌）
│   └── README.md              ★ 部署 / 环境变量 / 排障
├── legacy/                    🗄 归档（只读，git 历史完整）
│   ├── aigw-panel/            Python 原版
│   ├── aigw-panel-rs/         第一代 Rust 重写
│   ├── cli-proxy/             CLIProxyAPI 集成配置
│   └── README.md              ★ 归档原因 / 取回方式 / 仍有价值的文档索引
├── .github/workflows/         CI：三平台 cargo check + test + release
└── .workbuddy/memory/         开发日志（按日，含踩坑记录）
```

---

## 注意事项

| 项 | 说明 |
|---|---|
| 凭据安全 | 默认加密落盘，但**导出 JSON 里是明文**，别乱发 |
| 密钥备份 | 换机器要连 `secret.key` 一起拷（DPAPI 密文不行，只换机器需重登） |
| 公益站风险 | 社区站可能掺水或跑路，勿传隐私数据、勿绑主账号、勿大额充值 |
| 额度有效期 | 多数平台赠送额度有 30/90 天有效期，注意及时使用 |
| 公网暴露 | 默认只绑回环。对外监听时**务必设 `AIGW_PANEL_TOKEN`** |
| 协议合规 | 逆向接入随时可能失效，`aigw --doctor` 是排查第一手段 |

---

## 开源协议

MIT License —— 但请注意：**MIT 协议仅授予代码使用权限，不代表授权你可以用它
去逆向或违反第三方平台的服务条款。**

使用风险请自行承担，详见上方「风险提示」与「免责声明」。
