//! 本机反代上游管理器（移植自 app/localproxy.py，把 CLIProxyAPI 集成进面板）。
//!
//! 面板负责：释放/定位内置二进制 → 生成 config.yaml → 启停 + 状态探活 → 触发 OAuth 登录。
//! CLIProxyAPI 属**可选**外部件（默认不内置），缺失时如实上报 installed=false，不伪造状态。
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;

pub const VERSION: &str = "8.0.13";
pub const DEFAULT_PORT: u16 = 8318;
pub const DEFAULT_KEY: &str = "aigw-local-key";
const STAMP: &str = ".binary.stamp";

/// 支持的 OAuth 登录（--help 实测，v8.0.13；无 CodeBuddy / Qoder）。
pub const PROVIDERS: &[(&str, &str, &str, &str)] = &[
    ("kimi", "-kimi-login", "Kimi", "kimi.com 的 Kimi Code 订阅，设备码流程"),
    ("kimi-ai", "-kimi-ai-login", "Kimi.ai", "Kimi 国际站"),
    ("codex", "-codex-login", "Codex（OpenAI）", "ChatGPT Plus/Pro 订阅里的 Codex"),
    ("codex-device", "-codex-device-login", "Codex（设备码）", "没有回调端口时用这个"),
    ("claude", "-claude-login", "Claude Code", "Anthropic 订阅（Pro/Max）"),
    ("antigravity", "-antigravity-login", "Antigravity", "Google Antigravity（Gemini 系）"),
    ("xai", "-xai-login", "xAI Grok", "Grok Build 订阅"),
    ("devin", "-devin-login", "Devin", "Devin 订阅"),
    ("meta", "-meta-login", "Meta Muse", "Meta 登录接入 Muse Code"),
];

static PROC: Mutex<Option<Child>> = Mutex::new(None);
static LOGIN_PROC: Mutex<Option<Child>> = Mutex::new(None);

