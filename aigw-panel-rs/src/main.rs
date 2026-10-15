//! aigw-panel 二进制入口：CLI 解析 → 日志初始化 → 状态装配 → HTTP 服务。
//!
//! 大厂级风格：bin 只做「装配 + 启动」，所有业务在 lib（`aigw_panel`）里，
//! 便于集成测试直接调用 `build_app` 走 tower oneshot，无需真实监听端口。
//!
//! ★ windows_subsystem="windows"：GUI 子系统，双击启动**不再弹黑色控制台窗口**
//!   （2026-10-15 用户反馈「为啥启动还附带命令窗口」）。日志改走面板内日志文件，
//!   需要看日志用 `--console` 或直接看 data/panel.log。
// 编译期开关：`--features console` 可保留控制台（调试用）
#![cfg_attr(not(feature = "console"), windows_subsystem = "windows")]

use aigw_panel::catalog::Catalog;
use aigw_panel::core::config::{self, Settings};
use aigw_panel::core::model::VERSION;
use aigw_panel::platforms::Registry;
use aigw_panel::router::Router as RouteEngine;
use aigw_panel::server::{build_app, state::AppState};
use clap::Parser;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Parser, Debug)]
#[command(
    name = "aigw-panel",
    about = "本地 AI 资源整合网关面板（Rust · 大厂级分层架构）",
    version = VERSION
)]
struct Cli {
    /// 覆盖面板端口（默认 8800，或读 settings.json / 环境变量 AIGW_PANEL_PORT）
    #[arg(long, env = "AIGW_PANEL_PORT")]
    port: Option<u16>,

    /// 绑定地址（默认 127.0.0.1）
    #[arg(long)]
    bind: Option<String>,

    /// 覆盖静态资源目录（默认自动探测 / AIGW_STATIC_DIR）
    #[arg(long = "static-dir")]
    static_dir: Option<PathBuf>,

    /// 配置文件目录（默认取 exe 同级 data/）
    #[arg(long)]
    config: Option<PathBuf>,

    /// 禁止启动后自动打开浏览器（默认对齐 Python 版：1.5s 后自动开面板）
    #[arg(long)]
    no_open: bool,
}

/// 打开默认浏览器访问面板（对齐 Python `_open_browser`：cmd /c start 最稳，
/// 新进程开 URL，绕开 GUI 线程 apartment 问题）。失败只记日志不致命。
fn open_browser(url: &str) {
    aigw_panel::server::open_browser(url)
}

