# legacy/ —— 已废弃实现归档

本目录存放**不再维护、但按需求保留源码与改动记录**的历史实现。
它们不参与构建、不参与 CI、不进入发布产物；保留目的是「可查、可回溯、可取回」。

- 归档日期：**2026-10-29**
- 归档方式：`git mv`（原路径 → `legacy/原路径`），**git 历史全部保留**
- 主线：`aigw2/`（Rust workspace，8 crates + 158 份 `providers/*.toml`）

---

## 一、归档清单

| 原路径 | 现路径 | 语言/栈 | 首提交 | 最后提交 | 触及提交数 | tracked 文件 | 状态 |
|---|---|---|---|---|---|---|---|
| `aigw-panel/` | `legacy/aigw-panel/` | Python 3（纯标准库 + PyInstaller） | `cc70abc` 2026-10-04 | `3f00360` 2026-10-19 | 106 | 143 | 🗄 只读归档 |
| `aigw-panel-rs/` | `legacy/aigw-panel-rs/` | Rust（第一代重写） | `c3fad36` 2026-10-14 | `3f00360` 2026-10-19 | 12 | 78 | 🗄 只读归档 |
| `cli-proxy/` | `legacy/cli-proxy/` | YAML 配置 + 上游 README | `28b71ee` 2026-10-04 | `e45495f` 2026-10-04 | 2 | 7 | 🗄 只读归档 |
| `cliproxy-src/` | `legacy/cliproxy-src/`（**未入版本库**） | Go（第三方仓库） | — | — | 1（仅 gitlink） | 0（原为 gitlink） | 📦 仅留磁盘副本 |

> `cliproxy-src` 原本是一个 **gitlink（mode 160000）但没有 `.gitmodules`** ——
> 新克隆会得到一个空目录，属于「看起来在库里、实际什么都没存」。
> 归档时已 `git rm --cached` 解除登记，磁盘上的完整源码副本仍在，
> 上游地址与固定 commit 见下文第三节。

---

## 二、为什么废弃

### 1. `aigw-panel`（Python 版）

- **主线已全量 Rust 重写**（`aigw2/`）：同样的功能（150+ 接入源、号池治理、
  多协议入口、备份恢复、前端面板）在 Rust 版里是编译期类型检查 + 855 项测试。
- Python 版是**单文件 18 万字符**（`main.py` 185 KB）+ 90 多个散落的探针脚本，
  没有测试基线，改一处要手工回归。
- 非 Windows 上凭据只能明文落盘（`secret.rs` 之前的实现），与「支持 Linux/macOS
  部署」的目标直接冲突 —— 这条已在 `aigw2` 里修掉（DPAPI / AES-256-GCM 双后端）。

### 2. `aigw-panel-rs`（第一代 Rust）

- 它是 Python 版的**逐模块直译**：`src/server/handlers/api/routers/*.rs` 14 个域
  与 Python 的 `app/*.py` 一一对应，继承了同样的耦合（单 `lib.rs` 巨大、
  平台能力判断散落多处）。
- 架构上已经落后：没有声明式接入（`providers/*.toml`），平台逻辑写死在代码里；
  没有号池六件套、没有多协议入口（只有 `/v1/chat/completions` + `/v1/messages`）。
- 数据目录写死 `%LOCALAPPDATA%`，非 Windows 不可用。

### 3. `cli-proxy` / `cliproxy-src`（CLIProxyAPI 集成）

- `cli-proxy/` 只是**一份指向第三方 Go 二进制的配置文件 + README**，
  内容是「把这个 68 MB 的 exe 塞进面板 EXE」（`build.spec` 的 `datas`）。
- 这条路线要求**外挂一个 Go 进程**（`127.0.0.1:8318`），与本项目「单 EXE 自洽、
  无外部网关」的定位相反；且该二进制不入库（`.gitignore` 里有 `cli-proxy/*.exe`），
  克隆下来根本跑不起来。
- `cliproxy-src/` 是上游 `router-for-me/CLIProxyAPI` 的完整克隆，**不是本项目代码**，
  不应占据本仓库体积与许可责任。

---

## 三、如何取回 / 如何回滚

### 只读查阅（推荐）

历史就在 git 里，**不需要动工作区**：

```bash
# 看某个文件的历史版本
git show 3f00360:aigw-panel/main.py | less

# 看某个目录在某次提交时的完整快照
git ls-tree -r --name-only cc70abc -- aigw-panel

# 只看改动记录（提交数 106）
git log --oneline -- aigw-panel
```

### 整体恢复到工作区（不提交）

```bash
git checkout 3f00360 -- aigw-panel aigw-panel-rs cli-proxy
# 用完清掉：
git rm -r --cached aigw-panel aigw-panel-rs cli-proxy
```

### 撤销本次归档（把目录挪回根）

```bash
git mv legacy/aigw-panel     aigw-panel
git mv legacy/aigw-panel-rs  aigw-panel-rs
git mv legacy/cli-proxy      cli-proxy
git commit -m "revert(legacy): 取消归档"
```

### `cliproxy-src`（第三方仓库）

磁盘副本仍在 `legacy/cliproxy-src/`，但它**不受本仓库版本控制**。
需要重建时直接克隆上游并检出归档时的 commit：

```bash
git clone https://github.com/router-for-me/CLIProxyAPI.git cliproxy-src
git -C cliproxy-src checkout 6746588   # fix(pluginhost): detach unselected plugins on config apply
```

> 归档时的本地副本是 **shallow clone**（`rev-list --count HEAD` = 1），
> 只有 `6746588` 这一个 commit 的完整树，没有更早历史。

---

## 四、这些目录里仍有价值的东西（不要当垃圾清掉）

主线 `aigw2` 大量机制是从这两个旧实现里**提炼**出来的，
反查「当初为什么这么写」时，旧代码是最好的注释：

| 位置 | 价值 |
|---|---|
| `legacy/aigw-panel-rs/docs/ARCHITECTURE.md` | 分层设计的原始论证（server/relay/router/pool 四层为什么这么切） |
| `legacy/aigw-panel-rs/docs/LOGIN-MECHANISMS.md` | 各平台登录机制的**一手整理**（设备码 / PKCE / 轮询 / 回调） |
| `legacy/aigw-panel/反代项目对比与补全报告.md` | 反代项目横向对比 |
| `legacy/aigw-panel/项目全链路解析报告.md` | Python 版全链路解析（含各平台端点细节） |
| `legacy/aigw-panel/库定义解析.md` | APK 侧库/常量定义梳理 |
| `legacy/aigw-panel-rs/双项目对标与借鉴.md` | 与参考项目的逐项对标 |
| `legacy/cli-proxy/README-AIGW.md` | CLIProxyAPI 集成的完整设计与踩坑（如仍要评估该路线） |

---

## 五、维护约定

1. **本目录不再接受功能改动。** 发现 bug 也不修，除非是「归档本身写错了」。
2. **不要把它加回 workspace / CI。** `aigw2/Cargo.toml` 的 `members` 不应包含这类路径。
3. **不要在这里放凭据。** 旧版的 `data/`、`dist/`、`auths/` 目录已被 `.gitignore`
   排除（含 `chrome-capture-profile` 等抓包产物），它们只存在于本机磁盘。
4. 真要复活某个实现，正确做法是**从 git 历史新建分支**，而不是在 `legacy/` 里改。
