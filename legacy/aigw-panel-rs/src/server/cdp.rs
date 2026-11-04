//! CDP 受控浏览器 —— 真正「一键获取 Cookie」。
//!
//! 背景（2026-10-15 实测）：直接读本机浏览器 Cookie 库这条路走不通 ——
//!   · Chrome/Edge 127+ 用 **app-bound 加密（v20）**，外部进程解不开；
//!   · 第三方浏览器（雨见等）会把 Cookies 文件**独占锁定**，连共享读都被拒；
//!   · 面板还得知道每个浏览器的 profile 路径（雨见之类根本不在列表里）。
//!
//! 正解就是 Python 版当年用的办法（`app/cdp.py`）：**面板自己拉起一个带远程调试端口的
//! Chromium 浏览器**（独立 profile，登录态持久），用户在里面登录后，经 **CDP 直接读出
//! 明文 cookie** —— 不涉及任何解密、不受 app-bound / 锁库限制。
//!
//! ⚠ 仓库离线（无法 `cargo add`），所以这里**手写最小 WebSocket 客户端**：
//!   HTTP Upgrade 握手 + 文本帧收发（含分片、ping/pong），够 CDP 用。

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// 固定调试端口（独立于面板 8800）
pub const CDP_PORT: u16 = 9333;

fn base64_std(data: &[u8]) -> String {
    crate::server::loomy_auth::base64_std(data)
}

/// 找可用的 Chromium 系浏览器（Edge 优先 —— Windows 自带，一定有）
pub fn find_browser() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = vec![
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe".into(),
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe".into(),
        r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(),
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe".into(),
    ];
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        cands.push(PathBuf::from(&la).join(r"Google\Chrome\Application\chrome.exe"));
        cands.push(PathBuf::from(&la).join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    cands.into_iter().find(|p| p.is_file())
}

/// 受控浏览器专用 profile（放在面板 data 目录下，登录态持久）
pub fn profile_dir() -> PathBuf {
    crate::core::config::data_dir().join("cdp-profile")
}

