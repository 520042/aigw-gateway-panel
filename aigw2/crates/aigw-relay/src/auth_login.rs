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

/// 平台 id → 面板显示名。只为「账号自动命名」服务：登录链路拿不到 ProviderSpec
/// （它只有 platform id），而账号名要给人看，不能写成 `wb-gateway（设备授权）`。
/// 认不出就返回 None，调用方退回用 id 命名。
fn display_name_of(platform: &str) -> Option<&'static str> {
    Some(match platform {
        "wb-gateway" => "WorkBuddy 国内站",
        "wb-gateway-intl" => "WorkBuddy 国际站",
        "codebuddy" => "CodeBuddy",
        "apk-doubao" => "豆包",
        "apk-kuku" => "库库",
        "apk-loomy" => "Loomy",
        "apk-raccoon" => "小浣熊",
        "apk-yuanbao" => "腾讯元宝",
        "apk-wps" => "WPS AI",
        "apk-metaso" => "秘塔搜索",
        "lobster" => "网易 Lobster",
        "kimi-code" => "Kimi Code",
        "cline" => "Cline",
        "codex" => "Codex",
        "minimax" => "MiniMax",
        "qoder" => "Qoder",
        "trae" => "Trae",
        "devin" => "Devin",
        "antigravity" => "Antigravity",
        _ => return None,
    })
}
/// 「手动粘贴」这条路的账号名：与登录链路同一套可读命名（平台显示名 · 手填凭据）。
#[allow(dead_code)]
fn pasted_account_name(platform: &str) -> String {
    match display_name_of(platform) {
        Some(n) => format!("{n} · 手填凭据"),
        None => format!("{platform} · 手填凭据"),
    }
}