fn data_dir() -> PathBuf {
    let d = crate::core::config::data_dir().join("cliproxy");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn bin_dir() -> PathBuf {
    let d = data_dir().join("bin");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn auth_dir() -> PathBuf {
    let d = data_dir().join("auths");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn config_path() -> PathBuf {
    data_dir().join("config.yaml")
}

fn log_path() -> PathBuf {
    data_dir().join("service.log")
}

/// 候选二进制路径（内置释放位 / 开发期仓库位 / 环境变量）。
fn binary_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = std::env::var("AIGW_CLIPROXY_EXE") {
        v.push(PathBuf::from(p));
    }
    v.push(bin_dir().join("cli-proxy-api.exe"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            // <dist>/../../cli-proxy/cli-proxy-api.exe（开发期：dist 在 target 下）
            v.push(p.join("../../cli-proxy/cli-proxy-api.exe"));
        }
    }
    v.push(PathBuf::from("cli-proxy/cli-proxy-api.exe"));
    v
}

/// 返回 (path, exists, size, bundled)
pub fn binary_info() -> (String, bool, u64, bool) {
    for c in binary_candidates() {
        if let Ok(md) = std::fs::metadata(&c) {
            if md.is_file() {
                let bundled = c.starts_with(bin_dir());
                return (c.display().to_string(), true, md.len(), bundled);
            }
        }
    }
    (String::new(), false, 0, false)
}

/// 确保二进制在持久目录（当前实现：直接返回可用路径，不做内置释放）。
pub fn ensure_binary() -> Option<PathBuf> {
    for c in binary_candidates() {
        if c.is_file() {
            // 若来自开发期仓库位，写一份 stamp 便于后续判断
            if !c.starts_with(bin_dir()) {
                if let Ok(md) = std::fs::metadata(&c) {
                    let _ = std::fs::write(bin_dir().join(STAMP), md.len().to_string());
                }
            }
            return Some(c);
        }
    }
    None
}

fn default_config(port: u16, key: &str) -> String {
    format!(
        "# 由 AI 资源网关面板自动生成，可手工编辑\n\
         host: \"127.0.0.1\"\n\
         port: {}\n\
         auth-dir: \"auths\"\n\
         api-keys:\n  - \"{}\"\n\
         debug: false\n\
         logging-to-file: false\n\
         usage-statistics-enabled: false\n\
         proxy-url: \"\"\n\
         request-retry: 2\n",
        port, key
    )
}

pub fn read_config() -> (String, u16, Vec<String>) {
    let p = config_path();
    let mut host = "127.0.0.1".to_string();
    let mut port = DEFAULT_PORT;
    let mut keys: Vec<String> = Vec::new();
    if let Ok(txt) = std::fs::read_to_string(&p) {
        for line in txt.lines() {
            let s = line.trim();
            if let Some(rest) = s.strip_prefix("host:") {
                host = rest.trim().trim_matches('"').trim_matches('\'').to_string();
            } else if let Some(rest) = s.strip_prefix("port:") {
                if let Ok(n) = rest.trim().split('#').next().unwrap_or("").trim().parse::<u16>() {
                    port = n;
                }
            }
        }
        // api-keys 列表
        let mut in_keys = false;
        for line in txt.lines() {
            let s = line.trim();
            if s.starts_with("api-keys:") {
                in_keys = true;
                continue;
            }
            if in_keys {
                if let Some(rest) = s.strip_prefix('-') {
                    keys.push(rest.trim().trim_matches('"').trim_matches('\'').to_string());
                } else if !s.is_empty() && !s.starts_with('#') {
                    in_keys = false;
                }
            }
        }
    }
    (host, port, keys)
}

pub fn write_config(port: Option<u16>, key: Option<&str>, host: Option<&str>) -> (bool, String) {
    let (cur_host, cur_port, cur_keys) = read_config();
    let port = port.unwrap_or(if cur_port == 0 { DEFAULT_PORT } else { cur_port });
    let host = host.map(|s| s.to_string()).unwrap_or(cur_host);
    let key = key
        .map(|s| s.to_string())
        .or_else(|| cur_keys.first().cloned())
        .unwrap_or_else(|| DEFAULT_KEY.to_string());
    let p = config_path();
    if std::fs::write(&p, default_config(port, &key)).is_ok() {
        (true, format!("已生成/更新配置（端口 {}）", port))
    } else {
        (false, format!("写入配置失败：{}", p.display()))
    }
}

fn port_busy(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        std::time::Duration::from_millis(800),
    )
    .is_ok()
}

fn pick_port(prefer: u16) -> u16 {
    for i in 0..20u16 {
        let p = prefer + i;
        if !port_busy(p) {
            return p;
        }
    }
    prefer
}

async fn http_models(port: u16, key: &str) -> (u16, String) {
    let url = format!("http://127.0.0.1:{}/v1/models", port);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut req = client.get(&url).header("Accept", "application/json");
    if !key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", key));
    }
    match req.send().await {
        Ok(r) => (r.status().as_u16(), r.text().await.unwrap_or_default()),
        Err(_) => (0, String::new()),
    }
}

fn login_running() -> bool {
    LOGIN_PROC
        .lock()
        .ok()
        .and_then(|mut g| g.as_mut().map(|c| c.try_wait().ok().flatten().is_none()))
        .unwrap_or(false)
}

fn auth_files() -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(auth_dir()) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    out.push(json!({
                        "name": e.file_name().to_string_lossy(),
                        "size": md.len(),
                        "mtime": "",
                    }));
                }
            }
        }
    }
    out
}

