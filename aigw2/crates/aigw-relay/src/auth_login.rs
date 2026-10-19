//! 登录引擎（批2）：设备授权/跳转登录 → 凭据自动落账号池。
//!
//! 一手来源：aigw-panel-rs routers/login.rs + wb.rs（2026-10-14/15 实拆沉淀）：
//! - **wb 族（CodeBuddy 国内/国际）**：POST /v2/plugin/auth/state?platform=CLI
//!   （必须带 Origin+Referer+CLI UA，否则 APISIX 拦）→ data.state/authUrl；
//!   用户在授权页登录 → GET /v2/plugin/auth/token?state= → data.accessToken。
//! - **MiniMax Code**：OAuth2 设备码 PKCE S256（client_id=mcode-public）。
//! - **Kimi Code（sub-kimi / sub-kimi-ai）**：RFC 8628 设备码流
//!   （一手来源：CLIProxyAPI v6 internal/auth/kimi，client_id=17e5f671-…-c098；
//!   国内站 OAuth host=auth.kimi.com，国际站=auth.kimi.ai；带 X-Msh-* 头）。
//! - **其余 cookie 型平台**：跳转登录（返回 login_url，用户浏览器打开），
//!   登录后手动粘贴 Cookie/凭据落账号池（v2 不做扫码/受控浏览器收割）。
//! token 拿到即 add_account 持久化（v2 data/accounts.json）。

use crate::OutClient;
use aigw_registry::ProviderSpec;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const CLI_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

/// Kimi Code OAuth（CLIProxyAPI v6 一手来源）。
const KIMI_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_OAUTH_COM: &str = "https://auth.kimi.com";
const KIMI_OAUTH_AI: &str = "https://auth.kimi.ai";
const KIMI_MSH_VERSION: &str = "v6.10.9";

#[derive(Debug, Clone)]
struct Session {
    id: String,
    platform: String,
    /// wb 族 = state；minimax = "minimax:{device_code}:{verifier}:{user_code}"；跳转型为空
    state: String,
    status: String, // pending | success | error
    message: String,
    login_url: String,
    user_code: String,
}

pub struct LoginEngine {
    sessions: Mutex<Vec<Session>>,
}

fn wb_base(platform: &str) -> Option<(String, String)> {
    match platform {
        "wb-gateway" | "codebuddy" => Some((
            "https://copilot.tencent.com".into(),
            "https://www.codebuddy.cn".into(),
        )),
        "wb-gateway-intl" | "codebuddy-intl" => Some((
            "https://www.workbuddy.ai".into(),
            "https://www.workbuddy.ai".into(),
        )),
        _ => None,
    }
}

fn base62_48() -> String {
    const CH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = nanos as u64 ^ 0x9E37_79B9_7F4A_7C15;
    let mut v = String::new();
    for _ in 0..48 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        v.push(CH[(x >> 33) as usize % CH.len()] as char);
    }
    v
}

fn b64url_nopad(d: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(d)
}

impl LoginEngine {
    pub fn new() -> Self {
        LoginEngine {
            sessions: Mutex::new(Vec::new()),
        }
    }