/// 登录/外部接口专用 HTTP Client：每次按当前代理配置动态构建（热更新即时生效）。
/// 有代理（global/bypass_cn）则注入，否则直连。
///
/// 客户端级默认超时（connect 10s / read 30s）只是**兜底**：reqwest 默认无超时，
/// 一旦新增调用点忘了写 `.timeout(...)`，请求会无限挂住（登录流程卡死、tokio worker 泄漏）。
/// 现有调用点都自带 `.timeout()`，按请求设置优先于这里的默认值，不受影响。
pub fn auth_http() -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(30));
    if let Some(p) = crate::proxy::proxy_for_current() {
        b = b.proxy(p);
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 扫码**长轮询**专用 Client：只有 connect 超时，**不设 read_timeout**。
///
/// auth_http() 的 read_timeout(30s) 会把「上游长轮询期间一个字节都不发」误判成故障：
/// 百度 passport 的 /channel/unicast 在未扫码时会挂起约 30 秒才回 {"errno":1}
/// （2026-10-21 实测两次 29,985ms / 29,970ms），30s read_timeout 正好卡在它回包前后，
/// 于是轮询随机报 "error sending request"。长轮询必须绕开这个兜底，只靠请求级 .timeout()。
pub fn auth_http_long() -> reqwest::Client {
    let mut b = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
    if let Some(p) = crate::proxy::proxy_for_current() {
        b = b.proxy(p);
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// reqwest 的 Display 会把整条 URL（含 channel_id / callback / sign）拼进错误串，
/// 既泄漏会话参数又淹没真正原因 → 只保留分类。
fn brief_net_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "超时".to_string()
    } else if e.is_connect() {
        "连接失败（DNS / 代理 / 被墙）".to_string()
    } else if e.is_decode() {
        "响应解码失败".to_string()
    } else if e.is_body() {
        "响应体中断".to_string()
    } else if e.is_redirect() {
        "重定向异常".to_string()
    } else if e.is_request() {
        "请求未发出".to_string()
    } else {
        "网络错误".to_string()
    }
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

/// Antigravity/Google OAuth（cliproxy-src internal/auth/antigravity/constants.go 一手）：
/// Google 授权码 PKCE + 本地 51121 回调。
const ANTI_CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTI_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTI_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTI_REDIRECT: &str = "http://localhost:51121/callback";
const ANTI_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
// 上游 API 端点（Antigravity 转发用）：https://cloudcode-pa.googleapis.com（daily- 域 daily-cloudcode-pa.googleapis.com）

/// Meta Muse 设备码（cliproxy-src internal/auth/meta/meta.go 一手）：RFC 8628 + mint API Key。
const META_CLIENT_ID: &str = "1031625952748946";
const META_DEVICE_URL: &str = "https://auth.meta.com/oidc/device/authorization/";
const META_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";
const META_MINT_URL: &str = "https://api.meta.ai/muse-code/key";
const META_DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const META_UA: &str = "muse-code/1.0.2";

// ═════════ 华为云 CodeArts（01 DSH login.ts 一手：ticket 流程，免 DPoP/PKCE） ═════════
const CODEARTS_LOGIN_BASE: &str = "https://devcloud.cn-north-4.huaweicloud.com/doer/redirect";
const HUAWEI_AUTH_BASE: &str = "https://auth.huaweicloud.com/authui/login.html";
const CODEARTS_CRED_ENDPOINT: &str =
    "https://snap-access.cn-north-4.myhuaweicloud.com/snap-manager/v1/login/ticket";
const CODEARTS_CALLBACK: &str = "http://127.0.0.1:51122/authentication";
const CODEARTS_PLUGIN: &str = "snap_jetbrains";
const CODEARTS_PLUGIN_VER: &str = "26.3.3";

// ═════════ Qoder 阿里（16 qoder2api-hub 一手：设备授权 + PKCE，国际版） ═════════
const QODER_WEBSITE: &str = "https://qoder.com";
const QODER_OPENAPI: &str = "https://openapi.qoder.sh";
const QODER_CLIENT_ID: &str = "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb";

// ═════════ Trae 字节（19 trae2api 一手：跳转回调 18080 + ExchangeToken） ═════════
const TRAE_CONSOLE: &str = "https://www.trae.cn";
const TRAE_OAUTH_HOST: &str = "https://api.trae.com.cn";
const TRAE_CLIENT_ID: &str = "en1oxy7wnw8j9n";
const TRAE_APP_ID: &str = "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8";
const TRAE_CALLBACK: &str = "http://127.0.0.1:18080/authorize";
const TRAE_IDE_VERSION: &str = "0.1.52";

/// Devin headless PKCE（cliproxy-src internal/auth/devin/devin_auth.go 一手）：
/// 官方 CLI 无 redirect_uri + cli_pkce_marker=1（服务器场景直接粘贴地址栏 URL）。
const DEVIN_APP_BASE: &str = "https://app.devin.ai";
const DEVIN_API_BASE: &str = "https://api.devin.ai";

/// 网易 Lobster AI（有道龙虾）OAuth（本地 LobsterAI2API/ 源码一手来源）：
/// 本地随机端口回调 → 门户授权 → exchange 换码（跳转登录，非扫码）。
const LOBSTER_UPSTREAM: &str = "https://lobsterai-server.youdao.com";
const LOBSTER_LOGIN: &str = "https://lobsterai.youdao.com";

// ═════════ 豆包网页版扫码登录（doubao2api qr_login.py 一手：passport CSRF + 二维码 + check_qrconnect 轮询） ═════════
const DOUBAO_BASE_URL: &str = "https://www.doubao.com";
const DOUBAO_AID: &str = "497858";
const DOUBAO_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
// ═════════ 库库 AI 扫码登录（kuku2api passport.mjs 一手：百度 passport 扫码 + BDUSS/PTOKEN + STOKEN 交换） ═════════
const KUKU_PASSPORT_BASE: &str = "https://passport.baidu.com";
const KUKU_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) GenFlowPro/1.6.5.130 Chrome/120.0.0.0 Electron/28.0.0.0 Safari/537.36";
const KUKU_REFERER: &str = "https://kuku.baidu.com/genflowpro/login";
const KUKU_TPL: &str = "genflowpro";
const KUKU_ENGINE_UA: &str = "genflow;1.6.5;PC;PC-Windows;10.0.26200;GenFlowPro";
const KUKU_ENGINE_SIGN_KEY: &str = "34868b316de55815273b2616954b0286";
const KUKU_U: &str = "https://kuku.baidu.com";

// ═════════ 小浣熊（商汤）微信小程序扫码（dsh-our-free-model raccoon.ts 一手：自造 code + login_with_qrcode_code 轮询） ═════════
const RACCOON_BASE: &str = "https://xiaohuanxiong.com";
const RACCOON_UA: &str = "Raccoon Work/1.0.35 (Windows)";
const RACCOON_CLIENT_PLATFORM: &str = "desktop-windows";
const RACCOON_CLIENT_VERSION: &str = "v1.0.35";
/// 小程序码里的 appname（「商汤小浣熊官网」，已 percent-encode）。
const RACCOON_QR_APPNAME: &str =
    "%E5%95%86%E6%B1%A4%E5%B0%8F%E6%B5%A3%E7%86%8A%E5%AE%98%E7%BD%91";

// ── Cline WorkOS 设备码（一手：cline2api auth.go:16-21，_refs\luawei1-cline2api）──
const WORKOS_CLIENT_ID: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
const WORKOS_DEVICE_URL: &str = "https://api.workos.com/user_management/authorize/device";
const WORKOS_AUTH_URL: &str = "https://api.workos.com/user_management/authenticate";
const CLINE_API_BASE: &str = "https://api.cline.bot/api/v1";

#[derive(Debug, Clone)]
struct Session {
    id: String,
    platform: String,
    /// 扫码登录二维码图（data URI，前端直接 <img> 展示；非扫码平台为空）。
    qr_image: String,
    /// 二维码**文本**（服务端没有编码器时用：前端拿内嵌编码器自己画，如小浣熊）。
    qr_text: String,
    /// wb 族 = state；minimax = "minimax:{device_code}:{verifier}:{user_code}"；跳转型为空
    state: String,
    status: String, // pending | success | error
    message: String,
    login_url: String,
    user_code: String,
}

impl Session {
    /// 把会话置为失败态并记下原因（各登录分支的 Err 侧统一走这里）。
    ///
    /// 旧写法在每个分支里手抄两行（status = "error" + message = ...）。各分支抄了
    /// 十几遍，漏写 status 的那一处会让前端永远显示「等待授权中」—— 看起来像卡住，
    /// 实际是已经在等一个不会来的回调。
    fn fail(&mut self, msg: String) {
        self.status = "error".into();
        self.message = msg;
    }
}

/// 登录会话保留上限。旧实现只 push 不清理：每次点「登录」都留一条永久会话，
/// 长时间运行后面板 /api/login/sessions 会越拉越长（还都带着 login_url）。
const SESSIONS_CAP: usize = 24;

pub struct LoginEngine {
    sessions: Mutex<Vec<Session>>,
}

/// WorkBuddy / CodeBuddy 的「(API 域, 登录来源域, platform 参数)」三元组。
///
/// ★ 这里曾经把 WorkBuddy 国内版指到了 CodeBuddy 的登录页（`www.codebuddy.cn`），
/// 结果是：国内 WorkBuddy 账号要在**另一个产品**的页面上登录，登录完拿到的
/// `domain` 也是 CodeBuddy 的 —— 面板里看起来就是「WorkBuddy 没有国内版」。
///
/// 一手依据（`_refs/workbuddy2api-master/admin/wb_login.py:82,1247`）：
/// 国内 WorkBuddy 的 HOST 是 `www.workbuddy.cn`，且 `/v2/plugin/auth/state`
/// 的 **platform 参数是 `workbuddy`**（不是 CLI）。实测四个域的 state 都能签发，
/// 但登录页内容不同：`workbuddy.cn` 的 `IS_INTERNATIONAL_EDITION='false'`、
/// `workbuddy.ai` 的是 `'true'` —— 这两条就是国内版/国际版的真凭实据。
///
/// - WorkBuddy 国内版：API `copilot.tencent.com`（官方桌面端的真后端），
///   登录域 `www.workbuddy.cn`，platform=`workbuddy`
/// - WorkBuddy 国际版：API/登录都在 `www.workbuddy.ai`，platform=`workbuddy`
/// - CodeBuddy（腾讯另一个产品，不是 WorkBuddy）：API `copilot.tencent.com`，
///   登录域 `www.codebuddy.cn`，platform=`CLI`
fn wb_base(platform: &str) -> Option<(String, String, &'static str)> {
    match platform {
        "wb-gateway" => Some((
            "https://copilot.tencent.com".into(),
            "https://www.workbuddy.cn".into(),
            "workbuddy",
        )),
        "codebuddy" => Some((
            "https://copilot.tencent.com".into(),
            "https://www.codebuddy.cn".into(),
            "CLI",
        )),
        "wb-gateway-intl" => Some((
            "https://www.workbuddy.ai".into(),
            "https://www.workbuddy.ai".into(),
            "workbuddy",
        )),
        "codebuddy-intl" => Some((
            "https://www.workbuddy.ai".into(),
            "https://www.workbuddy.ai".into(),
            "CLI",
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
            qr_image: String::new(),
            qr_text: String::new(),
            state: String::new(),
            status: "pending".into(),
            message: String::new(),
            login_url: String::new(),
            user_code: String::new(),
        };
        if let Some((base, origin, plat_param)) = wb_base(platform) {
            match wb_start(client, &base, &origin, plat_param).await {
                Ok((state, auth_url)) => {
                    sess.state = state;
                    sess.login_url = auth_url;
                    sess.message = "已生成设备授权会话：打开授权页完成登录，面板自动拿到 token 并入库。".into();
                }
                Err(e) => {
                    sess.fail(format!("auth/state 失败：{e}（可回退手动粘贴 token）"));
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
                    sess.fail(format!("设备码申请失败：{e}"));
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
                    sess.fail(format!("WorkOS 设备码申请失败：{e}"));
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
                    sess.fail(format!("Kimi 设备码申请失败：{e}（可回退手动粘贴 token）"));
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
                    sess.fail(format!("Grok 设备码申请失败：{e}（可回退手动粘贴 token）"));
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
                    sess.fail(format!("Codex 授权 URL 生成失败：{e}"));
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
                    sess.fail(format!("Claude 授权 URL 生成失败：{e}"));
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
                    sess.fail(format!("Lobster 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "sub-antigravity" {
            // Antigravity/Google：PKCE 授权码 + 本地 51121 回调（跳转登录，非扫码）
            match antigravity_start().await {
                Ok((url, verifier, state)) => {
                    sess.state = format!("antigravity:{verifier}:{state}");
                    sess.login_url = url.clone();
                    sess.message = "Antigravity 跳转登录（Google 账号）：点「打开授权页」在浏览器完成登录，面板自动捕获回调换 token 入库；若面板部署在服务器，把浏览器跳转后地址栏的 URL 粘贴到下方输入框。".into();
                }
                Err(e) => {
                    sess.fail(format!("Antigravity 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "sub-devin" {
            // Devin headless PKCE：官方 CLI 无 redirect_uri（服务器场景直接粘地址栏 URL）
            match devin_start().await {
                Ok((url, verifier, state)) => {
                    sess.state = format!("devin:{verifier}:{state}");
                    sess.login_url = url.clone();
                    sess.message = "Devin 跳转登录（headless PKCE）：点「打开授权页」在浏览器完成登录后，把跳转后地址栏的完整 URL（带 code=...&state=...）粘贴到下方输入框并点「提交回调」；本机自动捕获回调。".into();
                }
                Err(e) => {
                    sess.fail(format!("Devin 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "sub-meta" {
            // Meta Muse：RFC 8628 设备码 + mint API Key
            match meta_start(&auth_http()).await {
                Ok((device_code, interval, user_code, verify_url)) => {
                    sess.state = format!("meta:{device_code}:{interval}");
                    sess.user_code = user_code.clone();
                    sess.login_url = verify_url.clone();
                    sess.message = "Meta Muse 设备码：打开验证页完成登录后，面板自动轮询换 token 并铸造 API Key 入库。".into();
                }
                Err(e) => {
                    sess.fail(format!("Meta 设备码申请失败：{e}"));
                }
            }
        } else if platform == "codearts" {
            // 华为云 CodeArts：华为云账号跳转登录（ticket 流程）→ 临时 AK/SK 入库
            match codearts_start().await {
                Ok((url, ticket_id, secret)) => {
                    sess.state = format!("codearts:{ticket_id}:{secret}");
                    sess.login_url = url.clone();
                    sess.message = "CodeArts 跳转登录（华为云账号）：点「打开授权页」在浏览器完成华为云登录，面板自动轮询换取临时 AK/SK 入库（AK:SK 格式，转发时 SDK-HMAC-SHA256 签名）。服务器部署：授权完成后把浏览器地址栏 URL（含 secret=...）粘贴到下方输入框并点「提交回调」。".into();
                }
                Err(e) => {
                    sess.fail(format!("CodeArts 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "apk-qoder" || platform == "web-qoder" {
            // Qoder 阿里：设备授权 + PKCE（国际版 qoder.com，参考 qoder2api-hub）
            match qoder_start().await {
                Ok((url, verifier, nonce)) => {
                    sess.state = format!("qoder:{verifier}:{nonce}");
                    sess.login_url = url.clone();
                    sess.message = "Qoder 设备授权（免客户端）：点「打开授权页」在浏览器登录 Qoder 账号，面板自动轮询 deviceToken 入库（dt- 前缀 token，Bearer 转发）。".into();
                }
                Err(e) => {
                    sess.fail(format!("Qoder 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "apk-trae" || platform == "web-trae" {
            // Trae 字节：跳转回调（www.trae.cn/authorization + 本地 18080 收 refreshToken + ExchangeToken）
            match trae_start().await {
                Ok((url, trace_id)) => {
                    sess.state = format!("trae:{trace_id}");
                    sess.login_url = url.clone();
                    sess.message = "Trae 跳转登录：点「打开授权页」在浏览器完成 Trae 登录，面板自动捕获 18080 回调换 token 入库；服务器部署：授权完成后把浏览器地址栏 URL（含 refreshToken=...）粘贴到下方输入框并点「提交回调」。".into();
                }
                Err(e) => {
                    sess.fail(format!("Trae 授权 URL 生成失败：{e}"));
                }
            }
        } else if platform == "apk-doubao" {
            // 豆包网页版扫码登录（doubao2api qr_login.py 一手：passport CSRF + 二维码 + check_qrconnect 轮询）
            match doubao_qr_start(&auth_http(), DOUBAO_BASE_URL).await {
                Ok((state, qr_b64)) => {
                    sess.state = format!("doubao:{state}");
                    sess.qr_image = format!("data:image/png;base64,{qr_b64}");
                    sess.message = "豆包扫码登录：用豆包 App 或手机浏览器扫一扫二维码，确认登录后面板自动收 Cookie 入库（整段 Cookie 直连 samantha 端点，无需再次粘贴）。".into();
                }
                Err(e) => {
                    sess.fail(format!("豆包扫码登录发起失败：{e}（可回退手动粘贴 Cookie）"));
                }
            }
        } else if platform == "apk-kuku" {
            // 库库 AI（百度）扫码登录（kuku2api passport.mjs 一手：passport 二维码 + BDUSS/PTOKEN + STOKEN 交换）
            match kuku_qr_start(&auth_http(), KUKU_PASSPORT_BASE).await {
                Ok((state, qr_b64)) => {
                    sess.state = format!("kukuqr:{state}");
                    sess.qr_image = format!("data:image/png;base64,{qr_b64}");
                    sess.message = "库库 AI 扫码登录：用百度 App 扫一扫二维码，确认登录后面板自动完成 BDUSS/STOKEN 交换并入账号池（无需手动粘贴 Cookie）。".into();
                }
                Err(e) => {
                    sess.fail(format!("库库扫码登录发起失败：{e}（可回退手动粘贴 Cookie）"));
                }
            }
        } else if platform == "apk-loomy" {
            // Loomy（讯飞）微信扫码（dsh-our-free-model loomy-wechat.ts 一手：
            // open.weixin qrconnect → 二维码 → 长轮询 wx_code → 讯飞 bind/skip → session）
            match loomy_qr_start(
                &auth_http(),
                LOOMY_WECHAT_BASE,
                LOOMY_WECHAT_LONG_BASE,
                LOOMY_ACCOUNT_BASE,
            )
            .await
            {
                Ok((state, qr_b64, mime)) => {
                    sess.state = format!("loomyqr:{state}");
                    sess.qr_image = format!("data:{mime};base64,{qr_b64}");
                    sess.message = "Loomy 微信扫码登录：用微信扫一扫二维码并在手机上确认，面板自动换取 14 天有效的 session 并入账号池（该微信若未绑定讯飞账号，请先在官网绑定手机号；也可直接粘贴 session）。".into();
                }
                Err(e) => {
                    sess.fail(format!("Loomy 微信扫码发起失败：{e}（可回退手动粘贴 session）"));
                }
            }
        } else if platform == "apk-raccoon" {
            // 小浣熊（商汤）微信小程序扫码（dsh-our-free-model raccoon.ts 一手）：
            // code 由本端自造（32 位小写 hex，上游只当登记键，不校验来源），二维码内容就是
            // 小程序登录页 URL。Rust 侧没有二维码编码器 ⇒ 把 URL 交给前端用内嵌编码器画。
            let (code, qr_text) = raccoon_qr_start(RACCOON_BASE);
            sess.state = format!("raccoonqr:{code}");
            sess.qr_text = qr_text;
            sess.message = "小浣熊（商汤）微信扫码登录：用微信扫一扫二维码（进入「商汤小浣熊」小程序）并在手机上确认，面板自动换取 access_token 并入账号池（约 3 小时有效，过期后重新扫码即可）。".into();
        } else if let Some(spec) = specs.iter().find(|s| s.id == platform && !s.login_url.is_empty()) {
            // 跳转登录：用户自己浏览器打开登录页，登录后手动粘贴 Cookie/凭据入库。
            sess.login_url = spec.login_url.clone();
            // ★ 别写「面板不支持扫码」：豆包 / 库库 / 小浣熊 / Loomy 四个平台都有真扫码，
            //   一句话把四个都否定掉，用户就不会去试（这些平台的扫码分支在自己那边）。
            sess.message = format!(
                "已为你打开跳转登录页（{}）—— 请在你自己的浏览器完成登录，然后把 Cookie 或凭据粘贴回来（凭据由你掌控）。",
                spec.login_url
            );
        } else {
            sess.fail(format!("平台 {platform} 暂不支持自动登录（手动粘贴凭据到账号池）"));
        }
        let out = json!({
            "id": sess.id, "platform": sess.platform, "status": sess.status,
            "message": sess.message, "login_url": sess.login_url,
            "user_code": sess.user_code, "qr_image": sess.qr_image,
            "qr_text": sess.qr_text,
        });
        {
            // 锁中毒不能静默丢更新：那样会话会永远停在 pending，前端看不到任何结果。
            let mut g = match self.sessions.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            g.retain(|s| s.id != sess.id);
            g.push(sess);
            if g.len() > SESSIONS_CAP {
                // 先丢已经结束（success/error）的老会话，再按最旧丢
                let mut drop_n = g.len() - SESSIONS_CAP;
                let mut i = 0;
                while drop_n > 0 && i < g.len() {
                    if g[i].status != "pending" {
                        g.remove(i);
                        drop_n -= 1;
                    } else {
                        i += 1;
                    }
                }
                if drop_n > 0 {
                    g.drain(0..drop_n);
                }
            }
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
        // 先按老口径拿 Result<String, String>，wb 分支另算（它要带上游给的 domain），
        // 最后统一包成 (凭据, 域)。
        let secret_raw: Result<String, String> = if let Some((base, origin, _)) = wb_base(&sess.platform) {
            if sess.state.is_empty() {
                Err("state 为空（start 未成功）".to_string())
            } else {
                // wb 族要既拿 token 又拿 domain，这里用哨兵分隔塞进同一个 String，
                // 下面拆开 —— 免得专门为一条分支改整个 Result 类型。
                wb_poll_token(client, &base, &origin, &sess.state)
                    .await
                    .map(|(t, d)| format!("{t}\u{1}{d}"))
            }
        } else if sess.platform == "minimax" {
            // state = minimax:{device_code}:{verifier}:{user_code}（4 段）
            // ★ 必须 splitn(4)：用 splitn(3) 时 parts[2] 会是 "{verifier}:{user_code}"，
            // 拼进 code_verifier 后 S256 校验必败，MiniMax 登录 100% 拿不到 token。
            let parts: Vec<&str> = sess.state.splitn(4, ':').collect();
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
        } else if sess.state.starts_with("meta:") {
            // meta:{device_code}:{interval} → 三态轮询 → mint API Key
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("Meta 设备码会话不完整".to_string())
            } else {
                meta_poll(&auth_http(), parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("codearts:") {
            // codearts:{ticket_id}:{secret} → 轮询 ticket 端点 → AK:SK
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("CodeArts 会话不完整".to_string())
            } else {
                codearts_poll(&auth_http(), parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("qoder:") {
            // qoder:{verifier}:{nonce} → 轮询 deviceToken/poll
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("Qoder 会话不完整".to_string())
            } else {
                qoder_poll(&auth_http(), parts[1], parts[2]).await
            }
        } else if sess.state.starts_with("trae:") {
            // trae:{trace_id} → 回调队列取 refreshToken → ExchangeToken
            let trace = sess.state.trim_start_matches("trae:").to_string();
            // 只认 state 精确等于 loginTraceID 的回调。旧实现额外匹配 s.is_empty()，
            // 会把「回调里没带 loginTraceID」的其它平台条目抢走（而且取走后别人的会话就再也拿不到）。
            // trace 本身为空时（sess.state == "trae:"）仍然能命中空 state 条目。
            let code = take_callback(&trace);
            match code {
                Some(rt) => trae_exchange(&auth_http(), &rt).await,
                None => Err("等待授权中（在打开的授权页完成登录，面板自动捕获回调；若未自动捕获，请把浏览器地址栏 URL 粘贴到下方输入框后点「提交回调」）".to_string()),
            }
        } else if sess.state.starts_with("codex:") || sess.state.starts_with("claude:")
            || sess.state.starts_with("antigravity:") || sess.state.starts_with("devin:") {
            // {prefix}:{verifier}:{state} → 从回调队列取 code → 换 token
            let parts: Vec<&str> = sess.state.splitn(3, ':').collect();
            if parts.len() < 3 {
                Err("PKCE 会话不完整".to_string())
            } else {
            let (kind, verifier, state) = (parts[0], parts[1], parts[2]);
            let code = take_callback(state);
            match code {
                Some(code) => {
                    match kind {
                        "codex" => codex_exchange(&auth_http(), &code, verifier).await,
                        "claude" => claude_exchange(&auth_http(), &code, verifier, state).await,
                        "antigravity" => antigravity_exchange(&auth_http(), &code, verifier).await,
                        _ => devin_exchange(&auth_http(), &code, verifier).await,
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
                let code = take_callback(state);
                match code {
                    Some(code) => {
                        lobster_exchange(&auth_http(), &code, parts[2], parts[3]).await
                    }
                    None => Err("等待授权中（在打开的授权页完成登录，面板自动捕获回调；若未自动捕获，请把浏览器地址栏 URL 粘贴到下方输入框后点「提交回调」）".to_string()),
                }
            }
        } else if sess.state.starts_with("doubao:") {
            // doubao:{base64 state} → 轮询 check_qrconnect → 整段登录 Cookie
            let state = sess.state.trim_start_matches("doubao:").to_string();
            doubao_qr_poll(&auth_http(), &state).await
        } else if sess.state.starts_with("kukuqr:") {
            // kukuqr:{base64 state} → 轮询 unicast → BDUSS/PTOKEN → STOKEN 交换 → 整段 Cookie
            let state = sess.state.trim_start_matches("kukuqr:").to_string();
            kuku_qr_poll(&auth_http_long(), &state).await
        } else if sess.state.starts_with("loomyqr:") {
            // loomyqr:{base64 state} → 微信长轮询 → 405 带 wx_code → 讯飞 bind/skip → session
            let state = sess.state.trim_start_matches("loomyqr:").to_string();
            loomy_qr_poll(&auth_http(), &state).await
        } else if sess.state.starts_with("raccoonqr:") {
            // raccoonqr:{32 位 hex code} → POST login_with_qrcode_code → access_token（Bearer）
            let code = sess.state.trim_start_matches("raccoonqr:").to_string();
            raccoon_qr_poll(&auth_http(), RACCOON_BASE, &code).await
        } else {
            Err("跳转登录会话：请粘贴 Cookie/凭据后走 save 动作入库（不自动轮询）".to_string())
        };
        // 拆哨兵：没有分隔符的就是普通 token（非 wb 平台没有域概念）。
        let secret: Result<(String, String), String> = secret_raw.map(|raw| match raw.split_once('\u{1}') {
            Some((t, d)) => (t.to_string(), d.to_string()),
            None => (raw, String::new()),
        });
        // 落库结果：拿到凭据时才填 (account_id, 是否覆盖了已有行)。
        // 「覆盖了已有行」必须回报给上层 —— 换过凭据却不清号池治理状态与粘性绑定，
        // 会让「重新登录后仍然 401」看起来像登录没生效。
        let mut saved: Option<(String, bool)> = None;
        match secret {
            // ★ 先 trim 再判空：上游偶尔返回带换行/空格的 token，
            // 旧实现只判 !is_empty() 就直接落库，而 add_account 对同 platform+name 是**覆盖**，
            // 一个空白 token 会静默顶掉原本可用的密钥。这里与 save() 的 trim 语义保持一致。
            Ok((tok, domain)) if !tok.trim().is_empty() => {
                let tok = tok.trim();
                // doubao 扫码拿到的是整段登录 Cookie，kind 必须 "cookie" 才会被
                // doubao.rs 当作 Cookie 直连；其余平台是 token（Bearer 转发）。
                let (label, kind) = if sess.platform == "apk-doubao" || sess.platform == "apk-kuku" {
                    ("（扫码登录）".to_string(), "cookie")
                } else if sess.platform == "apk-loomy" {
                    // Loomy 的 session 是 Bearer token（不是 Cookie 串），kind 必须 token。
                    ("（微信扫码）".to_string(), "token")
                } else if sess.platform == "apk-raccoon" {
                    // 小浣熊扫码拿到的是 Bearer access_token（上游不认 Cookie），kind 必须 token。
                    ("（微信扫码）".to_string(), "token")
                } else {
                    ("（设备授权）".to_string(), "token")
                };
                // 账号名 = 「平台显示名 · 登录方式」，例如「WorkBuddy 国内站 · 账号登录」。
                // 旧实现写的是「{平台 id}{（设备授权）}」这种机器串（wb-gateway（设备授权）），
                // 账号一多就分不清谁是谁；用平台显示名 + 人话标签，面板上不用再猜。
                let label = label.trim_matches(|c: char| c == '（' || c == '）').to_string();
                let acc_name = match display_name_of(&sess.platform) {
                    Some(n) => format!("{n} · {label}"),
                    None => format!("{} · {label}", sess.platform),
                };
                let (acc, existed) =
                    accounts.add_account_with_domain(&sess.platform, &acc_name, kind, tok, &domain);
                saved = Some((acc.id, existed));
                sess.status = "success".into();
                sess.message = if kind == "cookie" { "已拿到登录 Cookie 并入账号池".into() } else { "已拿到 token 并入账号池".into() };
            }
            Ok(_) => {
                sess.message = "上游未返回凭据（多半还没完成授权），稍后重试".into();
            }
            Err(e) => {
                sess.message = e;
            }
        }
        {
            // 锁中毒不能静默丢更新：那样会话会永远停在 pending，前端看不到任何结果。
            let mut g = match self.sessions.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            if let Some(s) = g.iter_mut().find(|s| s.id == sess.id) {
                *s = sess.clone();
            }
        }
        let mut out = json!({
            "ok": true,
            "session": {"id": sess.id, "status": sess.status, "message": sess.message},
        });
        if let Some((acc_id, existed)) = saved {
            out["account_id"] = json!(acc_id);
            out["credential_replaced"] = json!(existed);
        }
        out
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
        let (acc, existed) =
            accounts.add_account(platform, &pasted_account_name(platform), "cookie", secret.trim());
        json!({
            "ok": true,
            "message": "凭据已保存到账号池（kind=cookie，可在账号页查看）",
            "account_id": acc.id,
            "credential_replaced": existed,
        })
    }
}

impl Default for LoginEngine {
    fn default() -> Self {
        Self::new()
    }
}

async fn wb_start(
    client: &OutClient,
    base: &str,
    origin: &str,
    plat_param: &str,
) -> Result<(String, String), String> {
    let resp = client
        .http
        .post(format!("{base}/v2/plugin/auth/state?platform={plat_param}"))
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
        format!("{origin}/login?platform={plat_param}&state={state}")
    };
    Ok((state, url))
}

/// 轮询设备授权，返回 `(accessToken, domain)`。
///
/// `domain` 是**上游告诉我们这个账号属于哪个域**（`www.workbuddy.cn` /
/// `www.codebuddy.cn` / …）。它必须跟着凭据一起存：后续每个对话请求都要带
/// `X-Domain`，而「国内账号发到国际域」在上游看来就是跨域不一致，直接拒。
async fn wb_poll_token(
    client: &OutClient,
    base: &str,
    origin: &str,
    state: &str,
) -> Result<(String, String), String> {
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
    let domain = v
        .pointer("/data/domain")
        .or_else(|| v.get("domain"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok((tok, domain))
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

/// URL 解码（回调参数值）。
///
/// ★ 回调 URL 里的 code/token 常常是 percent-encoded 的：Google/Antigravity 的 code
/// 形如 `4/0A...` 会被编码成 `4%2F0A...`，token 里的 `+`/`=` 会变成 `%2B`/`%3D`。
/// 旧实现直接存原始串，exchange 时带着 %2F 去换 token 必然失败。
/// 同时把 query 里的 `+` 还原成空格（application/x-www-form-urlencoded 语义）。
pub fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    // 残缺转义（%A、%ZZ）：原样保留 % 继续，避免吞掉后续字符
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 解析 OAuth 回调/粘贴 URL 的 query → (code, state, error)。
/// 统一在这里做 percent-decode，避免两条入口（本地监听 / 粘贴）行为不一致。
pub(crate) fn parse_callback_query(query: &str) -> (String, String, String) {
    let mut code = String::new();
    let mut state = String::new();
    let mut error = String::new();
    for kv in query.split('&') {
        let (k, raw_v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = urldecode(raw_v);
        match k {
            "code" => code = v,
            "state" => state = v,
            "refresh_token" | "refreshToken" => code = v,
            "access_token" | "accessToken" | "token" | "authCode" => {
                if code.is_empty() {
                    code = v;
                }
            }
            "secret" => {
                if code.is_empty() {
                    code = v;
                }
            }
            "loginTraceID" | "login_trace_id" | "ticket_id" => {
                if state.is_empty() {
                    state = v;
                }
            }
            // 用户拒绝授权 / 上游报错：必须识别，否则会话永远停在 pending 无终态
            "error" => error = v,
            "error_description" => {
                if error.is_empty() {
                    error = v;
                }
            }
            _ => {}
        }
    }
    (code, state, error)
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
    #[test]
    fn session_fail_sets_both_status_and_message() {
        // 各登录分支的 Err 侧都走 Session::fail。旧写法手抄两行，
        // 漏写 status 的那一处会让前端永远显示「等待授权中」。
        let mut s = Session {
            id: "x".into(),
            platform: "p".into(),
            qr_image: String::new(),
            qr_text: String::new(),
            state: String::new(),
            status: "pending".into(),
            message: String::new(),
            login_url: String::new(),
            user_code: String::new(),
        };
        s.fail("设备码申请失败：网络错误".into());
        assert_eq!(s.status, "error");
        assert!(s.message.contains("设备码申请失败"), "{}", s.message);
    }

    use super::*;

    /// 用户在表单里会写成各种样子：带空格、带横线、带 +86。上游只认 11 位纯数字，
    /// 归一化错了就是「号码未注册」这种完全误导的报错。
    #[test]
    fn wb_domains_separate_workbuddy_from_codebuddy() {
        // ★ 回归：WorkBuddy 国内版曾被指到 CodeBuddy 的登录页，导致「面板里没有
        //   国内 WorkBuddy」——这里把三条链路钉死。
        let (api, origin, plat) = wb_base("wb-gateway").unwrap();
        assert_eq!(api, "https://copilot.tencent.com");
        assert_eq!(origin, "https://www.workbuddy.cn", "国内 WorkBuddy 的登录域");
        assert_eq!(plat, "workbuddy", "WorkBuddy 的 platform 参数是 workbuddy 不是 CLI");

        let (api, origin, plat) = wb_base("wb-gateway-intl").unwrap();
        assert_eq!(api, "https://www.workbuddy.ai");
        assert_eq!(origin, "https://www.workbuddy.ai");
        assert_eq!(plat, "workbuddy");

        // CodeBuddy 是另一个产品，必须仍然走 codebuddy.cn + CLI
        let (_, origin, plat) = wb_base("codebuddy").unwrap();
        assert_eq!(origin, "https://www.codebuddy.cn");
        assert_eq!(plat, "CLI");

        assert!(wb_base("lobster").is_none());
    }

    #[test]
    fn norm_phone_accepts_the_shapes_people_actually_type() {
        for raw in [
            "13800138000",
            "138 0013 8000",
            "138-0013-8000",
            "+8613800138000",
            "008613800138000",
            "8613800138000",
            " 13800138000 ",
            "138（0013）8000",
        ] {
            assert_eq!(norm_phone(raw).as_deref(), Ok("13800138000"), "输入 {raw:?}");
        }
    }

    #[test]
    fn norm_phone_rejects_obviously_wrong_input() {
        // 空 / 太短 / 太长 / 首位不是 1 / 次位是 0-2 / 含字母 —— 都要在本地就拦住，
        // 不要拿去问上游（那会变成「号码未注册讯飞账号」，把人引到错误方向）。
        for raw in ["", "1380013800", "138001380000", "23800138000", "12800138000", "1380013800a"] {
            assert!(norm_phone(raw).is_err(), "输入 {raw:?} 应该被拒");
        }
        let e = norm_phone("12345").unwrap_err();
        assert!(e.contains("13800138000"), "错误提示该给个正确样例：{e}");
    }

    #[test]
    fn callback_queue_matches_state_and_dedupes() {
        // 队列是全局静态：本测试独占操作，先清空。
        AUTH_CALLBACKS.lock().unwrap().clear();
        push_callback("state-a", "code-1");
        push_callback("state-a", "code-2"); // 同 state 重复回调 → 只保留最新
        assert_eq!(AUTH_CALLBACKS.lock().unwrap().len(), 1);
        assert_eq!(take_callback("state-a").as_deref(), Some("code-2"));
        assert!(take_callback("state-a").is_none(), "取走后不应再命中");

        // 空 state 条目不能被「有 state 的会话」抢走，反之亦然
        push_callback("", "trae-empty");
        push_callback("trace-9", "trae-9");
        assert_eq!(take_callback("trace-9").as_deref(), Some("trae-9"));
        assert_eq!(take_callback("").as_deref(), Some("trae-empty"));

        // 容量上限：超出 CAP 丢最旧
        for i in 0..(AUTH_CALLBACKS_CAP + 5) {
            push_callback(&format!("s{i}"), "c");
        }
        assert_eq!(AUTH_CALLBACKS.lock().unwrap().len(), AUTH_CALLBACKS_CAP);
        AUTH_CALLBACKS.lock().unwrap().clear();
    }

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
        let (state, url) = wb_start(
            &client,
            &format!("http://{addr}"),
            &format!("http://{addr}"),
            "workbuddy",
        )
        .await
        .unwrap();
        assert_eq!(state, "st-1");
        assert!(url.contains("state=st-1") || url.starts_with("http://"), "url={url}");
        let tok = wb_poll_token(&client, &format!("http://{addr}"), &format!("http://{addr}"), &state)
            .await
            .unwrap();
        assert_eq!(tok.0, "tok-9", "token 要取到：{tok:?}");
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

    #[tokio::test]
    async fn doubao_qr_start_and_poll_via_mock() {
        // mock 完整扫码链路：首页 Set-Cookie ttwid → csrf → get_qrcode → check_qrconnect(先 new 后 confirmed)
        // → redirect_url 302 → /final Set-Cookie sessionid。
        use axum::http::header;
        use axum::routing::get;
        use axum::Json;
        use axum::body::Body;
        use axum::response::Response;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let check_calls = Arc::new(AtomicUsize::new(0));
        let check_calls2 = check_calls.clone();
        let addr2 = addr.clone();

        let app = axum::Router::new()
            .route(
                "/",
                get(|| async {
                    let mut resp = Response::new(Body::from("{}"));
                    resp.headers_mut().insert(
                        header::SET_COOKIE,
                        header::HeaderValue::from_static("ttwid=abc; Path=/"),
                    );
                    resp
                }),
            )
            .route(
                "/passport/safe/csrf_token/",
                get(|| async {
                    Json(serde_json::json!({"data":{"passport_csrf_token":"csrf-1"}}))
                }),
            )
            .route(
                "/passport/web/get_qrcode/",
                get(|| async {
                    // 真实 doubao 结构：error_code 在 data 内，顶层只有 message
                    Json(serde_json::json!({
                        "message": "success",
                        "data": {
                            "error_code": 0,
                            "token": "tok-1",
                            "qrcode": "data:image/png;base64,QUJDRA=="
                        }
                    }))
                }),
            )
            .route(
                "/passport/web/check_qrconnect/",
                get(move || {
                    let calls = check_calls2.clone();
                    let addr3 = addr2.clone();
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            Json(serde_json::json!({"message":"success","data":{"error_code":0,"status":"new"}}))
                        } else {
                            Json(serde_json::json!({"message":"success","data":{
                                "error_code":0,
                                "status":"confirmed",
                                "redirect_url": format!("http://{addr3}/hop")
                            }}))
                        }
                    }
                }),
            )
            .route(
                "/hop",
                get(|| async {
                    let mut resp = Response::new(Body::from("go"));
                    *resp.status_mut() = axum::http::StatusCode::FOUND; // 302
                    resp.headers_mut().insert(
                        header::LOCATION,
                        header::HeaderValue::from_static("/final"),
                    );
                    resp
                }),
            )
            .route(
                "/final",
                get(|| async {
                    let mut resp = Response::new(Body::from("ok"));
                    resp.headers_mut().insert(
                        header::SET_COOKIE,
                        header::HeaderValue::from_static("sessionid=sid-42; Path=/"),
                    );
                    resp
                }),
            );
        tokio::spawn(async move { axum::serve(l, app).await });

        let client = OutClient::new();
        let base = format!("http://{addr}");
        let (state_b64, qr_b64) = doubao_qr_start(&client.http(), &base).await.unwrap();
        assert!(!state_b64.is_empty());
        assert!(qr_b64.contains("QUJDRA"), "二维码 base64={qr_b64}");

        // 第一次轮询：new → 等待扫码
        let err = doubao_qr_poll(&client.http(), &state_b64).await.unwrap_err();
        assert!(err.contains("等待扫码"), "err={err}");

        // 第二次轮询：confirmed → 302 → /final 收 sessionid（整段 Cookie）
        let cookie = doubao_qr_poll(&client.http(), &state_b64).await.unwrap();
        assert!(cookie.contains("sessionid=sid-42"), "cookie={cookie}");
        assert!(cookie.contains("ttwid=abc"), "cookie={cookie}");
    }

    #[tokio::test]
    async fn kuku_qr_start_and_poll_via_mock() {
        // mock passport 完整链路：首页 Set-Cookie BAIDUID → getqrcode(JSONP) → qrcode PNG
        // → unicast(先 pending 后 confirmed 带 ch.v) → bdusslogin(Set-Cookie BDUSS/PTOKEN)
        // → POST /v3/login/api/auth 换 STOKEN。
        use axum::http::header;
        use axum::routing::{get, post};
        use axum::body::Body;
        use axum::response::Response;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let poll_calls = Arc::new(AtomicUsize::new(0));
        let poll_calls2 = poll_calls.clone();

        let app = axum::Router::new()
            .route(
                "/",
                get(|| async {
                    let mut resp = Response::new(Body::from("{}"));
                    resp.headers_mut().insert(
                        header::SET_COOKIE,
                        header::HeaderValue::from_static("BAIDUID=buid-1; Path=/"),
                    );
                    resp
                }),
            )
            .route(
                "/v2/api/getqrcode",
                get(|| async {
                    // JSONP：errno/ sign 在 JSON 顶层
                    let body = "bd__cbs__x({\"errno\":0,\"sign\":\"0000000000000000000000000000000s\"})";
                    Response::new(Body::from(body))
                }),
            )
            .route(
                "/v2/api/qrcode",
                get(|| async {
                    // PNG 占位（非空即可）
                    let png = [0x89u8, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];
                    Response::new(Body::from(png.to_vec()))
                }),
            )
            .route(
                "/channel/unicast",
                get(move || {
                    let calls = poll_calls2.clone();
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            // 未扫码：errno=1
                            Response::new(Body::from("bd__cbs__y({\"errno\":1,\"channel_v\":\"\"})"))
                        } else {
                            // confirmed：顶层 errno=0 + channel_v 是「JSON 字符串」，其 status=0、v=32 位 ticket
                            let cv = "{\"status\":\"0\",\"v\":\"11111111111111111111111111111111\",\"u\":\"http://u\"}";
                            let payload = serde_json::json!({"errno": 0, "channel_v": cv}).to_string();
                            Response::new(Body::from(format!("bd__cbs__z({payload})")))
                        }
                    }
                }),
            )
            .route(
                "/v2/api/bdusslogin",
                get(|| async {
                    let mut resp = Response::new(Body::from("bd__cbs__w({\"data\":{},\"errInfo\":{}})"));
                    resp.headers_mut().insert(
                        header::SET_COOKIE,
                        header::HeaderValue::from_static("BDUSS=bd-value-192; Path=/"),
                    );
                    resp.headers_mut().append(
                        header::SET_COOKIE,
                        header::HeaderValue::from_static("PTOKEN=pt-value-32; Path=/"),
                    );
                    resp
                }),
            )
            .route(
                "/v3/login/api/auth",
                post(|| async {
                    axum::Json(serde_json::json!({"errno":0,"stoken_list":{"genflowpro":"st-ok"}}))
                }),
            );
        tokio::spawn(async move { axum::serve(l, app).await });

        let client = OutClient::new();
        let base = format!("http://{addr}");
        let (state_b64, qr_b64) = kuku_qr_start(&client.http(), &base).await.unwrap();
        assert!(!state_b64.is_empty());
        assert!(!qr_b64.is_empty(), "二维码 base64={qr_b64}");

        // 第一次轮询：未扫码 → 等待
        let err = kuku_qr_poll(&client.http(), &state_b64).await.unwrap_err();
        assert!(err.contains("等待扫码"), "err={err}");

        // 第二次轮询：confirmed → BDUSS 交换 → STOKEN → 整段 Cookie
        let cred = kuku_qr_poll(&client.http(), &state_b64).await.unwrap();
        assert!(cred.contains("BDUSS=bd-value-192"), "cred={cred}");
        assert!(cred.contains("STOKEN=st-ok"), "cred={cred}");
    }


    #[test]
    fn raccoon_qr_start_makes_scannable_url() {
        // code 是本端自造的 32 位小写 hex；二维码内容必须是小程序登录页 URL（微信只认这个形状）。
        let (code, url) = raccoon_qr_start(RACCOON_BASE);
        assert_eq!(code.len(), 32, "code={code}");
        assert!(
            code.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "code 必须小写 hex：{code}"
        );
        assert!(
            url.starts_with("https://xiaohuanxiong.com/login/mp?code="),
            "url={url}"
        );
        assert!(url.contains(&code), "url 里的 code 必须与 state 一致：{url}");
        assert!(url.contains("appname="), "url={url}");
        // 二维码容量上限：编码器只支持 ≤213 字节（版本 1-10），这个 URL 实测 144 字节。
        assert!(url.len() <= 213, "url 太长，前端编码器会抛错：{}", url.len());
    }

    #[tokio::test]
    async fn raccoon_qr_poll_maps_upstream_status() {
        // 上游四态：pending / logging / canceled / success，外加业务错误码与空 token 降级。
        use axum::body::Body;
        use axum::response::Response;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let step = Arc::new(AtomicUsize::new(0));
        let step2 = step.clone();
        let app = axum::Router::new().route(
            "/api/web/auth/v1/login_with_qrcode_code",
            axum::routing::post(move || {
                let s = step2.clone();
                async move {
                    let n = s.fetch_add(1, Ordering::SeqCst);
                    let body = match n {
                        0 => json!({"code": 0, "message": "success", "data": {"status": "pending"}}),
                        1 => json!({"code": 0, "message": "success", "data": {"status": "logging"}}),
                        2 => json!({"code": 0, "message": "success", "data": {"status": "canceled"}}),
                        // success 但 access_token 空：一手实现降级 pending，绝不能落空串
                        3 => json!({"code": 0, "message": "success", "data": {"status": "success", "access_token": ""}}),
                        4 => json!({"code": 0, "message": "success", "data": {"status": "success", "access_token": " tok-raccoon-1 "}}),
                        _ => json!({"code": 40001, "message": "qrcode expired", "data": null}),
                    };
                    Response::new(Body::from(body.to_string()))
                }
            }),
        );
        tokio::spawn(async move { axum::serve(l, app).await });

        let client = OutClient::new();
        let base = format!("http://{addr}");
        let code = "0123456789abcdef0123456789abcdef";

        let e = raccoon_qr_poll(&client.http(), &base, code).await.unwrap_err();
        assert!(e.contains("等待扫码"), "pending → 等待扫码，e={e}");
        let e = raccoon_qr_poll(&client.http(), &base, code).await.unwrap_err();
        assert!(e.contains("已扫码"), "logging → 已扫码，e={e}");
        let e = raccoon_qr_poll(&client.http(), &base, code).await.unwrap_err();
        assert!(e.contains("已取消"), "canceled → 已取消（前端据此自动换码），e={e}");
        let e = raccoon_qr_poll(&client.http(), &base, code).await.unwrap_err();
        assert!(e.contains("等待扫码"), "空 access_token 必须降级 pending，e={e}");
        let tok = raccoon_qr_poll(&client.http(), &base, code).await.unwrap();
        assert_eq!(tok, "tok-raccoon-1", "token 必须 trim");
        let e = raccoon_qr_poll(&client.http(), &base, code).await.unwrap_err();
        assert!(e.contains("qrcode expired"), "业务错误码要原样带出上游原因，e={e}");
    }

    /// 密码登录的加密环节必须是标准的 PKCS#1 v1.5：上游用私钥解，格式错就得 401。
    #[test]
    fn rsa_encrypt_password_round_trips_with_pkcs1_v15() {
        use base64::Engine as _;
        use rsa::pkcs1::EncodeRsaPublicKey;
        use rsa::pkcs8::EncodePublicKey;
        use rsa::traits::PublicKeyParts;
        use rsa::RsaPrivateKey;
        // 上游给的是 1024 位，这里照同规格造一把，慢一点也要贴真实形态。
        let mut rng = rand::thread_rng();
        let priv_key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        let pub_key = priv_key.to_public_key();
        // 两种 DER 形态都要能吃下：上游实际发的是 PKCS#1。
        for der in [
            pub_key.to_pkcs1_der().unwrap().to_vec(),
            pub_key.to_public_key_der().unwrap().to_vec(),
        ] {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
            let enc = rsa_encrypt_password(&b64, "hunter2").unwrap();
            let raw = base64::engine::general_purpose::STANDARD.decode(&enc).unwrap();
            let dec = priv_key
                .decrypt(rsa::Pkcs1v15Encrypt, &raw)
                .expect("密文必须能用私钥按 PKCS#1 v1.5 解回");
            assert_eq!(dec, b"hunter2");
            assert_eq!(raw.len(), pub_key.size(), "密文长度必须等于模长");
        }
    }

    /// 公钥解析失败要说清是公钥的问题（而不是含糊的上游错误）。
    #[test]
    fn rsa_encrypt_password_rejects_a_broken_public_key() {
        let e = rsa_encrypt_password("bm90LWEta2V5", "pw").unwrap_err();
        assert!(e.contains("公钥解析失败"), "{e}");
        let e = rsa_encrypt_password("!!!", "pw").unwrap_err();
        assert!(e.contains("base64"), "{e}");
    }

    #[tokio::test]
    async fn loomy_qr_start_and_poll_via_mock() {
        // mock 微信 + 讯飞账号完整链路：
        // qrconnect 授权页(含 /connect/qrcode/<uuid>) → 二维码 JPEG
        // → 长轮询(先 408 待扫码，再 405 带 wx_code)
        // → 讯飞 bind/auth(bind=1) → bind/skip → 32 位 session
        use axum::body::Body;
        use axum::response::Response;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();

        let app = axum::Router::new()
            .route(
                "/connect/qrconnect",
                axum::routing::get(|| async {
                    // 真实授权页里 uuid 出现在 window.QRLogin.uuid 与 /connect/qrcode/<uuid> 两处
                    let html = "<html><script>var u = '/connect/qrcode/abc123def456';</script></html>";
                    Response::new(Body::from(html))
                }),
            )
            .route(
                "/connect/qrcode/abc123def456",
                axum::routing::get(|| async {
                    // JPEG 占位：FF D8 FF 魔数 + 填充到 300 字节（脚本要求 >= 200 字节）
                    let mut jpg = vec![0xFFu8, 0xD8, 0xFF];
                    jpg.extend(std::iter::repeat(0u8).take(300));
                    Response::new(Body::from(jpg))
                }),
            )
            .route(
                "/connect/l/qrconnect",
                axum::routing::get(move || {
                    let c = calls2.clone();
                    async move {
                        let n = c.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            // 408 = 用户还没扫码
                            Response::new(Body::from(
                                "window.wx_errcode=408;window.wx_code='';",
                            ))
                        } else {
                            // 405 = 手机已确认，wx_code 就在这一帧
                            Response::new(Body::from(
                                "window.wx_errcode=405;window.wx_code='wx-code-1';",
                            ))
                        }
                    }
                }),
            )
            .route(
                "/login/thirdAccount/bind/auth",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "code": "000000",
                        "data": {"bind": 1, "rcode": "rc-1"}
                    }))
                }),
            )
            .route(
                "/login/thirdAccount/bind/skip",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "code": "000000",
                        "data": {"session": "0123456789abcdef0123456789abcdef", "userid": "u-1"}
                    }))
                }),
            );
        tokio::spawn(async move { axum::serve(l, app).await });

        let client = OutClient::new();
        let base = format!("http://{addr}");
        let (state_b64, qr_b64, mime) = loomy_qr_start(&client.http(), &base, &base, &base)
            .await
            .unwrap();
        assert!(!state_b64.is_empty());
        assert!(!qr_b64.is_empty());
        assert_eq!(mime, "image/jpeg");

        // 第一次轮询：408 → 等待扫码
        let err = loomy_qr_poll(&client.http(), &state_b64).await.unwrap_err();
        assert!(err.contains("等待扫码"), "err={err}");

        // 第二次轮询：405 + wx_code → bind/skip → session
        let session = loomy_qr_poll(&client.http(), &state_b64).await.unwrap();
        assert_eq!(session, "0123456789abcdef0123456789abcdef");
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
        // 每次发起 Lobster 登录都会绑一个新的随机端口；拿到回调或超时后必须退出，
        // 否则反复登录会累积一堆永不关闭的监听 socket + 常驻任务。
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(600));
        tokio::pin!(deadline);
        loop {
            let (mut sock, _) = tokio::select! {
                r = listener.accept() => match r {
                    Ok(x) => x,
                    Err(_) => continue,
                },
                _ = &mut deadline => {
                    tracing::debug!("Lobster 回调监听超时退出（未收到授权回调）");
                    return;
                }
            };
            // 单次登录只等一个回调：内联处理（读一个请求很快），拿到就退出循环。
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => continue,
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
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
            let body = "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权成功 ✓</h2><p>可以关闭此页，返回面板查看账号状态。</p></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            if !code.is_empty() && !state.is_empty() {
                push_callback(&state, &code);
                return;
            }
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

/// 回调队列容量上限。旧实现只 push 不 trim，登录会话被丢弃/超时后条目永久残留：
/// 内存缓慢增长，且陈旧的空 state 条目会被后续会话误配（见 trae 分支）。
const AUTH_CALLBACKS_CAP: usize = 32;

/// 入队一条回调（超出上限时丢弃最旧的）。所有 push 都必须走这里。
fn push_callback(state: &str, code: &str) {
    let mut q = match AUTH_CALLBACKS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    // 同一 state 只保留最新一条：用户重复点授权、或回调页被刷新时，
    // 旧条目会一直占着队列（容量 32）把别人的会话挤掉。
    q.retain(|(s, _)| s != state);
    q.push((state.to_string(), code.to_string()));
    if q.len() > AUTH_CALLBACKS_CAP {
        let over = q.len() - AUTH_CALLBACKS_CAP;
        q.drain(0..over);
    }
}

/// 取出并移除匹配 state 的回调。
fn take_callback(state: &str) -> Option<String> {
    let mut q = match AUTH_CALLBACKS.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    // 空 state 的条目只允许被空 state 的会话取走。
    // 踩过的坑：旧实现用 `s == state` 精确比较，但**空 state 条目**（回调 URL 里
    // 没带 loginTraceID 的平台）会和 trae 的 `state="trae:"`（trace 为空）互相命中，
    // 于是 A 平台的回调被 trae 会话吃掉，A 自己永远等不到 code。
    if state.is_empty() {
        let pos = q.iter().position(|(s, _)| s.is_empty())?;
        return Some(q.remove(pos).1);
    }
    let pos = q.iter().position(|(s, _)| s == state)?;
    Some(q.remove(pos).1)
}

/// 回调端口绑定状态（端口, 是否成功, 失败原因）。
///
/// 为什么要暴露出来：1455/54545 被别的程序占着（Codex CLI 在跑、上次进程没退干净）时
/// 授权码根本进不了队列，用户对着「等待授权中」反复点，而面板上没有任何线索说明
/// 是端口没绑上。只打一行 tracing::warn 等于把它藏进日志。
static CALLBACK_PORTS: std::sync::Mutex<Vec<(u16, bool, String)>> =
    std::sync::Mutex::new(Vec::new());

/// 读回调端口绑定状态：[(端口, 已绑定, 失败原因)]。
pub fn callback_port_status() -> Vec<(u16, bool, String)> {
    CALLBACK_PORTS
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|e| e.into_inner().clone())
}

fn note_callback_port(port: u16, ok: bool, err: String) {
    let mut g = CALLBACK_PORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    g.retain(|(p, _, _)| *p != port);
    g.push((port, ok, err));
}

/// 启动 OAuth 本地回调监听：
/// - 1455=Codex、54545=Claude、51121=Antigravity（PKCE code+state）
/// - 18080=Trae（/authorize?refreshToken=..&loginTraceID=..，token 直传回调）
/// - 51122=CodeArts（/authentication?secret=..&ticket_id=..，ticket 流程回调）
/// 绑定失败仅告警（登录可用「粘贴回调 URL」兜底）。
pub fn start_callback_listeners() {
    for port in [1455u16, 54545u16, 51121u16, 18080u16, 51122u16] {
        let bind = format!("127.0.0.1:{port}");
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&bind).await {
                Ok(l) => {
                    note_callback_port(port, true, String::new());
                    l
                }
                Err(e) => {
                    tracing::warn!("OAuth 回调监听 {bind} 绑定失败：{e}（登录可用粘贴回调 URL 兜底）");
                    note_callback_port(port, false, format!("{e}"));
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
                    let (code, state, error) = parse_callback_query(query);
                    if !code.is_empty() {
                        push_callback(&state, &code);
                    }
                    // ★ 拒绝授权也要给终态：旧实现无条件回「授权成功 ✓」，
                    // 用户点了「取消」后页面说成功、面板却永远 pending，无法判断发生了什么。
                    let body = if !error.is_empty() {
                        let safe: String = error.chars().take(200).collect();
                        format!(
                            "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权未完成 ✗</h2><p>{}</p><p>可以关闭此页，返回面板重试登录。</p></body></html>",
                            safe
                                .replace('&', "&amp;")
                                .replace('<', "&lt;")
                                .replace('>', "&gt;")
                        )
                    } else if code.is_empty() {
                        "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权未完成 ✗</h2><p>回调里没有拿到授权码（code/token）。</p><p>可以关闭此页，返回面板改用「粘贴回调 URL」。</p></body></html>".to_string()
                    } else {
                        "<html><body style=\"font-family:sans-serif;padding:40px\"><h2>授权成功 ✓</h2><p>可以关闭此页，返回面板查看账号状态。</p></body></html>".to_string()
                    };
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
    let (code, state, error) = parse_callback_query(query);
    if !error.is_empty() {
        return Err(format!("授权未完成：{}", error.chars().take(200).collect::<String>()));
    }
    if code.is_empty() {
        return Err("回调地址缺少 code/secret/refreshToken 参数".into());
    }
    push_callback(&state, &code);
    Ok(())
}

/// ══════════════════════════════════════════════════════════════
/// Antigravity/Google PKCE（cliproxy-src internal/auth/antigravity 一手）
/// ══════════════════════════════════════════════════════════════

async fn antigravity_start() -> Result<(String, String, String), String> {
    let (verifier, challenge) = pkce_pair();
    let state = oauth_state();
    let url = format!(
        "{ANTI_AUTH_URL}?client_id={ANTI_CLIENT_ID}&redirect_uri={}&response_type=code&scope={}&state={state}&code_challenge={challenge}&code_challenge_method=S256&prompt=consent&access_type=offline",
        urlencode(ANTI_REDIRECT),
        urlencode(ANTI_SCOPE)
    );
    Ok((url, verifier, state))
}

/// 用授权码换 Antigravity/Google token。
async fn antigravity_exchange(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let resp = http
        .post(ANTI_TOKEN_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&client_id={ANTI_CLIENT_ID}&code={code}&redirect_uri={}&code_verifier={verifier}",
            urlencode(ANTI_REDIRECT)
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

/// ══════════════════════════════════════════════════════════════
/// Meta Muse 设备码 + mint API Key（cliproxy-src internal/auth/meta 一手）
/// ══════════════════════════════════════════════════════════════

async fn meta_start(
    http: &reqwest::Client,
) -> Result<(String, String, String, String), String> {
    let resp = http
        .post(META_DEVICE_URL)
        .timeout(std::time::Duration::from_secs(15))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header("User-Agent", META_UA)
        .body(format!("client_id={META_CLIENT_ID}"))
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
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
        .unwrap_or("https://auth.meta.com/activate")
        .to_string();
    let interval = v.get("interval").and_then(|x| x.as_i64()).unwrap_or(5).to_string();
    if device_code.is_empty() || user_code.is_empty() {
        return Err("Meta 设备码响应缺少 device_code/user_code".into());
    }
    Ok((device_code, interval, user_code, verify))
}

/// Meta 轮询 → dca_token → mint API Key（usable credential = api_key）。
async fn meta_poll(
    http: &reqwest::Client,
    device_code: &str,
    interval: &str,
) -> Result<String, String> {
    let interval_s = interval.parse::<u64>().unwrap_or(5).max(5);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("Meta 设备码授权超时（15 分钟）".into());
        }
        let resp = http
            .post(META_TOKEN_URL)
            .timeout(std::time::Duration::from_secs(15))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("User-Agent", META_UA)
            .body(format!(
                "grant_type={META_DEVICE_GRANT}&device_code={device_code}&client_id={META_CLIENT_ID}"
            ))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if status == 200 {
            let dca = v.get("access_token").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if dca.is_empty() {
                return Err("Meta 响应无 access_token".into());
            }
            return meta_mint(http, &dca).await;
        }
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
        match err {
            "authorization_pending" => {
                tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
            }
            "slow_down" => {
                tokio::time::sleep(std::time::Duration::from_secs(interval_s + 5)).await;
            }
            "access_denied" => return Err("用户拒绝了授权".into()),
            "expired_token" => return Err("设备码已过期，请重新开始登录".into()),
            "" => {
                tokio::time::sleep(std::time::Duration::from_secs(interval_s)).await;
            }
            _ => return Err(format!("Meta 授权服务器错误：{err}")),
        }
    }
}

/// 用 dca_token 铸造 Meta API Key。
async fn meta_mint(http: &reqwest::Client, dca: &str) -> Result<String, String> {
    let resp = http
        .post(META_MINT_URL)
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {dca}"))
        .header("User-Agent", META_UA)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(json!({"dca_token": dca}).to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "mint HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
    }
    let api_key = v.get("api_key").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if api_key.is_empty() {
        return Err("Meta mint 响应无 api_key".into());
    }
    Ok(api_key)
}

/// ══════════════════════════════════════════════════════════════
/// Devin headless PKCE（cliproxy-src internal/auth/devin 一手）
/// 官方 CLI 无 redirect_uri + cli_pkce_marker=1；服务器场景粘贴地址栏 URL。
/// ══════════════════════════════════════════════════════════════

async fn devin_start() -> Result<(String, String, String), String> {
    let (verifier, challenge) = pkce_pair();
    let state = oauth_state();
    let url = format!(
        "{DEVIN_APP_BASE}/auth/cli/continue?prompt=select_account&code_challenge={challenge}&code_challenge_method=S256&cli_pkce_marker=1&state={state}"
    );
    Ok((url, verifier, state))
}

/// 用授权码换 Devin session token。
async fn devin_exchange(
    http: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let resp = http
        .post(format!("{DEVIN_API_BASE}/auth/cli/token"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(json!({"code": code, "code_verifier": verifier}).to_string())
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
    let tok = v.get("token").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if tok.is_empty() {
        return Err("响应无 token".into());
    }
    Ok(tok)
}

/// ══════════════════════════════════════════════════════════════
/// 华为云 CodeArts 跳转登录（01 DSH login.ts ticket 流程一手，免 DPoP）
/// ══════════════════════════════════════════════════════════════

fn rand_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 生成登录 URL + ticket_id + secret（回调 51122/authentication）。
async fn codearts_start() -> Result<(String, String, String), String> {
    let ticket_id = rand_hex(16); // uuid hex
    let secret = rand_hex(32); // 64 字符（login.ts generateRandomSecret）
    let redirect = format!(
        "{CODEARTS_LOGIN_BASE}?IdeaType=jetbrains&auth_callback_url={}&plugin-name={CODEARTS_PLUGIN}&plugin-version={CODEARTS_PLUGIN_VER}&ticket_id={}",
        urlencode(CODEARTS_CALLBACK),
        urlencode(&ticket_id)
    );
    let login_url = format!(
        "{HUAWEI_AUTH_BASE}?service={}",
        urlencode(&redirect)
    );
    Ok((login_url, ticket_id, secret))
}

/// 轮询 ticket 端点直到返回临时 AK/SK（AK:SK 入库，供 SDK-HMAC-SHA256 签名）。
async fn codearts_poll(
    http: &reqwest::Client,
    ticket_id: &str,
    secret: &str,
) -> Result<String, String> {
    // ★ ticket 流程的 secret 由华为云在回调里下发（auth_callback_url?secret=..&ticket_id=..），
    // start 时本地生成的那个华为云根本不知道，直接拿去查必然 404 → 轮询 120s 后超时。
    // 所以先消费回调队列：本地监听 51122 收到回调、或用户粘贴回调 URL，都能换到真 secret。
    // 回调通常在用户完成华为云登录**之后**才到，所以每轮都要再看一次队列，
    // 拿到真 secret 后重建 URL（本地自造的那个只有占位意义）。
    let mut real_secret = take_callback(ticket_id).unwrap_or_default();
    let mut url = format!(
        "{CODEARTS_CRED_ENDPOINT}?ticket_id={}&secret={}",
        urlencode(ticket_id),
        if real_secret.is_empty() { secret } else { &real_secret }
    );
    for i in 0..120 {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if real_secret.is_empty() {
            if let Some(s) = take_callback(ticket_id) {
                if !s.is_empty() {
                    real_secret = s;
                    url = format!(
                        "{CODEARTS_CRED_ENDPOINT}?ticket_id={}&secret={}",
                        urlencode(ticket_id),
                        urlencode(&real_secret)
                    );
                }
            }
        }
        let resp = match http
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .header("Content-Type", "application/json;charset=UTF-8")
            .header("plugin-name", CODEARTS_PLUGIN)
            .header("plugin-version", CODEARTS_PLUGIN_VER)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !resp.status().is_success() {
            continue;
        }
        let v: Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let cred = v.get("credential").or_else(|| v.get("result"));
        if let Some(c) = cred {
            let ak = c.get("access").or_else(|| c.get("accessKeyId")).and_then(|x| x.as_str()).unwrap_or("");
            let sk = c.get("secret").or_else(|| c.get("secretAccessKey")).and_then(|x| x.as_str()).unwrap_or("");
            if !ak.is_empty() && !sk.is_empty() {
                return Ok(format!("{ak}:{sk}"));
            }
        }
    }
    Err("CodeArts 登录超时（120s）：请确认已在华为云页面完成登录后重试".into())
}

/// ══════════════════════════════════════════════════════════════
/// Qoder 设备授权 + PKCE（16 qoder2api-hub 一手，国际版 qoder.com）
/// ══════════════════════════════════════════════════════════════

async fn qoder_start() -> Result<(String, String, String), String> {
    let (verifier, challenge) = pkce_pair();
    let nonce = rand_hex(16); // uuid4().hex 无横线
    // client_id：官方国际版设备授权 URL 必带（qoder2api-hub QODER_REGIONS
    // "intl".client_id 且 send_client_id=True；国内版带 redirect_uri、国际版不带）。
    let url = format!(
        "{QODER_WEBSITE}/device/selectAccounts?client_id={QODER_CLIENT_ID}&challenge={challenge}&challenge_method=S256&nonce={nonce}"
    );
    Ok((url, verifier, nonce))
}

/// 轮询 deviceToken/poll（200 返回 dt- access token；404/202=继续等）。
async fn qoder_poll(
    http: &reqwest::Client,
    verifier: &str,
    nonce: &str,
) -> Result<String, String> {
    let url = format!(
        "{QODER_OPENAPI}/api/v1/deviceToken/poll?nonce={nonce}&verifier={}&challenge_method=S256",
        urlencode(verifier)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        if std::time::Instant::now() > deadline {
            return Err("Qoder 设备授权超时（10 分钟）".into());
        }
        let resp = http
            .get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        match status {
            200 => {
                let v: Value = resp.json().await.unwrap_or(Value::Null);
                let tok = v
                    .get("access_token")
                    .or_else(|| v.get("token"))
                    .or_else(|| v.get("data").and_then(|d| d.get("access_token")))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if !tok.is_empty() {
                    return Ok(tok);
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            404 | 202 => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            401 | 403 => return Err(format!("Qoder 授权失败（HTTP {status}）")),
            _ => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

/// ══════════════════════════════════════════════════════════════
/// Trae 跳转回调（19 trae2api 一手：www.trae.cn/authorization + 18080 回调 + ExchangeToken）
/// ══════════════════════════════════════════════════════════════

async fn trae_start() -> Result<(String, String), String> {
    let trace_id = rand_hex(8); // login_trace_id hex16
    let machine_id = rand_hex(16); // hex32
    let mut device_id = String::new();
    {
        use rand::RngCore;
        let mut b = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut b);
        for x in b {
            device_id.push(char::from(b'0' + (x % 10)));
        }
    }
    let url = format!(
        "{TRAE_CONSOLE}/authorization?login_version=1&auth_from=solo&login_channel=native_ide&plugin_version=2.3.62834&auth_type=local&client_id={TRAE_CLIENT_ID}&redirect=0&login_trace_id={trace_id}&auth_callback_url={}&machine_id={machine_id}&device_id={device_id}&x_device_id={device_id}&x_machine_id={machine_id}&x_device_brand=PC&x_device_type=PC&x_os_version=1.0&x_app_version={TRAE_IDE_VERSION}&x_app_type=stable",
        urlencode(TRAE_CALLBACK)
    );
    Ok((url, trace_id))
}

/// 用回调 refreshToken 换 Trae token（ExchangeToken）。
async fn trae_exchange(http: &reqwest::Client, refresh_token: &str) -> Result<String, String> {
    let resp = http
        .post(format!("{TRAE_OAUTH_HOST}/cloudide/api/v3/trae/oauth/ExchangeToken"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        // X-App-Id：官方 SOLO 客户端 ExchangeToken 必带（JeffHu0912/trae2api
        // work_client.go:181-192 → WorkAppIDChat）。缺这个头上游直接拒绝。
        .header("X-App-Id", TRAE_APP_ID)
        .body(json!({
            "ClientID": TRAE_CLIENT_ID,
            "RefreshToken": refresh_token,
            "ClientSecret": "-",
            "UserID": "",
        }).to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let v: Value = resp.json().await.map_err(|e| format!("解析失败：{e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "Trae HTTP {status}：{}",
            v.to_string().chars().take(150).collect::<String>()
        ));
    }
    let tok = v
        .get("token")
        .or_else(|| v.get("access_token"))
        .or_else(|| v.get("accessToken"))
        .or_else(|| v.get("data").and_then(|d| d.get("token")))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if tok.is_empty() {
        return Err("Trae 响应无 token".into());
    }
    Ok(tok)
}

/// ══════════════════════════════════════════════════════════════
/// 豆包网页版扫码登录（doubao2api qr_login.py 一手：passport CSRF + 二维码 + check_qrconnect 轮询）
/// 纯 HTTP，无需无头浏览器。secret = 整段登录 Cookie（doubao.rs 直连 samantha 端点用）。
/// ══════════════════════════════════════════════════════════════

/// Set-Cookie 头 → (name, value) 列表（丢弃 path/domain/max-age 等属性）。
fn parse_set_cookies(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for v in headers.get_all("set-cookie").iter() {
        if let Ok(raw) = v.to_str() {
            if let Some(first) = raw.split(';').next() {
                if let Some((k, val)) = first.split_once('=') {
                    out.push((k.trim().to_string(), val.trim().to_string()));
                }
            }
        }
    }
    out
}

/// 启动豆包扫码：取 ttwid → CSRF → get_qrcode → (state, 二维码 base64)。
async fn doubao_qr_start(http: &reqwest::Client, base: &str) -> Result<(String, String), String> {
    // 1) 首页收集基础 cookies（ttwid 等）
    let home = http
        .get(base)
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", DOUBAO_UA)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{base}/chat/login"))
        .header("Origin", base)
        .send()
        .await
        .map_err(|e| format!("访问豆包首页失败：{e}"))?;
    let cookies = parse_set_cookies(home.headers());
    // 2) CSRF：cookie 里 passport_csrf_token / passport_csrf_token_default，没有则拉接口
    let mut csrf = cookies
        .iter()
        .find(|(k, _)| k == "passport_csrf_token" || k == "passport_csrf_token_default")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    if csrf.is_empty() {
        let r = http
            .get(format!("{base}/passport/safe/csrf_token/?aid={DOUBAO_AID}"))
            .timeout(std::time::Duration::from_secs(15))
            .header("User-Agent", DOUBAO_UA)
            .header("Accept", "application/json, text/plain, */*")
            .header("Referer", format!("{base}/chat/login"))
            .header("Origin", base)
            .send()
            .await
            .map_err(|e| format!("取 CSRF 失败：{e}"))?;
        let v: Value = r.json().await.unwrap_or(Value::Null);
        csrf = v
            .get("data")
            .and_then(|d| d.get("passport_csrf_token"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
    }
    if csrf.is_empty() {
        return Err("拿不到 passport CSRF token".into());
    }
    // 3) 二维码
    let next = urlencode(base);
    let cookie_str = cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let r = http
        .get(format!(
            "{base}/passport/web/get_qrcode/?next={next}&aid={DOUBAO_AID}"
        ))
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", DOUBAO_UA)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{base}/chat/login"))
        .header("Origin", base)
        .header("x-tt-passport-csrf-token", &csrf)
        .header("Cookie", &cookie_str)
        .send()
        .await
        .map_err(|e| format!("取二维码失败：{e}"))?;
    let v: Value = r.json().await.map_err(|e| format!("二维码响应解析失败：{e}"))?;
    let d0 = v.get("data").cloned().unwrap_or(Value::Null);
    if d0.get("error_code").and_then(|x| x.as_i64()) != Some(0)
        || v.get("message").and_then(|x| x.as_str()) != Some("success")
    {
        return Err(format!(
            "get_qrcode 失败：{}",
            truncate(&v.to_string(), 160)
        ));
    }
    // token/qrcode 在 data 字段内（doubao2api qr_login.py 同为 data["data"] 再取值）
    let d = v.get("data").cloned().unwrap_or(Value::Null);
    let token = d
        .get("token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let qrcode = d
        .get("qrcode")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if token.is_empty() || qrcode.is_empty() {
        return Err("get_qrcode 响应缺少 token/qrcode".into());
    }
    // qrcode 三态：data: 前缀 → 取逗号后 base64；http(s) 前缀 → 下载原图转 base64；否则视为裸 base64
    let qr_b64 = if qrcode.starts_with("data:") {
        qrcode.split_once(',').map(|(_, b)| b.to_string()).unwrap_or(qrcode)
    } else if qrcode.starts_with("http") {
        let img = http
            .get(&qrcode)
            .timeout(std::time::Duration::from_secs(15))
            .header("User-Agent", DOUBAO_UA)
            .send()
            .await
            .map_err(|e| format!("下载二维码失败：{e}"))?
            .bytes()
            .await
            .map_err(|e| format!("读二维码失败：{e}"))?;
        base64::engine::general_purpose::STANDARD.encode(&img)
    } else {
        qrcode
    };
    // 会话状态：token + csrf + 已有 cookies（base64url 打包，避免明文 cookie 出现在 state 里）
    let payload = json!({ "base": base, "token": token, "csrf": csrf, "cookies": cookies });
    let state = b64url_nopad(payload.to_string().as_bytes());
    Ok((state, qr_b64))
}

/// 轮询 check_qrconnect 一次：new=等待扫码 / scanned=已扫码待确认 / confirmed=逐跳收 Cookie /
/// expired=过期。单次解析返回，由前端 ~2s 自动轮询驱动（与设备码流一致）。
async fn doubao_qr_poll(http: &reqwest::Client, state_b64: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(state_b64)
        .map_err(|_| "会话状态损坏，请重新开始登录".to_string())?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "会话状态解析失败".to_string())?;
    let base = v
        .get("base")
        .and_then(|x| x.as_str())
        .unwrap_or(DOUBAO_BASE_URL)
        .to_string();
    let token = v.get("token").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let csrf = v.get("csrf").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut cookies: Vec<(String, String)> = v
        .get("cookies")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let k = c.get(0)?.as_str()?.to_string();
                    let val = c.get(1)?.as_str()?.to_string();
                    Some((k, val))
                })
                .collect()
        })
        .unwrap_or_default();
    if token.is_empty() || csrf.is_empty() {
        return Err("会话状态不完整，请重新开始登录".into());
    }
    let next = urlencode(&base);
    let cookie_str = cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let r = http
        .get(format!(
            "{base}/passport/web/check_qrconnect/?next={next}&token={token}&aid={DOUBAO_AID}"
        ))
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", DOUBAO_UA)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{base}/chat/login"))
        .header("Origin", &base)
        .header("x-tt-passport-csrf-token", &csrf)
        .header("Cookie", &cookie_str)
        .send()
        .await
        .map_err(|e| format!("轮询扫码状态失败：{e}"))?;
    let v: Value = r.json().await.map_err(|e| format!("扫码状态解析失败：{e}"))?;
    let d = v.get("data").cloned().unwrap_or(Value::Null);
    let error_code = d.get("error_code").and_then(|x| x.as_i64()).unwrap_or(0);
    let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
    if error_code != 0 || msg != "success" {
        if msg.contains("expired") || msg.contains("过期") {
            return Err("二维码已过期，请重新开始登录".into());
        }
        return Err(format!(
            "扫码接口异常：{}",
            truncate(&v.to_string(), 160)
        ));
    }
    let d = v.get("data").cloned().unwrap_or(Value::Null);
    let status = d.get("status").and_then(|x| x.as_str()).unwrap_or("");
    match status {
        "new" => Err("等待扫码：请用豆包 App 或手机扫码".to_string()),
        "scanned" => Err("已扫码：请在手机上确认登录".to_string()),
        "confirmed" => {
            let redirect_url = d.get("redirect_url").and_then(|x| x.as_str()).unwrap_or("");
            if redirect_url.is_empty() {
                return Err("扫码成功但缺少 redirect_url".into());
            }
            // 逐跳跟踪 redirect 收集 Set-Cookie（reqwest 默认自动跟随会吞掉中间跳转的 Cookie）。
            let no_redirect = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| format!("构建客户端失败：{e}"))?;
            let mut url = redirect_url.to_string();
            let mut last: Option<reqwest::Response> = None;
            for _hop in 0..10 {
                let mut req = no_redirect
                    .get(&url)
                    .header("User-Agent", DOUBAO_UA)
                    .header("Accept", "application/json, text/plain, */*");
                if !cookie_str.is_empty() {
                    req = req.header("Cookie", &cookie_str);
                }
                let resp = req
                    .send()
                    .await
                    .map_err(|e| format!("跟踪登录跳转失败：{e}"))?;
                let status = resp.status().as_u16();
                let location = resp
                    .headers()
                    .get("location")
                    .and_then(|x| x.to_str().ok())
                    .map(|s| s.to_string());
                // 收本跳 Set-Cookie 并入已有集合
                for (k, val) in parse_set_cookies(resp.headers()) {
                    if let Some(slot) = cookies.iter_mut().find(|(ek, _)| *ek == k) {
                        slot.1 = val;
                    } else {
                        cookies.push((k, val));
                    }
                }
                if (300..400).contains(&status) {
                    match location {
                        Some(loc) => {
                            // 相对 Location 用 urljoin 语义：基于当前 URL 解析
                            url = reqwest::Url::parse(&url)
                                .ok()
                                .and_then(|u| u.join(&loc).ok())
                                .map(|u| u.to_string())
                                .unwrap_or(loc);
                            continue;
                        }
                        None => break,
                    }
                }
                last = Some(resp);
                break;
            }
            let _ = last; // 终响应体不需要，Cookie 已收集
            let all = cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            if !all.contains("sessionid") {
                return Err("扫码成功，但跳转后没拿到 sessionid（Cookie 可能被分流）".into());
            }
            Ok(all)
        }
        "expired" => Err("二维码已过期，请重新开始登录".into()),
        other => Err(format!("未知扫码状态：{other}（请稍后重试）")),
    }
}

/// ══════════════════════════════════════════════════════════════
/// 库库 AI（百度）扫码登录：百度 passport 二维码 + BDUSS/PTOKEN + STOKEN 交换
/// 一手来源：kuku2api passport.mjs 与 logins.mjs
/// ══════════════════════════════════════════════════════════════

/// JSONP 回调名：bd__cbs__ + 7 位随机 base36。
fn kuku_jsonp_name() -> String {
    use rand::RngCore;
    const ALPH: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut b = [0u8; 7];
    rand::thread_rng().fill_bytes(&mut b);
    let mut s = String::from("bd__cbs__");
    for x in b {
        s.push(ALPH[(x as usize) % 36] as char);
    }
    s
}

/// 剥离 JSONP 外壳：取第一个 ( 与最后一个 ) 之间的 JSON 文本并解析。
fn unwrap_jsonp(text: &str) -> Option<serde_json::Value> {
    let open = text.find("(")?;
    let close = text.rfind(")")?;
    if close <= open {
        return None;
    }
    serde_json::from_str(text[open + 1..close].trim()).ok()
}

/// md5 hex（STOKEN 交换签名）。
fn kuku_md5_hex(s: &str) -> String {
    use md5::Digest;
    format!("{:x}", md5::Md5::digest(s.as_bytes()))
}

/// 启动库库扫码：seed BAIDUID → createQr → 下载二维码 → (state, 二维码 base64)。
async fn kuku_qr_start(http: &reqwest::Client, base: &str) -> Result<(String, String), String> {
    let home = http
        .get(base)
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", KUKU_UA)
        .header("Referer", KUKU_REFERER)
        .header("Accept", "application/json, text/plain, */*")
        .send()
        .await
        .map_err(|e| format!("访问 passport 首页失败：{e}"))?;
    let cookies = parse_set_cookies(home.headers());
    if !cookies.iter().any(|(k, _)| k == "BAIDUID") {
        return Err("passport 首页未下发 BAIDUID（可能被风控拦截）".into());
    }
    let cookie_str = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    // createQr：GET /v2/api/getqrcode（JSONP）→ sign
    let tt = now_millis();
    let cb = kuku_jsonp_name();
    let url = format!(
        "{base}/v2/api/getqrcode?lp=pc&qrloginfrom=pc&gid=&oauthLog=&apiver=v3&tpl={KUKU_TPL}&tt={tt}&callback={cb}"
    );
    let r = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", KUKU_UA)
        .header("Referer", KUKU_REFERER)
        .header("Accept", "application/json, text/plain, */*")
        .header("Cookie", &cookie_str)
        .send()
        .await
        .map_err(|e| format!("取二维码失败：{e}"))?;
    let text = r.text().await.map_err(|e| format!("读二维码响应失败：{e}"))?;
    let v = unwrap_jsonp(&text).ok_or_else(|| format!("getqrcode 非 JSONP：{}", truncate(&text, 160)))?;
    let errno = v
        .get("errno")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| v.get("errno").and_then(|x| x.as_i64()))
        .unwrap_or(-1);
    let sign = v.get("sign").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if errno != 0 || sign.is_empty() {
        return Err(format!("getqrcode 失败：{}", truncate(&v.to_string(), 160)));
    }
    // 下载二维码图片（刚生成立刻拉可能空体 → 重试 3 次）
    let img_url = format!("{base}/v2/api/qrcode?sign={}&lp=pc&qrloginfrom=pc", urlencode(&sign));
    let mut qr_bytes: Vec<u8> = Vec::new();
    for attempt in 1..=3u32 {
        let resp = http
            .get(&img_url)
            .timeout(std::time::Duration::from_secs(15))
            .header("User-Agent", KUKU_UA)
            .header("Referer", KUKU_REFERER)
            .header("Cookie", &cookie_str)
            .send()
            .await
            .map_err(|e| format!("下载二维码失败：{e}"))?;
        let bytes = resp.bytes().await.map_err(|e| format!("读二维码失败：{e}"))?;
        if !bytes.is_empty() {
            qr_bytes = bytes.to_vec();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250 * attempt as u64)).await;
    }
    if qr_bytes.is_empty() {
        return Err("二维码图片为空".into());
    }
    let qr_b64 = base64::engine::general_purpose::STANDARD.encode(&qr_bytes);
    let payload = json!({ "base": base, "sign": sign, "cookies": cookies });
    let state = b64url_nopad(payload.to_string().as_bytes());
    Ok((state, qr_b64))
}

/// 轮询扫码状态：unicast（JSONP）→ status：0=confirmed(v=ticket) / 1=scanned / 2=expired；
/// confirmed 后 bdusslogin 换真实 BDUSS/PTOKEN，再 v3/login/api/auth 换 STOKEN，返回整段 Cookie。
async fn kuku_qr_poll(http: &reqwest::Client, state_b64: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(state_b64)
        .map_err(|_| "会话状态损坏，请重新开始登录".to_string())?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "会话状态解析失败".to_string())?;
    let base = v
        .get("base")
        .and_then(|x| x.as_str())
        .unwrap_or(KUKU_PASSPORT_BASE)
        .to_string();
    let sign = v.get("sign").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut cookies: Vec<(String, String)> = v
        .get("cookies")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let k = c.get(0)?.as_str()?.to_string();
                    let val = c.get(1)?.as_str()?.to_string();
                    Some((k, val))
                })
                .collect()
        })
        .unwrap_or_default();
    if sign.is_empty() {
        return Err("会话状态不完整，请重新开始登录".into());
    }
    let cookie_str = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
    // 1) unicast 轮询一次
    let tt = now_millis();
    let cb = kuku_jsonp_name();
    let url = format!(
        "{base}/channel/unicast?channel_id={}&gid=&_sdkFrom=1&apiver=v3&tpl={KUKU_TPL}&tt={tt}&callback={cb}",
        urlencode(&sign)
    );
    // 百度 unicast 是**长轮询**：未扫码时挂起 ~30s 才回 {"errno":1}（实测 29.97–29.99s）。
    // 旧版给 15s 超时 → 每次都被客户端自己掐断，reqwest 报 "error sending request for url (…)"，
    // 于是「库库扫码」二维码能出、状态永远拿不到（功能等于不可用）。
    // 现在：超时抬到 35s（> 上游 30s 保持时间），并且**超时按「等待扫码」处理**——
    // 长轮询到点没消息是正常语义，不是故障。
    let r = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(35))
        .header("User-Agent", KUKU_UA)
        .header("Referer", KUKU_REFERER)
        .header("Accept", "application/json, text/plain, */*")
        .header("Cookie", &cookie_str)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "等待扫码：请用百度 App 扫一扫二维码".to_string()
            } else {
                format!("轮询扫码状态失败：{}", brief_net_err(&e))
            }
        })?;
    let text = r
        .text()
        .await
        .map_err(|e| format!("读扫码状态失败：{}", brief_net_err(&e)))?;
    let v = unwrap_jsonp(&text).ok_or_else(|| format!("unicast 非 JSONP：{}", truncate(&text, 160)))?;
    let errno = v
        .get("errno")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| v.get("errno").and_then(|x| x.as_i64()))
        .unwrap_or(-1);
    let ch_raw = v.get("channel_v").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let ch: Value = serde_json::from_str(&ch_raw).unwrap_or(Value::Null);
    let status = ch.get("status").and_then(|x| x.as_str()).unwrap_or("");
    if errno == 0 && status == "0" {
        let ticket = ch.get("v").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if ticket.is_empty() || ticket.len() != 32 {
            return Err(format!("扫码成功但 ticket 异常（{}）", truncate(&ch_raw, 120)));
        }
        // 2) bdusslogin：ticket → 真实 BDUSS/PTOKEN（Set-Cookie 落在响应头）
        let tt2 = now_millis();
        let cb2 = kuku_jsonp_name();
        let url2 = format!(
            "{base}/v2/api/bdusslogin?bduss={}&u={}&qrcode=1&apiver=v3&tpl={KUKU_TPL}&tt={tt2}&callback={cb2}",
            urlencode(&ticket),
            urlencode(KUKU_U)
        );
        let r2 = http
            .get(&url2)
            .timeout(std::time::Duration::from_secs(15))
            .header("User-Agent", KUKU_UA)
            .header("Referer", KUKU_REFERER)
            .header("Accept", "application/json, text/plain, */*")
            .header("Cookie", &cookie_str)
            .send()
            .await
            .map_err(|e| format!("BDUSS 交换失败：{}", brief_net_err(&e)))?;
        let set_cookies = parse_set_cookies(r2.headers());
        let bduss_text = r2.text().await.map_err(|e| format!("读 BDUSS 响应失败：{e}"))?;
        let bduss_v = unwrap_jsonp(&bduss_text).ok_or_else(|| format!("bdusslogin 非 JSONP：{}", truncate(&bduss_text, 160)))?;
        // 成功响应 {data,errInfo} 无 errno 字段；拒绝才带 errno（假 ticket → errno -9999）
        let be = bduss_v
            .get("errno")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<i64>().ok())
            .or_else(|| bduss_v.get("errno").and_then(|x| x.as_i64()));
        if be != None && be != Some(0) {
            return Err(format!("BDUSS 交换被拒：{}", truncate(&bduss_v.to_string(), 160)));
        }
        for (k, val) in set_cookies {
            if let Some(slot) = cookies.iter_mut().find(|(ek, _)| *ek == k) {
                slot.1 = val;
            } else {
                cookies.push((k, val));
            }
        }
        let bduss = cookies.iter().find(|(k, _)| k == "BDUSS" || k == "BDUSS_BFESS").map(|(_, vv)| vv.clone()).unwrap_or_default();
        let ptoken = cookies.iter().find(|(k, _)| k == "PTOKEN" || k == "PTOKEN_BFESS").map(|(_, vv)| vv.clone()).unwrap_or_default();
        if bduss.is_empty() || ptoken.is_empty() {
            return Err("登录成功但未拿到 BDUSS/PTOKEN（Set-Cookie 缺失）".into());
        }
        // 3) STOKEN 交换：POST /v3/login/api/auth（form + md5 签名）
        let signed = format!(
            "appid=1&bduss={bduss}&ptoken={ptoken}&return_type=1&tpl={KUKU_TPL}&tpl_list={KUKU_TPL}|netdisk&sign_key={KUKU_ENGINE_SIGN_KEY}"
        );
        let sig = kuku_md5_hex(&signed);
        let body = format!(
            "appid=1&bduss={bduss}&ptoken={ptoken}&return_type=1&tpl={KUKU_TPL}&tpl_list={KUKU_TPL}|netdisk&sig={sig}"
        );
        let st_cookie = if cookies.iter().any(|(k, _)| k == "BAIDUID") {
            format!("{cookie_str}; gfprotpl=genflowpro")
        } else {
            cookie_str.clone()
        };
        let r3 = http
            .post(format!("{base}/v3/login/api/auth"))
            .timeout(std::time::Duration::from_secs(20))
            .header("User-Agent", KUKU_ENGINE_UA)
            .header("Referer", KUKU_REFERER)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Cookie", &st_cookie)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("STOKEN 交换失败：{}", brief_net_err(&e)))?;
        let st: Value = r3.json().await.map_err(|e| format!("读 STOKEN 响应失败：{e}"))?;
        let se = st.get("errno").and_then(|x| x.as_i64()).unwrap_or(-1);
        let stoken = st
            .get("stoken_list")
            .and_then(|s| s.get("genflowpro"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if se != 0 || stoken.is_empty() {
            return Err(format!("STOKEN 交换失败：{}", truncate(&st.to_string(), 160)));
        }
        Ok(format!("BDUSS={bduss}; STOKEN={stoken}"))
    } else if errno == 0 && status == "1" {
        Err("已扫码：请在手机上确认登录".to_string())
    } else if errno == 0 && status == "2" {
        Err("二维码已过期，请重新开始登录".to_string())
    } else {
        Err("等待扫码：请用百度 App 扫一扫二维码".to_string())
    }
}

// ═════════════════════════════════════════════════════════════
// Loomy（讯飞）微信扫码登录
// 一手来源：_refs/Ebony-Vinyl-dsh-our-free-model/vendor/channel-pack/src/
//   loomy-wechat.ts（qrconnect → 二维码 → 长轮询）
//   loomy-oauth.ts（bind/auth → bind/skip → session，HMAC-SHA1 签名）
//   loomy-sign.ts（9 段待签串，Authorization: account AK:sig）
//   loomy-product.ts（appId=GM3LOOMY、accountBase、AK）
// ══════════════════════════════════════════════════════════════

/// 微信开放平台 AppID（Loomy 官方登录页写死）。
const LOOMY_WECHAT_APPID: &str = "wx18d60be432287cf8";
/// redirect_uri **必须**是官方回调：换本地/其它域名微信直接回「参数错误」页。
/// 它只用于取 uuid —— code 由长轮询直接拿到，不经过这个回调页（实测 404）。
const LOOMY_WECHAT_REDIRECT: &str = "https://loomy.xunfei.cn/oauth/wechat/callback";
/// 微信授权页 / 二维码 / 长轮询的默认基址。
const LOOMY_WECHAT_BASE: &str = "https://open.weixin.qq.com";
const LOOMY_WECHAT_LONG_BASE: &str = "https://long.open.weixin.qq.com";
/// 讯飞账号（CAccount）基址：微信 code 换 session 走这里。
const LOOMY_ACCOUNT_BASE: &str = "https://account.xfinfr.com";
/// 讯飞 appId（账号请求体 base.appid）。
const LOOMY_APPID: &str = "GM3LOOMY";
/// 成功业务码（其余一律失败；Loomy 失败也是 HTTP 200，只能读 code）。
const LOOMY_OK_CODE: &str = "000000";
const LOOMY_WECHAT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 从微信授权页 HTML 提取二维码 uuid（主正则 /connect/qrcode/，兜底 l/qrconnect?uuid=）。
fn loomy_wechat_uuid(html: &str) -> Option<String> {
    for mark in ["/connect/qrcode/", "l/qrconnect?uuid="] {
        if let Some(pos) = html.find(mark) {
            let rest = &html[pos + mark.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '=' || c == '+' || c == '/'))
                .unwrap_or(rest.len());
            let u = &rest[..end];
            if u.len() >= 6 && u.len() <= 64 {
                return Some(u.to_string());
            }
        }
    }
    None
}

/// 图片魔数识别（微信二维码实测 JPEG；png/gif 兼容）。
fn loomy_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

/// 从 JS 片段取裸数字（wx_errcode = 408;）。
fn loomy_wx_number(body: &str, key: &str) -> Option<i64> {
    let i = body.find(key)?;
    let rest = &body[i + key.len()..];
    let start = rest.find(|c: char| c.is_ascii_digit())?;
    let tail = &rest[start..];
    let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
    tail[..end].parse::<i64>().ok()
}

/// 从 JS 片段取引号字符串（wx_code = 'xxxx';）。
fn loomy_wx_quoted(body: &str, key: &str) -> String {
    let Some(i) = body.find(key) else {
        return String::new();
    };
    let rest = &body[i + key.len()..];
    let Some(eq) = rest.find('=') else {
        return String::new();
    };
    let tail = rest[eq + 1..].trim_start();
    let tail = tail
        .strip_prefix('\u{27}')
        .or_else(|| tail.strip_prefix('\u{22}'))
        .unwrap_or(tail);
    let end = tail
        .find(|c: char| c == '\u{27}' || c == '\u{22}' || c == ';')
        .unwrap_or(tail.len());
    tail[..end].trim().to_string()
}

/// 调讯飞账号端点（HMAC-SHA1 签名，Authorization: account AK:sig），返回信封 data。
async fn loomy_account_post(
    http: &reqwest::Client,
    account_base: &str,
    path: &str,
    param: Value,
) -> Result<Value, String> {
    // body 只序列化一次：签名与发送必须逐字节一致（二次序列化会改键序导致签名失效）。
    let body = json!({
        "base": {
            "appid": LOOMY_APPID,
            "modelid": "Web",
            "version": "1.0.0",
            "devid": "web",
            // 客户端在 Windows 上发的也是这个值，照抄不要改。
            "ua": "Loomy|Desktop|Electron|macOS",
            "traceid": rand_hex(16),
        },
        "param": param,
    })
    .to_string();
    let headers = aigw_crypto::loomy::build_headers(
        aigw_crypto::loomy::DEFAULT_ACCESS_KEY_ID,
        aigw_crypto::loomy::DEFAULT_ACCESS_KEY_SECRET,
        "POST",
        path,
        &body,
        &[],
    );
    let mut req = http
        .post(format!("{account_base}{path}"))
        .timeout(std::time::Duration::from_secs(20))
        .body(body);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("讯飞账号请求失败（{path}）：{e}"))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读讯飞账号响应失败（{path}）：{e}"))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|_| format!("讯飞账号响应不是 JSON（{path}）：{}", truncate(&text, 160)))?;
    let code = v.get("code").and_then(|c| c.as_str()).unwrap_or("");
    if code != LOOMY_OK_CODE {
        let desc = v
            .get("desc")
            .and_then(|d| d.as_str())
            .or_else(|| v.get("message").and_then(|d| d.as_str()))
            .unwrap_or("");
        return Err(if desc.is_empty() {
            format!("讯飞账号接口失败（{path}，code={code}）")
        } else {
            format!("讯飞账号接口失败：{desc}")
        });
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

/// 密码登录用的 RSA 公钥接口。零副作用，先拿 key 再加密密码。
pub async fn loomy_get_pukey(http: &reqwest::Client) -> Result<(String, String), String> {
    let d = loomy_account_post(http, LOOMY_ACCOUNT_BASE, "/login/account/getPuKey", json!({})).await?;
    let pukey = d.get("pukey").and_then(|x| x.as_str()).unwrap_or("");
    let rcode = d.get("rcode").and_then(|x| x.as_str()).unwrap_or("");
    if pukey.is_empty() {
        return Err("讯飞账号没有返回公钥（getPuKey 返回空）。可能是该接口改版或被风控；".into());
    }
    Ok((pukey.to_string(), rcode.to_string()))
}

/// 密码登录：`getPuKey` 拿公钥与 nonce → RSA/PKCS#1 v1.5 加密密码 → `byPwd` 换 session。
///
/// 与短信登录相比，这条链路是**全自动**的：`rcode` 是服务端 nonce 而不是验证码，
/// 所以不需要人读码，可以用来定时续期（session 14 天过期时后台自己续）。
/// 上游要求密码按 PKCS#1 v1.5 加密后 base64 —— 与参考实现 `loomy2api/crypto.py` 一致。
pub async fn loomy_login_password(
    http: &reqwest::Client,
    loginid: &str,
    password: &str,
) -> Result<String, String> {
    let loginid = loginid.trim();
    if loginid.is_empty() || password.is_empty() {
        return Err("账号和密码都要填；".into());
    }
    let (pukey, rcode) = loomy_get_pukey(http).await?;
    let encrypted = rsa_encrypt_password(&pukey, password)?;
    let d = loomy_account_post(
        http,
        LOOMY_ACCOUNT_BASE,
        "/login/account/byPwd",
        json!({
            "loginid": loginid,
            "password": encrypted,
            "rcode": rcode,
            "type": 1,
            // 14 天：与参考实现的 SESSION_EXPIRE_SECONDS 一致。
            "expire": 14 * 24 * 3600,
        }),
    )
    .await?;
    let session = d.get("session").and_then(|x| x.as_str()).unwrap_or("");
    if session.is_empty() {
        return Err("讯飞账号接口没有返回 session（账号或密码不对？）；".into());
    }
    Ok(session.to_string())
}

/// RSA/PKCS#1 v1.5 加密 → base64。上游给的是 1024 位 DER 公钥。
fn rsa_encrypt_password(pubkey_der_b64: &str, plaintext: &str) -> Result<String, String> {
    use base64::Engine as _;
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;
    use rsa::traits::PublicKeyParts;
    use rsa::RsaPublicKey;
    let der = base64::engine::general_purpose::STANDARD
        .decode(pubkey_der_b64.trim())
        .map_err(|e| format!("公钥不是合法 base64：{e}"))?;
    // 上游给的是 PKCS#1 RSAPublicKey（'BEGIN RSA PUBLIC KEY'），但也兜住 SPKI 形态。
    let key = RsaPublicKey::from_pkcs1_der(&der)
        .or_else(|_| RsaPublicKey::from_public_key_der(&der))
        .map_err(|e| format!("公钥解析失败（{} 字节）：{e}", der.len()))?;
    let msg = plaintext.as_bytes();
    let k = key.size();
    if msg.len() > k.saturating_sub(11) {
        return Err(format!("密码太长：{} 字节，RSA-{} 最多 {} 字节", msg.len(), k * 8, k - 11));
    }
    let mut rng = rand::thread_rng();
    let enc = key
        .encrypt(&mut rng, rsa::Pkcs1v15Encrypt, msg)
        .map_err(|e| format!("密码加密失败：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(enc))
}

/// 微信 code → bind/auth 拿 rcode → 已绑手机号则 bind/skip 换 32 位 session。
/// 手机号归一化（对齐 aigw-panel-rs 的 loomy_auth::norm_phone）。
///
/// 用户在表单里会写成 `138 0013 8000` / `+8613800138000` / `0086...`，
/// 上游只认 11 位纯数字。校验规则：1 开头、第二位 3-9、其余全数字。
pub fn norm_phone(phone: &str) -> Result<String, String> {
    let cleaned: String = phone
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')' | '（' | '）'))
        .collect();
    let s = if let Some(r) = cleaned.strip_prefix("+86") {
        r.to_string()
    } else if let Some(r) = cleaned.strip_prefix("0086") {
        r.to_string()
    } else if cleaned.starts_with("86") && cleaned.len() == 13 {
        cleaned[2..].to_string()
    } else {
        cleaned
    };
    let bytes = s.as_bytes();
    let ok = bytes.len() == 11
        && bytes[0] == b'1'
        && (b'3'..=b'9').contains(&bytes[1])
        && bytes[2..].iter().all(|b| b.is_ascii_digit());
    if ok {
        Ok(s)
    } else {
        Err(format!(
            "手机号格式不对（{}）。写成 11 位国内手机号即可，例如 13800138000；",
            truncate(phone, 24)
        ))
    }
}

/// Loomy（讯飞）短信登录第一步：发验证码 → msgid。
///
/// 纯协议，不借浏览器：签名与扫码那条完全同一套（`loomy_account_post`）。
/// 用户拿到手机短信后，把 4-6 位验证码交给 `loomy_verify_sms` 完成。
pub async fn loomy_send_sms(http: &reqwest::Client, phone: &str) -> Result<String, String> {
    let ph = norm_phone(phone)?;
    let d = loomy_account_post(
        http,
        LOOMY_ACCOUNT_BASE,
        "/login/phone/sendMsgCode",
        json!({"ccode": "86", "phone": ph, "expire": 300}),
    )
    .await?;
    let msgid = d.get("msgid").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if !msgid.is_empty() {
        return Ok(msgid);
    }
    Err(format!(
        "讯飞账号没有返回 msgid（{}）。多数是号码未注册讯飞账号，或该号段不支持；",
        truncate(&d.to_string(), 180)
    ))
}

/// Loomy 短信登录第二步：验证码 → 32 位 session（14 天有效）。
///
/// 备注：讯飞接口里 `expire` 指的是 session 有效期（秒），不是验证码有效期 ——
/// 验证码本身由上游管（约 5 分钟）。这里与参考实现保持一致写 14 天。
pub async fn loomy_verify_sms(
    http: &reqwest::Client,
    phone: &str,
    code: &str,
    msgid: &str,
) -> Result<String, String> {
    let ph = norm_phone(phone)?;
    let code = code.trim();
    if code.is_empty() || msgid.is_empty() {
        return Err("缺验证码或 msgid（先点「发送验证码」拿到 msgid，再填验证码）".into());
    }
    if !code.chars().all(|c| c.is_ascii_digit()) || code.len() > 8 {
        return Err("验证码应该是几位数字".into());
    }
    let d = loomy_account_post(
        http,
        LOOMY_ACCOUNT_BASE,
        "/login/phone/checkCode",
        json!({"ccode": "86", "phone": ph, "mcode": code, "msgid": msgid, "expire": 14 * 24 * 3600}),
    )
    .await?;
    let session = d.get("session").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if session.is_empty() {
        return Err(format!(
            "讯飞账号没有返回 session（{}）。验证码可能已过期或输错；",
            truncate(&d.to_string(), 180)
        ));
    }
    Ok(session)
}

///
/// bind=0（这个微信没绑过讯飞账号）时必须先绑手机号才能完成，
/// 纯扫码拿不到 session —— 此时明确报错让用户先去官网绑定，不静默失败。
async fn loomy_bind_wechat(
    http: &reqwest::Client,
    account_base: &str,
    wx_code: &str,
) -> Result<String, String> {
    let auth = loomy_account_post(
        http,
        account_base,
        "/login/thirdAccount/bind/auth",
        json!({"tcode": {"code": wx_code}, "type": "wx"}),
    )
    .await?;
    let rcode = auth
        .get("rcode")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if rcode.is_empty() {
        return Err("微信授权响应缺少 rcode，请重新扫码".into());
    }
    let bind = auth.get("bind").and_then(|v| v.as_i64()).unwrap_or(0);
    if bind != 1 {
        return Err("该微信尚未绑定讯飞账号：请先在 loomy.xunfei.cn 用手机号登录并绑定微信，然后重新扫码（也可直接粘贴 32 位 session）".into());
    }
    let data = loomy_account_post(
        http,
        account_base,
        "/login/thirdAccount/bind/skip",
        json!({"rcode": rcode}),
    )
    .await?;
    let session = data
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if session.is_empty() {
        return Err("微信登录成功但未拿到 session，请重试".into());
    }
    Ok(session)
}

/// 发起 Loomy 微信扫码：(state 载荷, 二维码 base64, mime)。
async fn loomy_qr_start(
    http: &reqwest::Client,
    wechat_base: &str,
    long_base: &str,
    account_base: &str,
) -> Result<(String, String, String), String> {
    let auth_url = format!(
        "{wechat_base}/connect/qrconnect?appid={LOOMY_WECHAT_APPID}&redirect_uri={}&response_type=code&scope=snsapi_login&state=loomy#wechat_redirect",
        urlencode(LOOMY_WECHAT_REDIRECT)
    );
    let page = http
        .get(&auth_url)
        .timeout(std::time::Duration::from_secs(20))
        .header("User-Agent", LOOMY_WECHAT_UA)
        .header("Referer", "https://open.weixin.qq.com/")
        .send()
        .await
        .map_err(|e| format!("打开微信授权页失败：{e}"))?;
    let html = page
        .text()
        .await
        .map_err(|e| format!("读微信授权页失败：{e}"))?;
    let uuid = loomy_wechat_uuid(&html).ok_or_else(|| {
        format!(
            "微信授权页未找到二维码 uuid（{} 字节，多半是 redirect_uri 被拒）",
            html.len()
        )
    })?;
    let qr = http
        .get(format!("{wechat_base}/connect/qrcode/{uuid}"))
        .timeout(std::time::Duration::from_secs(20))
        .header("User-Agent", LOOMY_WECHAT_UA)
        .header("Referer", "https://open.weixin.qq.com/")
        .send()
        .await
        .map_err(|e| format!("下载微信二维码失败：{e}"))?;
    let bytes = qr
        .bytes()
        .await
        .map_err(|e| format!("读微信二维码失败：{e}"))?;
    if bytes.len() < 200 {
        return Err(format!(
            "微信二维码过小（{} 字节），多半是错误页",
            bytes.len()
        ));
    }
    let mime = loomy_image_mime(&bytes).ok_or_else(|| "微信二维码不是已知图片格式".to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let payload = json!({"uuid": uuid, "long_base": long_base, "account_base": account_base});
    Ok((
        b64url_nopad(payload.to_string().as_bytes()),
        b64,
        mime.to_string(),
    ))
}

/// 轮询微信扫码状态；405 拿到 wx_code 后立即换 session。
async fn loomy_qr_poll(http: &reqwest::Client, state_b64: &str) -> Result<String, String> {
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(state_b64)
        .map_err(|_| "会话状态损坏，请重新开始登录".to_string())?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "会话状态解析失败".to_string())?;
    let uuid = v.get("uuid").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let long_base = v
        .get("long_base")
        .and_then(|x| x.as_str())
        .unwrap_or(LOOMY_WECHAT_LONG_BASE)
        .to_string();
    let account_base = v
        .get("account_base")
        .and_then(|x| x.as_str())
        .unwrap_or(LOOMY_ACCOUNT_BASE)
        .to_string();
    if uuid.is_empty() {
        return Err("会话状态不完整，请重新开始登录".into());
    }
    let url = format!(
        "{long_base}/connect/l/qrconnect?uuid={}&_={}",
        urlencode(&uuid),
        now_millis()
    );
    // 微信长轮询在状态不变时会挂到约 25s。这里主动 5s 断开：前端每 2s 轮一次，
    // 5s 超时既能把「已扫码/已确认」的延迟控制在 5s 内，又不会让 /api/auth 长时间占着连接。
    let resp = http
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .header("User-Agent", LOOMY_WECHAT_UA)
        .header("Referer", "https://open.weixin.qq.com/")
        .send()
        .await;
    let body = match resp {
        Ok(r) => r.text().await.unwrap_or_default(),
        // 微信长轮询本身约 25s 才返回；我们主动短超时后按「还没变化」处理，
        // 避免面板的 /api/auth 请求被挂住（前端 2s 一轮）。
        Err(e) if e.is_timeout() => return Err("等待扫码：请用微信扫一扫二维码".into()),
        Err(e) => return Err(format!("轮询扫码状态失败：{e}")),
    };
    let errcode = loomy_wx_number(&body, "wx_errcode");
    match errcode {
        Some(405) => {
            let wx_code = loomy_wx_quoted(&body, "wx_code");
            if wx_code.trim().is_empty() {
                // 405 但没 code 是异常帧：保守当「已扫码」，绝不拿空 code 去换。
                return Err("已扫码：请在手机上确认登录".into());
            }
            loomy_bind_wechat(http, &account_base, &wx_code).await
        }
        Some(404) => Err("已扫码：请在手机上确认登录".into()),
        Some(403) => Err("已取消登录，请重新扫码".into()),
        Some(402) => Err("二维码已失效，请重新开始登录".into()),
        _ => Err("等待扫码：请用微信扫一扫二维码".into()),
    }
}

// ═════════ 小浣熊（商汤）扫码登录 ═════════
// 一手：_refs/Ebony-Vinyl-dsh-our-free-model/vendor/channel-pack/src/raccoon.ts
// 关键点：code 是**客户端自造**的（randomBytes(16).toString('hex')），上游只把它当登记键；
// 微信小程序扫码后把同一个 code 报给上游，本端再用它轮询换 access_token。
/// 生成 (code, 二维码文本)。**不联网**：code 不需要上游预登记。
fn raccoon_qr_start(base: &str) -> (String, String) {
    let code = rand_hex(16);
    let url = format!("{base}/login/mp?code={code}&appname={RACCOON_QR_APPNAME}");
    (code, url)
}

/// 轮询小浣熊扫码状态；成功后返回 Bearer access_token。
///
/// 一手实现里「任何异常一律降级 pending」——这里对**网络错误**仍然报错（面板要能看出是网络问题），
/// 但对业务状态一律按 pending / logging / canceled / success 四态映射。
async fn raccoon_qr_poll(
    http: &reqwest::Client,
    base: &str,
    code: &str,
) -> Result<String, String> {
    let resp = http
        .post(format!("{base}/api/web/auth/v1/login_with_qrcode_code"))
        .timeout(std::time::Duration::from_secs(15))
        .header("User-Agent", RACCOON_UA)
        .header("X-Client-Platform", RACCOON_CLIENT_PLATFORM)
        .header("X-Client-Version", RACCOON_CLIENT_VERSION)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{base}/"))
        .json(&json!({ "qrcode_code": code }))
        .send()
        .await
        .map_err(|e| format!("轮询扫码状态失败：{}", brief_net_err(&e)))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!(
            "轮询扫码状态失败：HTTP {status} {}",
            text.chars().take(160).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        format!(
            "扫码状态返回异常：{}",
            text.chars().take(160).collect::<String>()
        )
    })?;
    // 信封 {code,message,details,data}：code === 0 才算成功
    let biz = v.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    if biz != 0 {
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("上游拒绝");
        return Err(format!("扫码登录被拒：{msg}（code={biz}）"));
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let st = data
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match st.as_str() {
        "success" => {
            let tok = data
                .get("access_token")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if tok.is_empty() {
                // 一手实现同样处理：token 空 = 其实还没好，绝不能让空串覆盖账号池里的旧凭据
                Err("等待扫码：请用微信扫一扫二维码（打开商汤小浣熊小程序）".to_string())
            } else {
                Ok(tok)
            }
        }
        "logging" => Err("已扫码：请在手机上确认登录".to_string()),
        "canceled" | "cancelled" => {
            Err("已取消登录，请重新生成二维码".to_string())
        }
        _ => Err("等待扫码：请用微信扫一扫二维码（打开商汤小浣熊小程序）".to_string()),
    }
}