/// 状态：真实探活 +v1/models，不伪造 online。
pub async fn status() -> Value {
    let (exe, exists, size, bundled) = binary_info();
    let (_host, port, keys) = read_config();
    let key = keys.first().cloned().unwrap_or_else(|| DEFAULT_KEY.to_string());
    let (code, raw) = if exists {
        http_models(port, &key).await
    } else {
        (0, String::new())
    };
    let mut models = 0i64;
    let mut model_ids: Vec<Value> = Vec::new();
    let mut note = String::new();
    if code == 200 {
        if let Ok(d) = serde_json::from_str::<Value>(&raw) {
            if let Some(arr) = d.get("data").and_then(|v| v.as_array()) {
                models = arr.len() as i64;
                model_ids = arr.iter().take(50).map(|x| x.get("id").cloned().unwrap_or(Value::Null)).collect();
                note = if models > 0 { format!("在线，{} 个模型", models) } else { "在线，但还没登录任何账号".to_string() };
            } else {
                note = "在线（响应不是标准模型列表）".to_string();
            }
        } else {
            note = "在线（非 JSON）".to_string();
        }
    } else if code == 401 {
        note = "服务在跑，但 API Key 不对".to_string();
    } else if code == 404 {
        note = "端口有响应但没有 /v1/models，可能不是这个服务".to_string();
    } else if code != 0 {
        note = format!("HTTP {}", code);
    } else {
        note = "未运行".to_string();
    }
    let providers: Vec<Value> = PROVIDERS
        .iter()
        .map(|(id, flag, name, note)| json!({"id": id, "flag": flag, "name": name, "note": note}))
        .collect();
    json!({
        "ok": true,
        "installed": exists,
        "exe": exe,
        "bundled": bundled,
        "size_mb": ((size as f64) / 1048576.0 * 10.0).round() / 10.0,
        "version": VERSION,
        "host": "127.0.0.1",
        "port": port,
        "api_key": key,
        "base_url": format!("http://127.0.0.1:{}/v1", port),
        "config_path": config_path().display().to_string(),
        "auth_dir": auth_dir().display().to_string(),
        "log_path": log_path().display().to_string(),
        "online": code == 200,
        "code": code,
        "models": models,
        "model_ids": model_ids,
        "note": note,
        "providers": providers,
        "auth_files": auth_files(),
        "login_status": {"running": login_running()},
    })
}

/// 启动服务；返回 (ok, message)。
pub async fn start(port: Option<u16>, key: Option<&str>) -> (bool, String) {
    let Some(exe) = ensure_binary() else {
        return (false, "没有找到 cli-proxy-api.exe（内置包缺失或没释放成功）".to_string());
    };
    let st = status().await;
    if st.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
        return (true, format!("已经在运行（{}）", st.get("base_url").and_then(|v| v.as_str()).unwrap_or("")));
    }
    let mut cfg_port = port.unwrap_or_else(|| st.get("port").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_PORT as u64) as u16);
    if port_busy(cfg_port) {
        cfg_port = pick_port(cfg_port);
    }
    let (ok, msg) = write_config(Some(cfg_port), key, None);
    if !ok {
        return (false, msg);
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let logf = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
        .ok();
    let logf2 = logf.as_ref().and_then(|f| f.try_clone().ok());
    let mut cmd = Command::new(&exe);
    cmd.arg("--config")
        .arg(config_path())
        .current_dir(data_dir())
        .creation_flags(CREATE_NO_WINDOW);
    if let Some(f) = logf {
        cmd.stdout(f);
    }
    if let Some(f2) = logf2 {
        cmd.stderr(f2);
    }
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            if let Ok(mut g) = PROC.lock() {
                *g = Some(child);
            }
            for _ in 0..30 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if http_models(cfg_port, "").await.0 == 200 || port_busy(cfg_port) {
                    return (true, format!("已启动 http://127.0.0.1:{}/v1（PID {}）", cfg_port, pid));
                }
            }
            (false, format!("启动了但没监听端口，看 {}", log_path().display()))
        }
        Err(e) => (false, format!("启动失败：{}", e)),
    }
}

