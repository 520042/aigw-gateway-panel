# CLIProxyAPI —— 已集成进面板 EXE

> **你不需要单独装这个服务了。** 面板 EXE 里已经内置了 `cli-proxy-api.exe`，
> 装好面板后在「本地反代」页点一下「启动」就能用。

## 零、集成后是什么样

| 项 | 值 |
|---|---|
| 内置方式 | `build.spec` 的 `datas` 收进单 EXE（目录 `cliproxy/`） |
| 面板 EXE 体积 | **31.9 MB**（不含内置是 10.35 MB；Go 二进制 68 MB 被压成 ~21 MB） |
| 释放位置 | EXE 同级 `data/cliproxy/bin/cli-proxy-api.exe`（68 MB，只解一次） |
| 记账方式 | `data/cliproxy/bin/.binary.stamp` 记 size，二次调用 0.003 秒返回，不重复拷 |
| 配置 | `data/cliproxy/config.yaml`（面板自动生成，可手工编辑） |
| 凭据 | `data/cliproxy/auths/` |
| 日志 | `data/cliproxy/service.log` |
| 端点 | `http://127.0.0.1:8318/v1`（和网关 8317 错开，占用时自动顺延） |
| API Key | `aigw-local-key` |

### 面板里的操作

「**本地反代**」页（侧边栏 ⇄）：

- 4 张 KPI 卡：服务状态 / 内置二进制 / 可用模型 / 端点
- 启停按钮：启动 / 停止 / 重启 / 刷新状态 / 查看日志
- 端口和 API Key 可改，改完「保存并重启」
- 9 种 OAuth 登录，点「获取授权链接」→ 面板弹出链接 → 点开授权
  → 凭据自动落 `auths/`，服务热加载**不用重启**
- 已保存的凭据列表、可路由的模型列表、服务日志

### 不想内置？做个轻量版

`build.spec` 里把 `include_cliproxy = True` 改成 `False`，
打包出来还是 10.35 MB，面板会提示「没有找到 cli-proxy-api.exe」，
你把二进制手动放到 `网关项目/cli-proxy/` 或 `aigw-panel/data/cliproxy/bin/` 也能用。

---

## 一、现状（手工部署的记录，集成前的验证）

| 项 | 值 |
|---|---|
| 版本 | **v8.0.13**（commit `d7914afd`，2026-10-03 发布） |
| 可执行 | `cli-proxy-api.exe`（71 MB，Go 单二进制） |
| 来源 | `https://github.com/router-for-me/CLIProxyAPI`（54k★） |
| 下载 | GitHub releases 直链在���网不通，走的 `gh-proxy.com` 镜像 |
| ZIP 校验 | 通过（`testzip()` 无损坏项） |
| 本机端点 | `http://127.0.0.1:8318/v1` |
| API Key | `aigw-local-key`（写在 `config.yaml`） |
| 当前状态 | **服务在跑，0 个账号**（还没做任何 OAuth 登录） |

## 二、⚠️ 一处需要更正的信息

早期调研（写进 `VibeCoding与反代调研.md`）称 CLIProxyAPI「原生支持
CodeBuddy CN / CodeBuddy Intl / Qoder 的 OAuth」—— **这是错的**。

下载后跑 `--help` 实测，v8.0.13 的登录项只有：

```
-kimi-login            -kimi-ai-login
-codex-login           -codex-device-login
-claude-login          -antigravity-login
-xai-login             -devin-login           -meta-login
```

**没有 CodeBuddy，没有 Qoder。** 面板的 `upstreams.VIBE_PROXY` 里已更正，
并加了单测断言防止以后再写错（`CLIProxyAPI 登录项不含 CodeBuddy`）。

所以对你的价值变成：把 **Kimi Code / Codex / Claude Code / Antigravity / Grok /
Devin / Meta** 这些 CLI 订阅转成 OpenAI 兼容 API —— 其中 Antigravity 面板本来就有，
其余 6 家是新增的。