/// 调试端口是否已在监听（浏览器是否已起来）
pub fn is_listening() -> bool {
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", CDP_PORT).parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// 拉起受控浏览器并打开 url（已在运行则只导航到新标签页）
pub fn launch(url: &str) -> Result<()> {
    let exe = find_browser().ok_or_else(|| anyhow!("没找到 Edge / Chrome，无法启动受控浏览器"))?;
    let prof = profile_dir();
    std::fs::create_dir_all(&prof)?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(format!("--remote-debugging-port={}", CDP_PORT))
        .arg(format!("--user-data-dir={}", prof.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-sync")
        .arg(url);
    cmd.spawn().map_err(|e| anyhow!("启动受控浏览器失败：{}", e))?;
    Ok(())
}

/// 确保浏览器已起来并打开 url（最多等 ~20s）
pub async fn ensure(url: &str) -> Result<()> {
    if !is_listening() {
        launch(url)?;
    } else {
        // 已在跑：开个新标签页导航过去。
        // ★ CDP 的 /json/new 在 Chrome/Edge 111+ **只接受 PUT**（GET 会 405），这里必须用 PUT。
        if let Ok(c) = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            let _ = c
                .put(format!("http://127.0.0.1:{}/json/new?{}", CDP_PORT, url))
                .send()
                .await;
        }
    }
    for _ in 0..40 {
        if is_listening() {
            // 端口通了再确认 /json/version 可读
            if ws_url().await.is_ok() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(anyhow!("受控浏览器启动超时（端口 {}）", CDP_PORT))
}

async fn http_get_json(path: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let r = client
        .get(format!("http://127.0.0.1:{}{}", CDP_PORT, path))
        .send()
        .await?;
    Ok(serde_json::from_str(&r.text().await?)?)
}

/// 浏览器级 WebSocket 调试地址
pub async fn ws_url() -> Result<String> {
    let v = http_get_json("/json/version").await?;
    v.get("webSocketDebuggerUrl")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("CDP /json/version 无 webSocketDebuggerUrl"))
}

/// 页面级 WebSocket 调试地址（兜底通道）
async fn page_ws_url() -> Result<String> {
    let list = http_get_json("/json").await?;
    let arr = list.as_array().cloned().unwrap_or_default();
    for t in arr {
        if t.get("type").and_then(|x| x.as_str()) == Some("page") {
            if let Some(u) = t.get("webSocketDebuggerUrl").and_then(|x| x.as_str()) {
                return Ok(u.to_string());
            }
        }
    }
    Err(anyhow!("CDP /json 里没有可用的 page 目标"))
}

// ---------------------------------------------------------------- 最小 WebSocket
struct Ws {
    stream: TcpStream,
}

impl Ws {
    async fn connect(ws_url: &str) -> Result<Ws> {
        // ws://127.0.0.1:9333/devtools/browser/<id>
        let rest = ws_url.strip_prefix("ws://").unwrap_or(ws_url);
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let stream = TcpStream::connect(authority).await?;
        let mut stream = stream;
        let key = base64_std(&pseudo_random(16));
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
            path, authority, key
        );
        stream.write_all(req.as_bytes()).await?;
        // 读到 \r\n\r\n
        let mut hdr = Vec::new();
        let mut b = [0u8; 1];
        loop {
            let n = stream.read(&mut b).await?;
            if n == 0 {
                return Err(anyhow!("WS 握手失败：连接被关闭"));
            }
            hdr.push(b[0]);
            if hdr.len() >= 4 && &hdr[hdr.len() - 4..] == b"\r\n\r\n" {
                break;
            }
            if hdr.len() > 16384 {
                return Err(anyhow!("WS 握手响应头过长"));
            }
        }
        let head = String::from_utf8_lossy(&hdr);
        if !head.starts_with("HTTP/1.1 101") {
            return Err(anyhow!("WS 握手被拒：{}", head.lines().next().unwrap_or("")));
        }
        Ok(Ws { stream })
    }

    /// 发一个文本帧（客户端必须掩码）
    async fn send_text(&mut self, text: &str) -> Result<()> {
        let payload = text.as_bytes();
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x81); // FIN + text
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask = pseudo_random(4);
        frame.extend_from_slice(&mask);
        for (i, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[i % 4]);
        }
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn read_exact_vec(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.stream.read_exact(&mut v).await?;
        Ok(v)
    }

    /// 读一个完整消息（自动拼分片、回 pong）
    async fn read_message(&mut self) -> Result<String> {
        let mut out: Vec<u8> = Vec::new();
        loop {
            let h = self.read_exact_vec(2).await?;
            let fin = h[0] & 0x80 != 0;
            let opcode = h[0] & 0x0F;
            let masked = h[1] & 0x80 != 0;
            let mut len = (h[1] & 0x7F) as u64;
            if len == 126 {
                let e = self.read_exact_vec(2).await?;
                len = u16::from_be_bytes([e[0], e[1]]) as u64;
            } else if len == 127 {
                let e = self.read_exact_vec(8).await?;
                len = u64::from_be_bytes(e.try_into().unwrap());
            }
            let key = if masked {
                Some(self.read_exact_vec(4).await?)
            } else {
                None
            };
            if len > 32 * 1024 * 1024 {
                return Err(anyhow!("WS 帧过大：{}", len));
            }
            let mut payload = self.read_exact_vec(len as usize).await?;
            if let Some(k) = key {
                for (i, byte) in payload.iter_mut().enumerate() {
                    *byte ^= k[i % 4];
                }
            }
            match opcode {
                0x9 => {
                    // ping → pong
                    let mut f = vec![0x8A, 0x80];
                    let m = pseudo_random(4);
                    f.extend_from_slice(&m);
                    f.extend_from_slice(&[]);
                    self.stream.write_all(&f).await?;
                }
                0x8 => return Err(anyhow!("WS 被对端关闭")),
                0xA => {} // pong 忽略
                _ => {
                    out.extend_from_slice(&payload);
                    if fin {
                        return Ok(String::from_utf8_lossy(&out).into_owned());
                    }
                }
            }
        }
    }

    /// 发一条 CDP 命令并等它对应的回包
    async fn rpc(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        self.send_text(&msg).await?;
        for _ in 0..200 {
            let text = self.read_message().await?;
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return Ok(v);
            }
        }
        Err(anyhow!("CDP 等回包超时：{}", method))
    }
}