/// 停止服务；返回被杀的 PID 列表。
pub fn stop() -> Vec<u32> {
    let mut killed = Vec::new();
    if let Ok(mut g) = PROC.lock() {
        if let Some(c) = g.as_mut() {
            let pid = c.id();
            let _ = c.kill();
            killed.push(pid);
        }
        *g = None;
    }
    // 兜底：按镜像名杀
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(out) = Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq cli-proxy-api.exe", "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 2 {
                    let pid: String = parts[1].trim().trim_matches('"').to_string();
                    if let Ok(n) = pid.parse::<u32>() {
                        if !killed.contains(&n) {
                            let _ = Command::new("taskkill")
                                .args(["/F", "/PID", &n.to_string()])
                                .creation_flags(CREATE_NO_WINDOW)
                                .output();
                            killed.push(n);
                        }
                    }
                }
            }
        }
    }
    killed
}

pub async fn restart(port: Option<u16>, key: Option<&str>) -> (bool, String) {
    stop();
    start(port, key).await
}

/// 触发 OAuth 登录，从子进程 stdout 抓授权链接。返回 {provider,...,url,user_code,raw,pid}。
pub async fn start_login(provider_id: &str) -> (bool, Value) {
    let Some(exe) = ensure_binary() else {
        return (false, json!({"message": "没有找到 cli-proxy-api.exe"}));
    };
    let Some((pid, flag, name, _)) = PROVIDERS.iter().find(|(id, _, _, _)| *id == provider_id) else {
        return (false, json!({"message": format!("未知的登录方式：{}", provider_id)}));
    };
    let st = status().await;
    if !st.get("online").and_then(|v| v.as_bool()).unwrap_or(false) {
        let (okk, msg) = start(None, None).await;
        if !okk {
            return (false, json!({"message": format!("服务没起来，登录不了：{}", msg)}));
        }
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new(&exe);
    cmd.arg(flag)
        .arg("--config")
        .arg(config_path())
        .arg("--no-browser")
        .current_dir(data_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, json!({"message": format!("启动登录失败：{}", e)})),
    };
    let child_pid = child.id();
    if let Ok(mut g) = LOGIN_PROC.lock() {
        *g = Some(child);
    }
    // 读取 stdout 抓 URL / user code（有超时）
    let mut url = String::new();
    let mut user_code = String::new();
    let mut raw = String::new();
    let read = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader};
        let mut out = String::new();
        if let Ok(mut g) = LOGIN_PROC.lock() {
            if let Some(c) = g.as_mut() {
                if let Some(so) = c.stdout.take() {
                    let reader = BufReader::new(so);
                    let t0 = std::time::Instant::now();
                    for line in reader.lines().map_while(Result::ok) {
                        out.push_str(&line);
                        out.push('\n');
                        if t0.elapsed().as_secs() > 25 {
                            break;
                        }
                    }
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default();
    raw = read;
    let url_re = |s: &str| -> Option<String> {
        let idx = s.find("http")?;
        let rest = &s[idx..];
        let end = rest.find(|c: char| c == ' ' || c == '\n' || c == '\r').unwrap_or(rest.len());
        Some(rest[..end].trim_end_matches(|c| c == '.' || c == ',' || c == ';' || c == ')').to_string())
    };
    for line in raw.lines() {
        if url.is_empty() && !line.to_lowercase().contains("visit") {
            if let Some(u) = url_re(line) {
                if !u.contains("github") {
                    url = u;
                }
            }
        }
        if user_code.is_empty() {
            let lower = line.to_lowercase();
            if let Some(p) = lower.find("user code:") {
                user_code = line[p + 10..].trim().to_string();
            }
        }
    }
    (
        true,
        json!({
            "provider": pid, "name": name, "flag": flag,
            "url": url, "user_code": user_code,
            "raw": raw.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>(),
            "pid": child_pid,
            "note": "授权完成后凭据会自动落到 auths 目录，服务热加载不用重启",
        }),
    )
}

pub fn tail_log(n: usize) -> String {
    let p = log_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            let lines: Vec<&str> = s.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}