#[tokio::main]
async fn main() {
    // ── 结构化日志 ─────────────────────────────────────────────
    // GUI 子系统（windows_subsystem）下没有 stdout：日志落到 data/panel.log
    //（追加，10MB 轮转），同时保留 stdout（console feature / 手动 --console 时可见）。
    let log_dir = config::data_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("panel.log");
    let logfile = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    // 简单大小轮转：>10MB 时改名 panel.log.1 重新开
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() > 10 * 1024 * 1024 {
            let _ = std::fs::rename(&log_path, log_dir.join("panel.log.1"));
        }
    }
    let file_appender = tracing_appender::rolling::never(&log_dir, "panel.log");
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);
    // _guard 需要活到进程结束：leak 出去（面板是常驻进程，一次初始化）
    std::mem::forget(_guard);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(tracing_subscriber::fmt::writer::MakeWriterExt::and(
            std::io::stdout,
            writer,
        ))
        .with_ansi(false)
        .init();
    tracing::info!("日志文件：{}", log_path.display());

    let cli = Cli::parse();

    // ── 配置装配（CLI > env > settings.json > default） ──────────
    let mut settings: Settings = match &cli.config {
        Some(dir) => {
            // 显式指定配置目录：临时切换 data_dir 读取（load_settings 默认 exe 同级）
            let path = dir.join("settings.json");
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
                .unwrap_or_default()
        }
        None => config::load_settings(),
    };
    if let Some(p) = cli.port {
        settings.port = p;
    }
    if let Some(b) = cli.bind {
        settings.bind_host = b;
    }
    if let Some(s) = cli.static_dir {
        settings.static_dir = s.to_string_lossy().into_owned();
    }
    let port = settings.port;
    let bind_host = settings.bind_host.clone();
    aigw_panel::server::handlers::api::set_panel_port(port);

    // ── 端口预检（固定端口策略：被占 = 已有实例在跑 → 直接打开面板，双击永远有反应） ──
    // ★ 2026-10-15（用户反馈「打不开」）：原实现 spawn cmd start 后 sleep 0.8s
    //   就 exit(0)——GUI 子系统下短命进程的子进程经常来不及完成进程创建，
    //   浏览器没拉起、又没有控制台输出 → 用户双击后毫无反应。
    //   改为：同步等多重兜底（explorer → cmd start → rundll32），确认拉起再退。
    match std::net::TcpListener::bind((bind_host.as_str(), port)) {
        Ok(l) => drop(l),
        Err(e) => {
            let url = format!("http://{}:{}/", bind_host, port);
            tracing::warn!(port, error = %e, "端口已被占用 —— 已有 aigw-panel 实例在运行，直接打开面板");
            // explorer 优先（同步 spawn，必在交互桌面拉起默认浏览器）
            let opened = std::process::Command::new("explorer.exe").arg(&url).spawn().is_ok();
            if !opened {
                // 兜底：rundll32 url.dll（不依赖 cmd 的短命进程时序）
                let _ = std::process::Command::new("rundll32")
                    .args(["url.dll,FileProtocolHandler", &url])
                    .spawn();
            }
            // 给浏览器拉起留足时间再退出（GUI 进程退出会连带清理未完成的 spawn）
            std::thread::sleep(std::time::Duration::from_millis(2500));
            std::process::exit(0);
        }
    }

    // ── 状态装配 ──────────────────────────────────────────────
    let catalog = Catalog::load();
    let router = Arc::new(RouteEngine::new());
    router.register_auto_free("https://text.pollinations.ai/openai");
    let registry = Arc::new(Registry::build(Arc::new(catalog.clone()), config::data_dir()));
    let login = Arc::new(aigw_panel::server::login::LoginManager::new(config::data_dir()));
    let usage = Arc::new(aigw_panel::server::usage::UsageRecorder::new());

    // 模型开关持久化（data/disabled_models.json，重启仍生效）
    let disabled: HashSet<String> = aigw_panel::server::store::load("disabled_models")
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let state = AppState {
        inner: Arc::new(aigw_panel::server::state::AppStateInner {
            settings: RwLock::new(settings),
            catalog,
            router: router.clone(),
            registry,
            disabled: RwLock::new(disabled),
            started: std::time::Instant::now(),
            login,
            usage,
        }),
    };

    // ── 启动探针：探 Pollinations 免费车道延迟 ──────────────────
    let client = reqwest::Client::new();
    router.probe_all(&client).await;

    let n_platforms = state
        .inner
        .catalog
        .platforms()
        .as_object()
        .map(|m| m.len())
        .unwrap_or(0);
    let n_registered = state.inner.registry.ids().len();

    let addr = SocketAddr::new(
        bind_host
            .parse()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
        port,
    );

    tracing::info!(
        version = VERSION,
        %addr,
        platforms = n_platforms,
        registered = n_registered,
        "AI 资源整合网关面板 (Rust · 大厂级分层) 启动"
    );
    tracing::info!(url = %format!("http://{}:{}/", bind_host, port), "面板地址");
    tracing::info!("免费车道: Pollinations(gpt-oss-20b) + 上游透传；逐平台登录/原生中继按 relay_spec 接入中");

    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    // ── 自动开浏览器（对齐 Python 版：启动 1.5s 后打开面板） ─────
    if !cli.no_open && std::env::var("AIGW_NO_OPEN").is_err() {
        let url = format!("http://{}:{}/", bind_host, port);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            open_browser(&url);
        });
    }

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "HTTP 服务异常退出");
        std::process::exit(1);
    }
}