/// 伪随机字节（仅用于 WS 掩码/握手 key，非安全用途）
fn pseudo_random(n: usize) -> Vec<u8> {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        ^ (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

/// 读全部 cookie（先走浏览器级 Storage.getCookies，失败退回页面级 Network.getAllCookies）
pub async fn all_cookies() -> Result<Vec<Value>> {
    let url = ws_url().await?;
    let mut ws = Ws::connect(&url).await?;
    let r = ws.rpc(1, "Storage.getCookies", json!({})).await?;
    if let Some(arr) = r.pointer("/result/cookies").and_then(|x| x.as_array()) {
        return Ok(arr.clone());
    }
    drop(ws);
    let purl = page_ws_url().await?;
    let mut ws2 = Ws::connect(&purl).await?;
    let r2 = ws2.rpc(1, "Network.getAllCookies", json!({})).await?;
    r2.pointer("/result/cookies")
        .and_then(|x| x.as_array())
        .cloned()
        .ok_or_else(|| anyhow!("CDP 未返回 cookies：{}", r))
}

/// 按域名过滤并拼成 Cookie 请求头（host 命中规则：cookie 域去掉前导点后是 host 的后缀之一）
pub fn cookie_header_for(cookies: &[Value], hosts: &[String]) -> String {
    let hosts: Vec<String> = hosts.iter().map(|h| h.trim().trim_start_matches('.').to_lowercase()).collect();
    let mut parts: Vec<String> = Vec::new();
    for c in cookies {
        let dom = c
            .get("domain")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim_start_matches('.')
            .to_lowercase();
        if dom.is_empty() {
            continue;
        }
        let hit = hosts.iter().any(|h| h == &dom || h.ends_with(&format!(".{}", dom)) || dom.ends_with(&format!(".{}", h)));
        if !hit {
            continue;
        }
        if let (Some(n), Some(v)) = (
            c.get("name").and_then(|x| x.as_str()),
            c.get("value").and_then(|x| x.as_str()),
        ) {
            if !n.is_empty() {
                parts.push(format!("{}={}", n, v));
            }
        }
    }
    parts.join("; ")
}

/// 递归找 JSON 里是否存在某个非空键（对齐 Python 侧 verify 的宽松判定）
fn has_key(j: &Value, key: &str) -> bool {
    match j {
        Value::Object(m) => {
            if let Some(v) = m.get(key) {
                match v {
                    Value::Null => {}
                    Value::String(s) if s.is_empty() => {}
                    Value::Array(a) if a.is_empty() => {}
                    _ => return true,
                }
            }
            m.values().any(|v| has_key(v, key))
        }
        Value::Array(a) => a.iter().any(|v| has_key(v, key)),
        _ => false,
    }
}

/// 用平台的 verify 规则确认「这坨 cookie 真的是登录态」。
/// 返回 None = 该平台没有可用 verify 规则（调用方按「未校验」处理）。
/// ★ 2026-10-15：不加这道闸门会把 `BAIDUID` 这类追踪 cookie 误当成登录凭据入库
///   （实测库库只抓到 119 字节就被判成功）。
pub async fn verify_cookie(verify: &Value, cookie: &str) -> Option<bool> {
    let url = verify.get("url").and_then(|v| v.as_str())?;
    if url.is_empty() {
        return None;
    }
    let typ = verify.get("type").and_then(|v| v.as_str()).unwrap_or("cookie");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .ok()?;
    let mut req = client.get(url).header(
        "User-Agent",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    );
    req = if typ == "bearer" {
        req.header("Authorization", format!("Bearer {}", cookie))
    } else {
        req.header("Cookie", cookie)
    };
    let resp = match req.send().await {
        Ok(r) => r,
        Err(_) => return Some(false),
    };
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Some(false);
    }
    let parsed = serde_json::from_str::<Value>(&text).ok();
    // 业务码判定（200 + errno:-6 这类「假成功」）
    if let (Some(f), Some(bad)) = (
        verify.get("errno_field").and_then(|v| v.as_str()),
        verify.get("unlogin_errno").and_then(|v| v.as_i64()),
    ) {
        if let Some(j) = &parsed {
            if j.get(f).and_then(|v| v.as_i64()) == Some(bad) {
                return Some(false);
            }
        }
    }
    // ok_keys：任一键（递归）存在且非空 → 已登录
    if let Some(keys) = verify.get("ok_keys").and_then(|v| v.as_array()) {
        let keys: Vec<&str> = keys.iter().filter_map(|k| k.as_str()).collect();
        if !keys.is_empty() {
            if let Some(j) = &parsed {
                return Some(keys.iter().any(|k| has_key(j, k)));
            }
        }
    }
    if let Some(m) = verify.get("unlogin_msg").and_then(|v| v.as_str()) {
        if text.contains(m) {
            return Some(false);
        }
    }
    Some(true)
}
