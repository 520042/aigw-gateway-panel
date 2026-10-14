//! 鉴权中间件：对齐 Python main.py —— /api/* 需 X-AIGW-Token（本机回环且未设 panel_token 时免验证）；
//! 跨站 Origin/Referer 一律拒绝（只回显本机来源的 CORS）。
use crate::server::state::AppState;
use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http::HeaderMap;
use serde_json::json;

pub fn local_hosts() -> [&'static str; 3] {
    ["127.0.0.1", "localhost", "::1"]
}

fn host_of(hdr: &str) -> Option<String> {
    let h = hdr.trim();
    if h.is_empty() {
        return None;
    }
    if let Ok(u) = url_parse(h) {
        return Some(u);
    }
    None
}

/// 极简 URL host 解析（避免引入额外依赖）。
pub fn url_parse(s: &str) -> Result<String, ()> {
    let s = if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("http://{}", s)
    };
    let host = s
        .split("://")
        .nth(1)
        .ok_or(())?
        .split('/')
        .next()
        .ok_or(())?
        .split(':')
        .next()
        .ok_or(())?
        .to_string();
    if host.is_empty() {
        Err(())
    } else {
        Ok(host)
    }
}

pub fn origin_ok(headers: &HeaderMap) -> bool {
    for name in ["origin", "referer"] {
        if let Some(v) = headers.get(name) {
            if let Ok(s) = v.to_str() {
                if let Some(host) = host_of(s) {
                    return local_hosts().contains(&host.as_str());
                }
            }
            return false;
        }
    }
    true
}

/// 从请求头提取面板令牌（X-AIGW-Token / Authorization: Bearer / Cookie: aigw_token=）。
pub fn extract_token(headers: &HeaderMap, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    let mut given = headers
        .get("x-aigw-token")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    if given.is_empty() {
        if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
            if let Some(t) = auth.to_ascii_lowercase().strip_prefix("bearer ") {
                given = t.trim().to_string();
            }
        }
    }
    if given.is_empty() {
        if let Some(ck) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
            for part in ck.split(';') {
                let part = part.trim();
                if let Some(rest) = part.strip_prefix("aigw_token=") {
                    if let Ok(decoded) = url_decode(rest) {
                        given = decoded;
                        break;
                    }
                }
            }
        }
    }
    given == token
}

fn url_decode(s: &str) -> Result<String, ()> {
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8 as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    Ok(out)
}

/// 跨站拒绝响应。
pub fn cross_origin_response() -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(json!({
            "ok": false, "code": "cross_origin_denied",
            "message": "跨站请求已拒绝（面板只接受本机来源）"
        })),
    )
        .into_response()
}

pub fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({
            "ok": false, "code": "unauthorized",
            "message": "缺少或错误的 X-AIGW-Token（见 设置→面板令牌）"
        })),
    )
        .into_response()
}

/// 中间件：校验 origin 与 /api 鉴权。
pub async fn auth_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let (token, need) = {
        let s = st.inner.settings.read().await;
        let token_set = !s.panel_token.is_empty();
        let loopback = local_hosts().contains(&s.bind_host.as_str());
        (s.panel_token.clone(), token_set || !loopback)
    };
    if !origin_ok(req.headers()) {
        return cross_origin_response();
    }
    if need && !extract_token(req.headers(), &token) {
        return unauthorized_response();
    }
    next.run(req).await
}
