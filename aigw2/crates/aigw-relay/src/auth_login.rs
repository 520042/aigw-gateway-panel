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
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const CLI_UA: &str = "CLI/2.63.2 CodeBuddy/2.63.2";

/// 登录专用 HTTP Client：每次按当前代理配置动态构建（热更新即时生效）。
/// 有代理（global/bypass_cn）则注入，否则直连。
pub fn auth_http() -> reqwest::Client {
    let mut b = reqwest::Client::builder();
    if let Some(p) = crate::proxy::proxy_for_current() {
        b = b.proxy(p);
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Kimi Code OAuth（CLIProxyAPI v6 一手来源）。
const KIMI_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_OAUTH_COM: &str = "https://auth.kimi.com";
const KIMI_OAUTH_AI: &str = "https://auth.kimi.ai";
const KIMI_MSH_VERSION: &str = "v6.10.9";

/// xAI / Grok OAuth（CLIProxyAPI v6 internal/auth/xai 一手来源）。
const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DISCOVERY: &str = "https://auth.x.ai/.well-known/openid-configuration";

/// OpenAI Codex OAuth（CLIProxyAPI v6 internal/auth/codex 一手来源）：PKCE 授权码 + 本地 1455 回调。
const CODEX_AUTH_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_REDIRECT: &str = "http://localhost:1455/auth/callback";

/// Anthropic Claude Code OAuth（CLIProxyAPI v6 internal/auth/claude 一手来源）：PKCE 授权码 + 本地 54545 回调 + JSON 换码。
const CLAUDE_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_REDIRECT: &str = "http://localhost:54545/callback";
const CLAUDE_SCOPE: &str = "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

/// 网易 Lobster AI（有道龙虾）OAuth（本地 LobsterAI2API/ 源码一手来源）：
/// 本地随机端口回调 → 门户授权 → exchange 换码（跳转登录，非扫码）。
const LOBSTER_UPSTREAM: &str = "https://lobsterai-server.youdao.com";
const LOBSTER_LOGIN: &str = "https://lobsterai.youdao.com";

// ── Cline WorkOS 设备码（一手：cline2api auth.go:16-21，_refs\luawei1-cline2api）──
const WORKOS_CLIENT_ID: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
const WORKOS_DEVICE_URL: &str = "https://api.workos.com/user_management/authorize/device";
const WORKOS_AUTH_URL: &str = "https://api.workos.com/user_management/authenticate";
const CLINE_API_BASE: &str = "https://api.cline.bot/api/v1";

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
            match minimax_start(&auth_http()).await {
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
        } else if platform == "cline" {
            // Cline WorkOS 设备码（22 cline2api auth.go 一手）
            match cline_start(&auth_http()).await {
                Ok((device_code, user_code, verify_url, interval)) => {
                    sess.state = format!("cline:{device_code}:{interval}");
                    sess.login_url = verify_url.clone();
                    sess.user_code = user_code.clone();
                    sess.message = format!(
                        "Cline 设备码：打开验证页（{}）输入 {} 完成授权，面板自动换发 token 入库。",
                        verify_url, user_code
                    );
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("WorkOS 设备码申请失败：{e}");
                }
            }
        } else if let Some(oauth_host) = kimi_oauth_host(platform) {
            // Kimi Code 设备码（RFC 8628）：sub-kimi 国内站 / sub-kimi-ai 国际站
            match kimi_start(&auth_http(), oauth_host).await {
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
        } else if platform == "sub-grok" {
            // xAI Grok 设备码（RFC 8628 + OIDC discovery）
            match xai_start(&auth_http()).await {
                Ok((device_code, user_code, verify_url, tok_ep)) => {
                    sess.state = format!("xai:{device_code}:{tok_ep}");
                    sess.login_url = verify_url.clone();
                    sess.user_code = user_code.clone();
                    sess.message = format!(
                        "Grok 设备码：打开验证页（{}）输入 {} 完成授权，面板自动拿到 token 入库。",
                        verify_url, user_code
                    );
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("Grok 设备码申请失败：{e}（可回退手动粘贴 token）");
                }
            }
        } else if platform == "sub-codex" {
            // OpenAI Codex：PKCE 授权码 + 本地 1455 回调（跳转登录，非扫码）
            match codex_start().await {
                Ok((url, verifier, state)) => {
                    sess.state = format!("codex:{verifier}:{state}");
                    sess.login_url = url.clone();
                    sess.message = "Codex 跳转登录：点「打开授权页」在浏览器完成登录，面板自动捕获回调换 token 入库；若面板部署在服务器，把浏览器跳转后地址栏的 URL 粘贴到下方输入框。".into();
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("Codex 授权 URL 生成失败：{e}");
                }
            }
        } else if platform == "sub-claude" {
            // Anthropic Claude Code：PKCE 授权码 + 本地 54545 回调（跳转登录，非扫码）
            match claude_start().await {
                Ok((url, verifier, state)) => {
                    sess.state = format!("claude:{verifier}:{state}");
                    sess.login_url = url.clone();
                    sess.message = "Claude 跳转登录：点「打开授权页」在浏览器完成登录，面板自动捕获回调换 token 入库；若面板部署在服务器，把浏览器跳转后地址栏的 URL 粘贴到下方输入框。".into();
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("Claude 授权 URL 生成失败：{e}");
                }
            }
        } else if platform == "lobster" {
            // 网易 Lobster AI（有道龙虾）：本地随机端口回调 + exchange 换码（跳转登录，非扫码）
            match lobster_start().await {
                Ok((url, state, uuid, first_kf)) => {
                    sess.state = format!("lobster:{state}:{uuid}:{first_kf}");
                    sess.login_url = url.clone();
                    sess.message = "Lobster 跳转登录：点「打开授权页」在浏览器完成登录，面板自动捕获回调换 token 入库；若面板部署在服务器，把浏览器跳转后地址栏的 URL 粘贴到下方输入框。".into();
                }
                Err(e) => {
                    sess.status = "error".into();
                    sess.message = format!("Lobster 授权 URL 生成失败：{e}");
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
                minimax_poll(&auth_http(), parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("kimi:") {
            let device_code = sess.state.trim_start_matches("kimi:").to_string();
            match kimi_oauth_host(&sess.platform) {
                Some(host) => kimi_poll(&auth_http(), host, &device_code).await,
                None => Err("Kimi 平台映射缺失".to_string()),
            }
        } else if sess.state.starts_with("cline:") {
            // cline:{device_code}:{interval} → WorkOS 三态轮询 → register 换皮
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("Cline 会话不完整".to_string())
            } else {
                cline_poll(&auth_http(), parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("xai:") {
            // xai:{device_code}:{token_endpoint}
            let rest = sess.state.trim_start_matches("xai:").to_string();
            match rest.split_once(':') {
                Some((d, t)) => xai_poll(&auth_http(), t, d).await,
                None => Err("Grok 设备码会话不完整".to_string()),
            }
        } else if sess.state.starts_with("codex:") || sess.state.starts_with("claude:") {
            // {prefix}:{verifier}:{state} → 从回调队列取 code → 换 token
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("PKCE 会话不完整".to_string())
            } else {
            let (kind, verifier, state) = (parts[0], parts[1], parts[2]);
            let code = AUTH_CALLBACKS
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _)| s == state)
                .map(|(_, c)| c.clone());
            match code {
                Some(code) => {
                    AUTH_CALLBACKS.lock().unwrap().retain(|(s, _)| s != state);
                    if kind == "codex" {
                        codex_exchange(&auth_http(), &code, verifier).await
                    } else {
                        claude_exchange(&auth_http(), &code, verifier, state).await
                    }
                }
                None => Err("等待授权中（在打开的授权页完成登录，面板自动捕获回调；若未自动捕获，请把浏览器地址栏 URL 粘贴到下方输入框后点「提交回调」）".to_string()),
            }
            }
        } else if sess.state.starts_with("lobster:") {
            // lobster:{state}:{uuid}:{firstKF} → 回调队列取 code → exchange
            let parts: Vec<&str> = sess.state.splitn(4, ':').collect();
            if parts.len() < 4 {
                Err("Lobster 会话不完整".to_string())
            } else {
                let state = parts[1];
                let code = AUTH_CALLBACKS
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(s, _)| s == state)
                    .map(|(_, c)| c.clone());
                match code {
                    Some(code) => {
                        AUTH_CALLBACKS.lock().unwrap().retain(|(s, _)| s != state);
                        lobster_exchange(&auth_http(), &code, parts[2], parts[3]).await
                    }
                    None => Err("等待授权中（在打开的授权页完成登录，面板自动捕获回调；若未自动捕获，请把浏览器地址栏 URL 粘贴到下方输入框后点「提交回调」）".to_string()),
                }
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

/// 生成 PKCE S256 对（RFC 7636）：verifier 96 随机字节 base64url 无填充；challenge=SHA256(verifier)。
fn pkce_pair() -> (String, String) {
    use rand::RngCore;
    let mut b = [0u8; 96];
    rand::thread_rng().fill_bytes(&mut b);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    (verifier, challenge)
}

/// 生成随机 state（OAuth CSRF 防伪）。
fn oauth_state() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ══════════════════════════════════════════════════════════════
// xAI / Grok 设备码（RFC 8628 + OIDC discovery）
// ══════════════════════════════════════════════════════════════

/// 解析 xAI OIDC discovery → (device_authorization_endpoint, token_endpoint)。
async fn xai_discovery(http: &reqwest::Client) -> Result<(String, String), String> {
    let resp = http
        .get(XAI_DISCOVERY)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    let dev = v
        .get("device_authorization_endpoint")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let tok = v
        .get("token_endpoint")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if dev.is_empty() || tok.is_empty() {
        return Err("xAI discovery 缺端点".into());
    }
    Ok((dev, tok))
}

/// 发起 Grok 设备码 → (device_code, user_code, verification_uri, token_endpoint)。
async fn xai_start(http: &reqwest::Client) -> Result<(String, String, String, String), String> {
    let (dev_ep, tok_ep) = xai_discovery(http).await?;
    let resp = http
        .post(dev_ep)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={XAI_CLIENT_ID}&scope={}", urlencode(XAI_SCOPE)))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
    }
    let device_code = v.get("device_code").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let user_code = v.get("user_code").and_then(|x| x.as_str()).unwrap_or("").to_string();
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
    Ok((device_code, user_code, verify, tok_ep))
}

/// Grok token 轮询。
async fn xai_poll(
    http: &reqwest::Client,
    token_endpoint: &str,
    device_code: &str,
) -> Result<String, String> {
    let resp = http
        .post(token_endpoint)
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={device_code}&client_id={XAI_CLIENT_ID}"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        match err {
            "authorization_pending" => {
                return Err("等待授权中（打开验证页输入用户码，面板自动入库）".into())
            }
            "slow_down" => return Err("等待授权中（请稍候再查）".into()),
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

// ══════════════════════════════════════════════════════════════
// OpenAI Codex / Anthropic Claude：PKCE 授权码 + 本地回调
// ══════════════════════════════════════════════════════════════

/// 生成 Codex 授权 URL（PKCE S256，OpenAI 账号）。
async fn codex_start() -> Result<(String, String, String), String> {
    let (verifier, challenge) = pkce_pair();
    let state = oauth_state();
    let url = format!(
        "{CODEX_AUTH_URL}?client_id={CODEX_CLIENT_ID}&response_type=code&redirect_uri={}&scope=openid+email+profile+offline_access&state={state}&code_challenge={challenge}&code_challenge_method=S256&prompt=login&id_token_add_organizations=true&codex_cli_simplified_flow=true",
        urlencode(CODEX_REDIRECT)
    );
    Ok((url, verifier, state))
}

/// 用授权码换 Codex token。
async fn codex_exchange(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let resp = http
        .post(CODEX_TOKEN_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&client_id={CODEX_CLIENT_ID}&code={code}&redirect_uri={}&code_verifier={verifier}",
            urlencode(CODEX_REDIRECT)
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
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

/// 生成 Claude Code 授权 URL（PKCE S256，Anthropic 账号）。
async fn claude_start() -> Result<(String, String, String), String> {
    let (verifier, challenge) = pkce_pair();
    let state = oauth_state();
    let url = format!(
        "{CLAUDE_AUTH_URL}?code=true&client_id={CLAUDE_CLIENT_ID}&redirect_uri={}&response_type=code&scope={}&state={state}&code_challenge={challenge}&code_challenge_method=S256",
        urlencode(CLAUDE_REDIRECT),
        urlencode(CLAUDE_SCOPE)
    );
    Ok((url, verifier, state))
}

/// 用授权码换 Claude token（JSON body，Claude Code 2.1.220 实测字段序）。
async fn claude_exchange(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
    state: &str,
) -> Result<String, String> {
    let resp = http
        .post(CLAUDE_TOKEN_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("User-Agent", "axios/1.15.2")
        .body(json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": CLAUDE_REDIRECT,
            "client_id": CLAUDE_CLIENT_ID,
            "code_verifier": verifier,
            "state": state,
        }).to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
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

/// 简易 URL 编码（OAuth 参数值内空格/特殊字符）。
fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => {
                let mut b = [0u8; 4];
                c.encode_utf8(&mut b);
                b[..c.len_utf8()]
                    .iter()
                    .map(|x| format!("%{x:02X}"))
                    .collect()
            }
        })
        .collect()
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

/// Cline WorkOS 设备码申请（22 cline2api auth.go:352-414 一手）。
async fn cline_start(http: &reqwest::Client) -> Result<(String, String, String, u64), String> {
    let resp = http
        .post(WORKOS_DEVICE_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("client_id={WORKOS_CLIENT_ID}"))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
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
    let interval = v
        .get("interval")
        .and_then(|x| x.as_u64())
        .unwrap_or(5)
        .max(5); // 最小 5s（cline2api auth.go 注释）
    if device_code.is_empty() || user_code.is_empty() {
        return Err(format!(
            "响应缺 device_code/user_code：{}",
            v.to_string().chars().take(200).collect::<String>()
        ));
    }
    Ok((device_code, user_code, verify, interval))
}

/// Cline register 换皮（auth.go:263-267 注释：refresh token 本质即 WorkOS refresh token）。
/// 返回 (cline_access_token, cline_refresh_token)。
async fn cline_register(
    http: &reqwest::Client,
    wos_at: &str,
    wos_rt: &str,
) -> Result<(String, String), String> {
    let rr = http
        .post(format!("{CLINE_API_BASE}/auth/register"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(format!(
            r#"{{"accessToken":"{wos_at}","refreshToken":"{wos_rt}"}}"#
        ))
        .send()
        .await
        .map_err(|e| format!("register 请求失败：{e}"))?;
    let rstatus = rr.status().as_u16();
    let rv: Value = rr.json().await.map_err(|e| e.to_string())?;
    if !(200..300).contains(&rstatus) {
        return Err(format!(
            "register HTTP {rstatus}：{}",
            rv.to_string().chars().take(200).collect::<String>()
        ));
    }
    let cat = rv
        .get("accessToken")
        .or_else(|| rv.get("access_token"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let crt = rv
        .get("refreshToken")
        .or_else(|| rv.get("refresh_token"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| wos_rt.to_string());
    if cat.is_empty() {
        return Err(format!(
            "register 响应缺 accessToken：{}",
            rv.to_string().chars().take(200).collect::<String>()
        ));
    }
    Ok((cat, crt))
}

/// Cline 刷新双通道自愈（22 cline2api pool.go:358-388 一手）：
/// 先 `POST /auth/refresh`；400/401/403 才算拒绝（网络/5xx 只算 transient），
/// 拒绝后走 `refreshViaWorkOS`（authenticate grant_type=refresh_token）→ 再 register 换皮。
/// 输入/输出 secret = `workos:{at}:{rt}`；双通道全拒 → Err（需重新 OAuth）。
pub async fn cline_refresh(http: &reqwest::Client, secret: &str) -> Result<String, String> {
    let parts: Vec<&str> = secret.split(':').collect();
    if parts.len() < 3 {
        return Err("secret 无 refresh_token（需重新登录）".into());
    }
    let rt = parts[2].to_string();
    // 1) Cline /auth/refresh
    let rr = http
        .post(format!("{CLINE_API_BASE}/auth/refresh"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body(format!(
            r#"{{"refreshToken":"{rt}","grantType":"refresh_token"}}"#
        ))
        .send()
        .await
        .map_err(|e| format!("refresh 请求失败：{e}"))?;
    let rstatus = rr.status().as_u16();
    let rv: Value = rr.json().await.unwrap_or(Value::Null);
    if (200..300).contains(&rstatus) {
        let at = rv
            .get("accessToken")
            .or_else(|| rv.get("access_token"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !at.is_empty() {
            let new_rt = rv
                .get("refreshToken")
                .or_else(|| rv.get("refresh_token"))
                .and_then(|x| x.as_str())
                .unwrap_or(&rt);
            return Ok(format!("workos:{at}:{new_rt}"));
        }
    }
    // 2) 被拒（400/401/403）→ WorkOS 直刷；网络/5xx 算 transient 不误判 expired
    if rstatus == 400 || rstatus == 401 || rstatus == 403 {
        let w = http
            .post(WORKOS_AUTH_URL)
            .timeout(std::time::Duration::from_secs(20))
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&client_id={WORKOS_CLIENT_ID}&refresh_token={rt}"
            ))
            .send()
            .await
            .map_err(|e| format!("WorkOS 刷新请求失败：{e}"))?;
        let wstatus = w.status().as_u16();
        let wv: Value = w.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&wstatus) {
            return Err(format!(
                "WorkOS 刷新被拒（HTTP {wstatus}）：{}——请重新发起登录",
                wv.to_string().chars().take(150).collect::<String>()
            ));
        }
        let wos_at = wv
            .get("access_token")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let wos_rt = wv
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .unwrap_or(&rt)
            .to_string();
        if wos_at.is_empty() {
            return Err("WorkOS 刷新响应缺 access_token".into());
        }
        // 3) 回 register 换正式 Cline 令牌
        let (cat, crt) = cline_register(http, &wos_at, &wos_rt).await?;
        return Ok(format!("workos:{cat}:{crt}"));
    }
    Err(format!(
        "Cline refresh 上游异常（HTTP {rstatus}，transient，稍后自动重试）"
    ))
}

/// Cline WorkOS 轮询 + register 换皮（22 cline2api auth.go:263-267, pool.go:358-388 一手）。
/// 返回账号 secret = `workos:{cline_at}:{cline_rt}`（relay 转发取 `workos:{at}` 作 Bearer）。
async fn cline_poll(
    http: &reqwest::Client,
    device_code: &str,
    interval: &str,
) -> Result<String, String> {
    // 1) WorkOS 设备码轮询
    let resp = http
        .post(WORKOS_AUTH_URL)
        .timeout(std::time::Duration::from_secs(15))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=urn:ietf:params:oauth:grant-type:device_code&device_code={device_code}&client_id={WORKOS_CLIENT_ID}"
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error").and_then(|x| x.as_str()) {
        match err {
            "authorization_pending" | "slow_down" => {
                return Err(format!(
                    "等待授权中（间隔 {interval}s 轮询，打开验证页输入用户码，面板自动换发 token）"
                ))
            }
            "expired_token" => return Err("设备码已过期，请重新发起登录".into()),
            "access_denied" => return Err("授权被拒绝".into()),
            _ => return Err(format!("WorkOS 错误：{err}")),
        }
    }
    let wos_at = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let wos_rt = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if wos_at.is_empty() || wos_rt.is_empty() {
        return Err("WorkOS 响应缺 token".into());
    }
    // 2) Cline register 换皮（复用 cline_register）
    let (cat, crt) = cline_register(http, &wos_at, &wos_rt).await?;
    Ok(format!("workos:{cat}:{crt}"))
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

    #[test]
    fn pkce_pair_shape() {
        let (v, c) = pkce_pair();
        assert!(v.len() >= 43 && v.len() <= 128);
        assert_eq!(c.len(), 43, "S256 challenge base64url 43 字符");
        // 验证 challenge = SHA256(verifier) base64url 无填充
        let expect = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(v.as_bytes()));
        assert_eq!(c, expect);
        assert_eq!(oauth_state().len(), 32);
    }
}

/// ══════════════════════════════════════════════════════════════
/// 网易 Lobster AI（有道龙虾）OAuth：本地随机端口回调 + exchange 换码
/// ══════════════════════════════════════════════════════════════

/// 发起 Lobster 登录：动态绑定随机端口监听回调，返回 (登录URL, state, uuid, firstKeyfrom)。
async fn lobster_start() -> Result<(String, String, String, String), String> {
    let state = oauth_state();
    let uuid = new_uuid();
    let first_kf = now_millis();
    let port = spawn_lobster_callback().await?;
    let redirect = format!("http://127.0.0.1:{port}/auth/callback");
    let login = format!(
        "{}/portal#/login?source=electron&redirect_uri={}&state={}",
        LOBSTER_LOGIN,
        urlencode(&redirect),
        state
    );
    Ok((login, state, uuid, first_kf))
}

/// 动态回调监听：绑定 127.0.0.1:0 随机端口，解析 ?code=&state= 入队，返回端口。
async fn spawn_lobster_callback() -> Result<u16, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("回调端口绑定失败：{e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("取端口失败：{e}"))?
        .port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => continue,
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 4096];
                let n = match sock.read(&mut buf).await {
                    Ok(n) => n,
                    Err(_) => return,
                };
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/");
                let query = match path.split_once('?') {
                    Some((_, q)) => q,
                    None => "",
                };
                let mut code = String::new();
                let mut state = String::new();
                for kv in query.split('&') {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    match k {
                        "code" => code = v.to_string(),
                        "state" => state = v.to_string(),
                        _ => {}
                    }
                }
                if !code.is_empty() && !state.is_empty() {
                    AUTH_CALLBACKS.lock().unwrap().push((state, code));
                }
                let body = "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权成功 ✓</h2><p>可以关闭此页，返回面板查看账号状态。</p></body></html>";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    Ok(port)
}

/// 用 authCode 换 token（POST {upstream}/api/auth/exchange，authCode/uuid/keyfrom/version）。
async fn lobster_exchange(
    http: &reqwest::Client,
    code: &str,
    uuid: &str,
    first_kf: &str,
) -> Result<String, String> {
    let body = serde_json::json!({
        "authCode": code,
        "firstKeyfrom": first_kf,
        "latestKeyfrom": now_millis(),
        "uuid": uuid,
        "version": "0.1.0",
    });
    let resp = http
        .post(format!("{LOBSTER_UPSTREAM}/api/auth/exchange"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "LobsterAI/0.1.0")
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("exchange 请求失败：{e}"))?;
    let st = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;
    if !st.is_success() {
        return Err(format!("upstream {st}: {}", truncate(&raw, 160)));
    }
    let v: Value = serde_json::from_str(&raw).map_err(|_| format!("响应解析失败：{}", truncate(&raw, 160)))?;
    if v.get("code").and_then(|c| c.as_i64()) != Some(0) && v.get("accessToken").is_none() {
        let m = v
            .get("message")
            .or_else(|| v.get("msg"))
            .and_then(|x| x.as_str())
            .unwrap_or("未知错误");
        return Err(format!("exchange code={}: {m}", v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1)));
    }
    // token 可能在 data 里，也可能直接在顶层（两种都兼容）
    let tok = v
        .get("accessToken")
        .and_then(|x| x.as_str())
        .or_else(|| {
            v.get("data")
                .and_then(|d| d.get("accessToken"))
                .and_then(|x| x.as_str())
        })
        .map(|s| s.to_string());
    match tok {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err(format!("响应中无 accessToken：{}", truncate(&raw, 160))),
    }
}

fn new_uuid() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn now_millis() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".into())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t = String::new();
        for ch in s.chars() {
            if t.chars().count() >= n {
                break;
            }
            t.push(ch);
        }
        format!("{t}…")
    }
}

/// ══════════════════════════════════════════════════════════════
/// OAuth 本地回调捕获（Codex 1455 / Claude 54545）
/// ══════════════════════════════════════════════════════════════

/// 回调队列：(state, code)。面板监听端口收到回调即入队；poll 匹配 state 换 token。
/// 服务器场景用户粘贴回调 URL 也走此队列。
pub static AUTH_CALLBACKS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// 启动 OAuth 本地回调监听（127.0.0.1:1455=Codex、127.0.0.1:54545=Claude）。
/// 绑定失败仅告警（登录可用「粘贴回调 URL」兜底）。
pub fn start_callback_listeners() {
    for port in [1455u16, 54545u16] {
        let bind = format!("127.0.0.1:{port}");
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&bind).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("OAuth 回调监听 {bind} 绑定失败：{e}（登录可用粘贴回调 URL 兜底）");
                    return;
                }
            };
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 4096];
                    let n = match sock.read(&mut buf).await {
                        Ok(n) => n,
                        Err(_) => return,
                    };
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/");
                    let query = match path.split_once('?') {
                        Some((_, q)) => q,
                        None => "",
                    };
                    let mut code = String::new();
                    let mut state = String::new();
                    for kv in query.split('&') {
                        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                        match k {
                            "code" => code = v.to_string(),
                            "state" => state = v.to_string(),
                            _ => {}
                        }
                    }
                    if !code.is_empty() && !state.is_empty() {
                        AUTH_CALLBACKS.lock().unwrap().push((state, code));
                    }
                    let body = "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权成功 ✓</h2><p>可以关闭此页，返回面板查看账号状态。</p></body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
    }
}

/// 用户粘贴回调 URL（服务器场景）→ 提取 code/state 入队。
pub fn submit_callback_url(url: &str) -> Result<(), String> {
    let raw = url.trim();
    if raw.is_empty() {
        return Err("回调地址为空".into());
    }
    let (_, query) = match raw.split_once('?') {
        Some((_, q)) => (raw, q),
        None => return Err("回调地址缺少 ?code=&state= 参数".into()),
    };
    let mut code = String::new();
    let mut state = String::new();
    for kv in query.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        match k {
            "code" => code = v.to_string(),
            "state" => state = v.to_string(),
            _ => {}
        }
    }
    if code.is_empty() || state.is_empty() {
        return Err("回调地址缺少 code 或 state 参数".into());
    }
    AUTH_CALLBACKS.lock().unwrap().push((state, code));
    Ok(())
}
