# 架构说明（大厂级分层）

## 分层总览

```
aigw-panel-rs/
├── Cargo.toml               # lib + bin 双 target
├── catalog_bundled.json     # ★ 单一数据源（gen_catalog.py 从 Python 一手模块导出，零手写）
├── gen_catalog.py           # 数据导出脚本（bundled_models / native_relay / doubao_relay → JSON）
├── src/
│   ├── lib.rs               # 库入口（模块声明 + 公共 re-export）
│   ├── main.rs              # 二进制入口：CLI(clap) → tracing → 状态装配 → axum::serve
│   ├── core/                # 通用基座层
│   │   ├── error.rs         #   统一错误类型 AppError（thiserror + IntoResponse）
│   │   ├── config.rs        #   Settings 持久化 / 敏感字段脱敏
│   │   ├── model.rs         #   领域模型：Target / Plan / LoginMethod / RelayRequest
│   │   └── traits.rs        #   平台抽象：PlatformProvider / LoginProvider
│   ├── catalog/mod.rs       # 平台目录：grouped 分组 / relay_spec / free_lane 查询
│   ├── router/mod.rs        # 路由引擎：fastest/weight/priority/free 四策略 + 熔断 + 会话亲和
│   ├── relay/mod.rs         # 中继执行引擎：Plan → HTTP 转发（流式 SSE 透传）
│   ├── platforms/
│   │   └── registry.rs      # 平台注册表：catalog → CatalogPlatform（数据驱动，新增平台不改代码）
│   ├── server/
│   │   ├── mod.rs           # build_app()：路由装配 + 鉴权中间件
│   │   ├── state.rs         # AppState（Arc 共享句柄）
│   │   ├── auth.rs          # 鉴权中间件：Origin 校验 + X-AIGW-Token
│   │   ├── static_files.rs  # 静态资源 / index / catch_all / health
│   │   └── handlers/
│   │       ├── api.rs       # /api/* 数据面
│   │       └── v1.rs        # /v1/* OpenAI 兼容
│   └── tray/mod.rs          # 系统托盘（占位，后续 windows-sys）
├── tests/server_smoke.rs    # 集成测试：catalog / registry / router / HTTP oneshot
└── docs/ARCHITECTURE.md
```

## 关键设计决策

### 1. lib + bin 分离
所有业务逻辑在 lib crate `aigw_panel`；bin 只做装配。集成测试（`tests/`）直接调
`build_app()` + `tower::ServiceExt::oneshot` 走完整 HTTP 栈，无需监听端口。

### 2. 单一数据源（零手抄漂移）
模型清单 / 平台目录全部来自 `catalog_bundled.json`，由 `gen_catalog.py` 从 Python 版
一手模块（`bundled_models` / `bundled_trae` / `bundled_go` / `bundled_yuanbao` /
`doubao_relay.MODE_FLAGS` / `native_relay.RELAY_SPECS`）导出。Rust 侧 `include_str!`
编译期内嵌，运行时零 IO。

### 3. trait 抽象 + 数据驱动注册表
`PlatformProvider` trait 统一 42 个接入平台的抽象；`Registry::build()` 从 catalog
数据一次性构造所有平台实现。**新增平台 = 更新 JSON 数据，不改 Rust 代码。**
`@pid` 中继全量委托给对应 provider 的 `relay()`，顶层 `resolve()` 只负责
auto/free/upstream 兜底。

### 4. 统一错误类型
`core::error::AppError`（thiserror）实现 `IntoResponse`，HTTP 状态码集中映射；
后续原生中继模块的所有 fallible 路径统一收敛到它。

### 5. 结构化日志
全链路 `tracing`（handler 带 `#[tracing::instrument]`），替代 `println!`；
日志级别由 `RUST_LOG` 环境变量控制，默认 `info`。

### 6. 会话亲和与熔断（对齐 Python 版）
- 熔断：连续 3 次失败 → OPEN 60s → 半开试探
- 亲和：`aigw_session` 字段粘同上游，TTL 3600s
- 策略：`fastest`（EWMA 延迟）/ `weight` / `priority` / `free`（免费车道优先）

## Windows 构建要点（踩坑记录）

| 坑 | 解法 |
|---|---|
| 默认 host 是 `gnu`，dlltool.exe 缺失 | 必须 `--target x86_64-pc-windows-msvc` |
| 中文路径导致 MinGW ld 找不到 rcgu.o | `--target-dir C:/aigw-target`（纯 ASCII 路径） |
| 端口被旧进程占用 os error 5 | 构建前 `netstat -ano \| findstr :8800` 杀掉旧进程 |
| axum 0.7 HeaderMap 私有 | 从 `http` crate 导入，不用 `axum::extract::HeaderMap` |
| include_str 相对路径 | `src/catalog/mod.rs` → `../../catalog_bundled.json`（回退到 crate 根） |

## 与 Python 版的契约

- `/api/overview` `/api/models?action=grouped|sources` `/api/login?action=platforms`
  `/api/settings` `/api/checkins` `/api/accounts` `/api/health`
- `/v1/models` `/v1/chat/completions`（流式 SSE 透传）`/v1/messages`（Anthropic 兼容，后续）
- 鉴权：跨站 Origin 拒绝（403）+ X-AIGW-Token（401）；本机回环且未设令牌时免验证
- 前端：复用 Python 版 `app/static`，`AIGW_STATIC_DIR` 指向即可，零改动

## 后续模块（按优先级）

1. **原生中继**（per-platform relay）：豆包 samantha / 元宝 / Trae / Go 网关 / WorkBuddy copilot 系
   —— 每个 `PlatformProvider` 实现真实 `relay()`，替换当前 NeedLogin 占位
2. **凭据池 + 登录链路**：`LoginProvider` trait 落地（copilot 扫码 / cookie / anon）
3. **Anthropic /v1/messages** 兼容端点
4. **签到 / 成长任务**（数据驱动 autocheckin）
5. **系统托盘**（windows-sys：显示 / 退出 / 开机自启）