## 三、启动 / 停止

```bash
cd 网关项目/cli-proxy

# 启动（后台）
./cli-proxy-api.exe --config config.yaml

# 停止：任务管理器，或
taskkill /F /IM cli-proxy-api.exe
```

## 四、登录账号（需要你扫码/授权）

在 `cli-proxy` 目录里跑对应命令，它会打开浏览器让你授权：

```bash
./cli-proxy-api.exe -kimi-login        --config config.yaml   # Kimi
./cli-proxy-api.exe -codex-login       --config config.yaml   # Codex(OpenAI)
./cli-proxy-api.exe -claude-login      --config config.yaml   # Claude Code
./cli-proxy-api.exe -antigravity-login --config config.yaml   # Antigravity(Gemini)
./cli-proxy-api.exe -xai-login         --config config.yaml   # xAI Grok
./cli-proxy-api.exe -devin-login       --config config.yaml   # Devin
./cli-proxy-api.exe -meta-login        --config config.yaml   # Meta Muse
```

登录成功后凭据落在 `cli-proxy/auths/` 目录，服务**自动热加载**（有 file watcher），
不用重启。之后 `GET /v1/models` 就会列出该账号可用的模型。

不方便开浏览器的话加 `--no-browser`，它会打印一个 URL 自己复制到别的浏览器打开。

## 五、验证

```bash
# 本机直测
curl http://127.0.0.1:8318/v1/models -H "Authorization: Bearer aigw-local-key"

# 面板里看
# 「上游档案」→ Vibe Coding 反代 → 「本机状态」列显示「在线 28ms」
# 点「重新探活」会重新探测
```

实测记录：

| 端点 | 结果 |
|---|---|
| `GET /v1/models`（正确 key） | 200 `{"data":[]}` —— 服务活着但没挂账号 |
| `GET /` | 200，列出 `POST /v1/chat/completions` / `POST /v1/completions` / `GET /v1/models` |
| `GET /v1/models`（错误 key） | **401** `{"error":"Invalid API key"}` —— 鉴权生效 |

## 六、接到客户端

CLIProxyAPI 自己就是 OpenAI 兼容端点，可以直接用：

```python
# OpenAI SDK
from openai import OpenAI
client = OpenAI(base_url="http://127.0.0.1:8318/v1", api_key="aigw-local-key")
```

也可以在面板里生成配置（模型页勾选后点「生成客户端配置」），
把它当第二个上游串起来：

```
客户端 → 面板 /v1  →  网关 8317
                  └→  CLIProxyAPI 8318
```

## 七、故障排查

| 现象 | 原因 / 处理 |
|---|---|
| 面板显示「Key 不符」（401） | 面板配的 key 与 `config.yaml` 里的 `api-keys` 不一致 |
| 端口有响应但 404 | 这个端口上跑的不是 CLIProxyAPI |
| 登录后模型列表还是空的 | OAuth 没走完，看 `auths/` 目录有没有新文件 |
| 启动报端口占用 | 改 `config.yaml` 的 `port`（默认 8318，和网关 8317 特意错开） |
| 面板探活显示「离线」 | 服务没起，或端口不是 8318 |

## 八、面板怎么用它

面板目前**只是探活 + 展示**，没有把它接进 `app/router.py` 的路由池
（路由池里是「平台 → 上游端点」的结构，CLIProxyAPI 是一个独立聚合服务，
硬塞进去会让自动路由的语义变乱）。

如果你要真正用起来，有两条路：

1. **客户端直连**：Cherry Studio / Claude Code 里直接填 `http://127.0.0.1:8318/v1`，
   和面板并列使用（最简单，推荐）
2. **串进面板**：在 `app/upstreams.py` 加一条 `endpoint: http://127.0.0.1:8318/v1`，
   并在路由配置里启用为上游 —— 需要改路由的数据结构，说一声我来做
