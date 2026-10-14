# aigw-panel-rs

本地 AI 资源整合网关面板 · **Rust 重写版**（大厂级分层架构）。

Python 版（`../aigw-panel`）的完整 Rust 重写：面板自身即本地网关，`/v1` 暴露 OpenAI 兼容端点，
前端零改动（复用 Python 版 `app/static`）。

## 快速开始

```powershell
# 构建（Windows · msvc target + ASCII target dir）
C:/Users/liang.zhao/.cargo/bin/cargo.exe build --target x86_64-pc-windows-msvc --target-dir C:/aigw-target

# 运行（默认 127.0.0.1:8800）
C:/aigw-target/x86_64-pc-windows-msvc/debug/aigw-panel.exe

# 指定静态资源目录（前端沿用 Python 版）
$env:AIGW_STATIC_DIR = "C:/Users/liang.zhao/Downloads/网关项目/aigw-panel/app/static"
```

客户端接入：`http://127.0.0.1:8800/v1`（ZCode / Cherry / 任意 OpenAI 兼容客户端），
`model` 填 `auto`（最快优先）或 `free`（免费优先），或 `模型名@平台ID` 定向中继。

## 测试

```powershell
C:/Users/liang.zhao/.cargo/bin/cargo.exe test --target x86_64-pc-windows-msvc --target-dir C:/aigw-target
```

## 架构

见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。
