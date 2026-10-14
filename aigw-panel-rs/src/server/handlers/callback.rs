//! OAuth 回调：登录页授权后带 `code+iss` 重定向回面板，这里换取会话/token 并入库。
//!
//! 契约（逆向自 workbuddy 登录页 auth-v2 bundle）：
//!   POST {origin}/auth-v2/realms/copilot/signin/session
//!   body: {code, issuer, redirect_uri}
//! 响应里含 session / token 类字段（首跑全量记录响应，据实解析）。
use crate::server::state::AppState;
use axum::{extract::{Query, State}, http::header, response::{IntoResponse, Response}};
use serde_json::{json, Value};
use std::collections::HashMap;

pub async fn auth_callback(
    State(st): State<AppState>,
    q: Query<HashMap<String, String>>,
) -> Response {
    let code = q.get("code").cloned().unwrap_or_default();
    let issuer = q.get("iss").cloned().unwrap_or_default();
    let state = q.get("state").cloned().unwrap_or_default();
    if code.is_empty() {
        return html_page("授权回调缺少 code", "登录页未带回授权码，请重试跳转登录。");
    }

    let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", super::api::port_of_panel());
    // 国内站登录页 origin = www.workbuddy.cn（国际站 workbuddy.ai，回调里据 iss 判）
    let origin = if issuer.contains("workbuddy.ai") || issuer.contains("codebuddy.ai") {
        "https://www.workbuddy.ai"
    } else {
        "https://www.workbuddy.cn"
    };
    let url = format!("{}/auth-v2/realms/copilot/signin/session", origin);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(20))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .json(&json!({"code": code, "issuer": issuer, "redirect_uri": redirect_uri}))
        .send()
        .await;

    let (status, body) = match resp {
        Ok(r) => (r.status().as_u16(), r.text().await.unwrap_or_default()),
        Err(e) => (0, format!("exchange 请求失败：{}", e)),
    };
    tracing::info!(status, body = %body.chars().take(1200).collect::<String>(), "auth-v2 signin/session 响应");

    // 尽力解析 token（字段名以真实响应为准，递归找 accessToken/access_token/token）
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let token = find_token(&parsed);

    match token {
        Some(tok) if !tok.is_empty() => {
            st.inner.login.add_account("wb-gateway", "wb-gateway（自动授权）", "token", &tok);
            // 完成所有 wb 族在途会话（poll → success → 前端自动收尾）
            for s in st.inner.login.pending_sessions() {
                if s.starts_with("wb-gateway") {
                    let _ = st.inner.login.complete(&s, "自动授权成功，token 已入库");
                }
            }
            html_page(
                "授权成功",
                "已拿到 accessToken 并写入账号池。可以关闭本标签页，回到面板查看（卡片应显示已接入）。",
            )
        }
        _ => html_page(
            "授权交换未拿到 token",
            &format!(
                "HTTP {}。原始响应已记录到面板日志，请把这段发给助手继续分析：<br><code>{}</code>",
                status,
                body.chars().take(600).collect::<String>().replace('<', "&lt;")
            ),
        ),
    }
}

static PANEL_PORT_LOCK: u16 = 8800;
#[allow(dead_code)]
fn port_lock_unused() -> u16 { PANEL_PORT_LOCK }

fn find_token(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.len() > 40 && (s.starts_with("eyJ") || s.contains('.')) => {
            Some(s.clone())
        }
        Value::Object(m) => {
            for k in ["accessToken", "access_token", "token", "session_token"] {
                if let Some(t) = m.get(k).and_then(|x| x.as_str()) {
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
            m.values().find_map(find_token)
        }
        Value::Array(a) => a.iter().find_map(find_token),
        _ => None,
    }
}

fn html_page(title: &str, body: &str) -> Response {
    let html = format!(
        "<!DOCTYPE html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>{}</title>\
         <style>body{{font-family:system-ui;background:#f5f6fa;display:flex;align-items:center;\
         justify-content:center;height:100vh;margin:0}}.c{{background:#fff;border-radius:12px;\
         padding:32px 40px;box-shadow:0 4px 24px rgba(0,0,0,.08);max-width:640px}}\
         h1{{font-size:18px;margin:0 0 12px}}code{{white-space:pre-wrap;word-break:break-all;\
         font-size:12px}}</style></head><body><div class=\"c\"><h1>{}</h1><div>{}</div>\
         <p style=\"margin-top:16px\"><a href=\"http://127.0.0.1:{}/\">返回面板</a></p></div></body></html>",
        title,
        title,
        body,
        super::api::port_of_panel()
    );
    let mut resp = (axum::http::StatusCode::OK, html).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}