    fn new_id(&self) -> String {
        format!(
            "ls-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )
    }

    /// 发起登录会话。优先级：wb 族设备授权 → minimax 设备码 → spec.login_url 非空跳转登录（手动粘贴凭据）。
    pub async fn start(&self, client: &OutClient, platform: &str, specs: &[ProviderSpec]) -> Value {
        let id = self.new_id();
        let mut sess = Session {
            id: id.clone(),
            platform: platform.to_string(),
            state: String::new(),
            status: "pending".into(),
            message: String::new(),
            login_url: String::new(),
            user_code: String::new(),
        };
        if let Some((base, origin)) = wb_base(platform) {
            match wb_start(client, &base, &origin).await {
                Ok((state, auth_url)) => {
                    sess.state = state;
                    sess.login_url = auth_url;
                    sess.message = "已生成设备授权会话：打开授权页完成登录，面板自动拿到 token 并入库。".into();
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("auth/state 失败：{e}（可回退手动粘贴 token）");
                }
            }
        } else if platform == "minimax" {
            match minimax_start(&client.http).await {
                Ok((device_code, user_code, verify_url, verifier)) => {
                    sess.state = format!("minimax:{device_code}:{verifier}:{user_code}");
                    sess.login_url = verify_url;
                    sess.user_code = user_code.clone();
                    sess.message = format!("MiniMax 设备码：打开验证页输入 {user_code} 完成授权。");
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("设备码申请失败：{e}");
                }
            }
        } else if let Some(oauth_host) = kimi_oauth_host(platform) {
            // Kimi Code 设备码（RFC 8628）：sub-kimi 国内站 / sub-kimi-ai 国际站
            match kimi_start(&client.http, oauth_host).await {
                Ok((device_code, user_code, verify_url)) => {
                    sess.state = format!("kimi:{device_code}");
                    sess.login_url = verify_url.clone();
                    sess.user_code = user_code.clone();
                    sess.message = format!(
                        "Kimi 设备码：打开验证页（{}）输入 {} 完成授权，面板自动拿到 token 入库。",
                        verify_url, user_code
                    );
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("Kimi 设备码申请失败：{e}（可回退手动粘贴 token）");
                }
            }
        } else if let Some(spec) = specs.iter().find(|s| s.id == platform && !s.login_url.is_empty()) {
            // 跳转登录：用户自己浏览器打开登录页，登录后手动粘贴 Cookie/凭据入库。
            sess.login_url = spec.login_url.clone();
            sess.message = format!(
                "已为你打开跳转登录页（{}）—— 请在你自己的浏览器完成登录，然后把 Cookie 或凭据粘贴回来（面板不支持扫码，凭据由你掌控）。",
                spec.login_url
            );
        } else {
            sess.status = "error".into();
            sess.message = format!("平台 {platform} 暂不支持自动登录（手动粘贴凭据到账号池）");
        }
        let out = json!({
            "id": sess.id, "platform": sess.platform, "status": sess.status,
            "message": sess.message, "login_url": sess.login_url,
            "user_code": sess.user_code,
        });
        if let Ok(mut g) = self.sessions.lock() {
            g.retain(|s| s.id != sess.id);
            g.push(sess);
        }
        out
    }

    /// 轮询会话；成功时 token 落账号池。
    pub async fn poll(
        &self,
        client: &OutClient,
        id: &str,
        accounts: &aigw_store::AccountStore,
    ) -> Value {
        let Some(mut sess) = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|s| s.id == id)
            .cloned()
        else {
            return json!({"ok": false, "message": "会话不存在或已过期"});
        };
        if sess.status == "success" {
            return json!({"ok": true, "session": json!({"id": sess.id, "status": sess.status, "message": sess.message})});
        }
        let secret: Result<String, String> = if let Some((base, origin)) = wb_base(&sess.platform) {
            if sess.state.is_empty() {
                Err("state 为空（start 未成功）".to_string())
            } else {
                wb_poll_token(client, &base, &origin, &sess.state).await
            }
        } else if sess.platform == "minimax" {
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("设备码会话不完整".to_string())
            } else {
                minimax_poll(&client.http, parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("kimi:") {
            let device_code = sess.state.trim_start_matches("kimi:").to_string();
            match kimi_oauth_host(&sess.platform) {
                Some(host) => kimi_poll(&client.http, host, &device_code).await,
                None => Err("Kimi 平台映射缺失".to_string()),
            }
        } else {
            Err("跳转登录会话：请粘贴 Cookie/凭据后走 save 动作入库（不自动轮询）".to_string())
        };
        match secret {
            Ok(tok) if !tok.is_empty() => {
                accounts.add_account(&sess.platform, &format!("{}（设备授权）", sess.platform), "token", &tok);
                sess.status = "success".into();
                sess.message = "已拿到 token 并入账号池".into();
            }
            Ok(_) => {
                sess.message = "上游未返回凭据（多半还没完成授权），稍后重试".into();
            }
            Err(e) => {
                sess.message = e;
            }
        }
        if let Ok(mut g) = self.sessions.lock() {
            if let Some(s) = g.iter_mut().find(|s| s.id == sess.id) {
                *s = sess.clone();
            }
        }
        json!({
            "ok": true,
            "session": {"id": sess.id, "status": sess.status, "message": sess.message},
        })
    }

    /// 手动粘贴凭据落账号池（跳转登录平台的 save 闭环）。
    pub fn save(
        &self,
        platform: &str,
        secret: &str,
        accounts: &aigw_store::AccountStore,
    ) -> Value {
        if secret.trim().is_empty() {
            return json!({"ok": false, "message": "凭据为空，未入库"});
        }
        accounts.add_account(platform, &format!("{platform}（手动粘贴）"), "cookie", secret.trim());
        json!({"ok": true, "message": "凭据已保存到账号池（kind=cookie，可在账号页查看）"})
    }
}

impl Default for LoginEngine {
    fn default() -> Self {
        Self::new()
    }
}

async fn wb_start(client: &OutClient, base: &str, origin: &str) -> Result<(String, String), String> {
    let resp = client
        .http
        .post(format!("{base}/v2/plugin/auth/state?platform=CLI"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Origin", origin)
        .header("Referer", format!("{origin}/"))
        .header("User-Agent", CLI_UA)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}"));
    }
    let state = v
        .pointer("/data/state")
        .or_else(|| v.get("state"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if state.is_empty() {
        return Err("响应无 state".into());
    }
    let auth = v
        .pointer("/data/authUrl")
        .or_else(|| v.get("authUrl"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let url = if !auth.is_empty() {
        auth
    } else {
        format!("https://www.codebuddy.cn/login?platform=CLI&state={state}")
    };
    Ok((state, url))
}

async fn wb_poll_token(
    client: &OutClient,
    base: &str,
    origin: &str,
    state: &str,
) -> Result<String, String> {
    let resp = client
        .http
        .get(format!("{base}/v2/plugin/auth/token?state={state}"))
        .timeout(std::time::Duration::from_secs(15))
        .header("Origin", origin)
        .header("Referer", format!("{origin}/"))
        .header("User-Agent", CLI_UA)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    let tok = v
        .pointer("/data/accessToken")
        .or_else(|| v.get("accessToken"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if tok.is_empty() {
        return Err("等待授权中（登录完成后自动入库）".into());
    }
    Ok(tok)
}

/// MiniMax 设备码申请（PKCE S256，dsh 渠道实现一手）。
async fn minimax_start(
    http: &reqwest::Client,
) -> Result<(String, String, String, String), String> {
    let verifier = base62_48();
    let challenge = b64url_nopad(&Sha256::digest(verifier.as_bytes()));
    let resp = http
        .post("https://account.minimax.cn/oauth2/device/code")
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36",
        )
        .body(format!(
            "client_id=mcode-public&scope=agent.default&audience=agent-backend&code_challenge={challenge}&code_challenge_method=S256"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let device_code = v
        .get("device_code")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let verify_url = v
        .get("verification_uri")
        .or_else(|| v.get("verification_uri_complete"))
        .and_then(|x| x.as_str())
        .unwrap_or("https://platform.minimaxi.com/device")
        .to_string();
    if device_code.is_empty() || user_code.is_empty() {
        return Err(format!(
            "响应缺 device_code/user_code：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
    }
    Ok((device_code, user_code, verify_url, verifier))
}

/// MiniMax token 轮询（grant_type=device_code）。
async fn minimax_poll(http: &reqwest::Client, device_code: &str, verifier: &str) -> Result<String, String> {
    let resp = http
        .post("https://account.minimax.cn/oauth2/token")
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={device_code}&client_id=mcode-public&code_verifier={verifier}"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        if err == "authorization_pending" || err == "slow_down" {
            return Err("等待授权中（登录完成后自动入库）".into());
        }
        return Err(format!("oauth 错误：{err}"));
    }
    let tok = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if tok.is_empty() {
        return Err("响应无 access_token".into());
    }
    Ok(tok)
}

// ══════════════════════════════════════════════════════════════
// Kimi Code 设备码（RFC 8628，CLIProxyAPI v6 一手来源）
// ══════════════════════════════════════════════════════════════

/// 按平台解析 Kimi OAuth host：sub-kimi（国内）→ auth.kimi.com；sub-kimi-ai（国际）→ auth.kimi.ai。
fn kimi_oauth_host(platform: &str) -> Option<&'static str> {
    match platform {
        "sub-kimi" => Some(KIMI_OAUTH_COM),
        "sub-kimi-ai" => Some(KIMI_OAUTH_AI),
        _ => None,
    }
}

/// 生成 X-Msh-Device-Id（随机 32 hex；无 uuid crate，用 rand）。
fn kimi_device_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// X-Msh-* 公共头（CLIProxyAPI 兼容）。
fn kimi_headers(device_id: &str) -> Vec<(&'static str, String)> {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "aigw".into());
    let model = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    vec![
        ("X-Msh-Platform", "CLIProxyAPI".into()),
        ("X-Msh-Version", KIMI_MSH_VERSION.into()),
        ("X-Msh-Device-Name", host),
        ("X-Msh-Device-Model", model),
        ("X-Msh-Device-Id", device_id.to_string()),
    ]
}

/// 发起 Kimi 设备码流程 → (device_code, user_code, verification_uri)。
async fn kimi_start(
    http: &reqwest::Client,
    oauth_host: &str,
) -> Result<(String, String, String), String> {
    let device_id = kimi_device_id();
    let mut rb = http
        .post(format!("{oauth_host}/api/oauth/device_authorization"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={KIMI_CLIENT_ID}"));
    for (k, v) in kimi_headers(&device_id) {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}：{}", v.to_string().chars().take(150).collect::<String>()));
    }
    let device_code = v
        .get("device_code")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let user_code = v
        .get("user_code")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let verify = v
        .get("verification_uri_complete")
        .or_else(|| v.get("verification_uri"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if device_code.is_empty() || user_code.is_empty() {
        return Err(format!(
            "响应缺 device_code/user_code：{}",
            v.to_string().chars().take(200).collect::<String>()
        ));
    }
    Ok((device_code, user_code, verify))
}

/// Kimi token 轮询（authorization_pending → 继续等待）。
async fn kimi_poll(
    http: &reqwest::Client,
    oauth_host: &str,
    device_code: &str,
) -> Result<String, String> {
    let device_id = kimi_device_id();
    let mut rb = http
        .post(format!("{oauth_host}/api/oauth/token"))
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={KIMI_CLIENT_ID}&device_code={device_code}&grant_type=urn:ietf:params:oauth:grant-type:device_code"
        ));
    for (k, v) in kimi_headers(&device_id) {
        rb = rb.header(k, v);
    }
    let resp = rb.send().await.map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        match err {
            "authorization_pending" | "slow_down" => {
                return Err("等待授权中（打开验证页输入用户码，面板自动入库）".into())
            }
            "expired_token" => return Err("设备码已过期，请重新发起登录".into()),
            "access_denied" => return Err("授权被拒绝".into()),
            _ => return Err(format!("oauth 错误：{err}")),
        }
    }
    let tok = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if tok.is_empty() {
        return Err("响应无 access_token".into());
    }
    Ok(tok)
}

/// 占位引用（ProviderSpec 导入避免 unused；后续 spec 化登录页复用）。
#[allow(dead_code)]
fn _touch(_s: &ProviderSpec) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wb_start_and_poll_via_mock() {
        // mock：state 签发 + token 轮询
        use axum::routing::{get, post};
        let app = axum::Router::new()
            .route(
                "/v2/plugin/auth/state",
                post(|| async { axum::Json(serde_json::json!({"data":{"state":"st-1"}})) }),
            )
            .route(
                "/v2/plugin/auth/token",
                get(|| async { axum::Json(serde_json::json!({"data":{"accessToken":"tok-9"}})) }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });

        // 用 mock 地址覆盖 wb_base：LoginEngine 的 base 是常量映射——测试走 engine+mock 不好注入。
        // 改为直接测 wb_start/wb_poll_token 函数（带 base 参数，可注入 mock）。
        let client = OutClient::new();
        let (state, url) = wb_start(&client, &format!("http://{addr}"), &format!("http://{addr}"))
            .await
            .unwrap();
        assert_eq!(state, "st-1");
        assert!(url.contains("state=st-1") || url.starts_with("http://"), "url={url}");
        let tok = wb_poll_token(&client, &format!("http://{addr}"), &format!("http://{addr}"), &state)
            .await
            .unwrap();
        assert_eq!(tok, "tok-9");
    }

    #[test]
    fn base62_and_pkce_shape() {
        let v = base62_48();
        assert_eq!(v.len(), 48);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric()));
        let d = b64url_nopad(&Sha256::digest(v.as_bytes()));
        assert_eq!(d.len(), 43, "S256 challenge 应为 43 字符 urlsafe");
    }
}
