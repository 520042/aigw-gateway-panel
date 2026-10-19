//! aigw v2 入口：CLI 解析 → registry 加载 → 数据目录 + 旧数据导入 → axum 起服。
//!
//! 里程碑路线见 重构计划-v2.md：M0 骨架 → M1 引擎核（本文件含数据迁移接线）
//! → M2 声明式接入 → M3 平台全量 → M4 前端 → M5 交付链 → M6 可观测。
//! 端口最终目标 8800；与旧版并行期用 `--port 8801`。

mod install;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "aigw",
    version,
    about = "aigw v2 —— 免费 AI 额度 → 本地 OpenAI 兼容端点（Rust 全量重写）"
)]
struct Cli {
    /// HTTP 监听地址（仅本机回环）
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// HTTP 监听端口；与旧版并行期用 --port 8801
    #[arg(long, default_value_t = 8800)]
    port: u16,
    /// providers 目录（声明式平台 spec）。缺省依次找：
    /// cwd/providers → exe 同级 providers → 源码树 providers（本地开发）。
    #[arg(long)]
    providers: Option<String>,
    /// 跳过旧版数据导入
    #[arg(long, default_value_t = false)]
    no_import: bool,
    /// 安装到 %LOCALAPPDATA%\aigw2（快捷方式 + 自启）后退出
    #[arg(long, default_value_t = false)]
    install: bool,
    /// 卸载（保留数据）
    #[arg(long, default_value_t = false)]
    uninstall: bool,
    /// 彻底卸载（删除数据）
    #[arg(long, default_value_t = false)]
    purge: bool,
}

fn resolve_providers(cli: &Cli) -> Option<PathBuf> {
    if let Some(p) = &cli.providers {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("providers")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("providers"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../providers"));
    candidates.into_iter().find(|p| p.is_dir())
}

/// v2 数据目录：%LOCALAPPDATA%\aigw2\data（独立于旧版，导入采用只读语义）
fn data_dir() -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .map(|l| PathBuf::from(l).join("aigw2").join("data"))
        .unwrap_or_else(|_| PathBuf::from("./data"))
}

/// 旧版数据目录：%LOCALAPPDATA%\aigw-panel-rs\data
fn legacy_dir() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .map(|l| PathBuf::from(l).join("aigw-panel-rs").join("data"))
        .filter(|p| p.exists())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // 安装模式优先：安装/卸载后退出
    if cli.install {
        std::process::exit(install::run_install());
    }
    if cli.uninstall {
        std::process::exit(install::run_uninstall());
    }
    if cli.purge {
        std::process::exit(install::run_purge());
    }

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let providers_dir = resolve_providers(&cli);
    let mut specs = match &providers_dir {
        Some(dir) => match aigw_registry::load_dir(dir) {
            Ok(v) => {
                tracing::info!("registry：{} 个平台 spec（{}）", v.len(), dir.display());
                v
            }
            Err(e) => {
                tracing::warn!("registry 加载失败，空表启动：{e}");
                Vec::new()
            }
        },
        None => {
            tracing::warn!("providers 目录未找到，空表启动（可用 --providers 指定）");
            Vec::new()
        }
    };

    // settings：旧版 settings.json 只读导入占位符键（@lobster → lobster_server）
    let mut settings: std::collections::BTreeMap<String, String> = Default::default();
    if let Some(legacy) = legacy_dir() {
        let sp = legacy.join("settings.json");
        if sp.exists() {
            if let Ok(raw) = std::fs::read_to_string(&sp) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    for k in ["lobster_server"] {
                        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
                            settings.insert(k.to_string(), s.to_string());
                        }
                    }
                }
            }
        }
    }
    aigw_registry::apply_settings(&mut specs, &settings);
    let n_placeholder = specs.iter().filter(|s| s.base_url.starts_with('@')).count();
    if n_placeholder > 0 {
        tracing::warn!("{n_placeholder} 个平台 base_url 仍为未配置占位符（对应平台请求将失败）");
    }

    let dd = data_dir();
    let store = Arc::new(aigw_store::Store::open(&dd));
    tracing::info!(
        "store：{}（{}）",
        if store.is_db() { "SQLite" } else { "JSON 降级" },
        dd.display()
    );

    if !cli.no_import {
        if let Some(legacy) = legacy_dir() {
            match store.import_legacy(&legacy) {
                Ok(r) => tracing::info!(
                    "旧数据导入：{} 条（跳过 ts ≤ {:.0} 的既有数据）",
                    r.imported,
                    r.from_max_ts
                ),
                Err(e) => tracing::warn!("旧数据导入失败：{e}"),
            }
        }
    }

    let pool = aigw_pool::Pool::new();
    // 出站代理：启动时加载 data/proxy.json 到全局（OutClient 构造即生效；/api/proxy 可热更新）
    let pcfg = aigw_relay::proxy::load(&data_dir());
    aigw_relay::proxy::apply(&pcfg);
    if pcfg.mode != "direct" {
        tracing::info!("proxy：mode={} url={}", pcfg.mode, pcfg.url);
    }
    let client = Arc::new(aigw_relay::OutClient::new());
    // 账号池：双源装载（v2 data/accounts.json 权威；首次从 legacy 只读初始化）
    let legacy_accounts = legacy_dir()
        .map(|l| l.join("accounts.json"))
        .unwrap_or_else(|| PathBuf::from("accounts-legacy.json"));
    let own_accounts = dd.join("accounts.json");
    let accounts = Arc::new(aigw_store::AccountStore::load_dual(
        &legacy_accounts,
        &own_accounts,
    ));
    // 自定义 API 存储（data/custom.json）
    let custom = Arc::new(aigw_store::CustomStore::open(&dd));
    // OAuth 本地回调监听（Codex 1455 / Claude 54545）——跳转登录自动捕获
    aigw_relay::auth_login::start_callback_listeners();
    let app = aigw_server::build_app(aigw_server::AppState::new(
        specs, store, pool, client, accounts, custom,
    ));

    let addr = format!("{}:{}", cli.host, cli.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 {addr} 失败：{e}（已有实例在跑？换 --port 试试）");
            std::process::exit(1);
        }
    };
    tracing::info!("aigw v2 final listening on http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("serve error: {e}");
        std::process::exit(1);
    }
}
