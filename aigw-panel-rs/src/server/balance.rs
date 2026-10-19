//! 平台账号积分/余额通用刷新。
//! 每个平台在 catalog.gwextra.actions 里声明 `points_balance` 动作，
//! 后端按该动作实时拉取并解析余额，写回账号池的 `balance` 字段。

use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 从 JSON 里抠出一个看起来像余额的值（递归找 balance/points/credits/remain 等键）。
pub fn extract_balance_value(j: &Value) -> Option<String> {
    // ① 资源包数组求和（wb 系形状）：Accounts[] 里所有 CapacityUnit==credits 的
    //    CapacityRemain 相加。★ 2026-10-15 用户纠正：只取第一条会把 500 当总余额，
    //    实测 47 条资源包合计 6993（与响应聚合字段 TotalDosage 一致，口径互验）。
    if let Some(sum) = sum_resource_accounts(j) {
        return Some(sum);
    }
    // ② 聚合字段（WPS total_balance 等）
    for k in ["TotalDosage", "total_balance", "totalBalance", "TotalBalance"] {
        if let Some(v) = find_key(j, k) {
            if let Some(s) = as_balance_str(v) {
                return Some(s);
            }
        }
    }
    // ③ 单值递归（其它平台形状）
    let keys = [
        "CapacityRemain",
        "capacityRemain",
        "balance",
        "points",
        "credits",
        "score",
        "remain",
        "pkgRemain",
        "pkg_remain",
        "total_remain",
        "left",
        "available",
        "usable",
    ];
    for k in keys {
        if let Some(v) = find_key(j, k) {
            if let Some(s) = as_balance_str(v) {
                return Some(s);
            }
        }
    }
    // 兜底：扫描键名包含 balance/points/credits/remain 的对象字段
    find_balance_like(j)
}

/// wb 系：递归找 `Accounts` 数组，把 CapacityUnit==credits 的 CapacityRemain 求和。
pub fn sum_resource_accounts(j: &Value) -> Option<String> {
    let arr = find_key(j, "Accounts")?.as_array()?;
    let mut sum: i64 = 0;
    let mut any = false;
    for it in arr {
        let Some(obj) = it.as_object() else { continue };
        let unit = obj.get("CapacityUnit").and_then(|v| v.as_str()).unwrap_or("");
        // 只统计 credits 单位（并发/次数类不计入）；单位缺失时也计入（保守）
        if !unit.is_empty() && !unit.eq_ignore_ascii_case("credits") {
            continue;
        }
        if let Some(cr) = obj.get("CapacityRemain").and_then(|v| v.as_i64()) {
            sum += cr;
            any = true;
        }
    }
    if any { Some(sum.to_string()) } else { None }
}

pub fn find_key<'v>(j: &'v Value, key: &str) -> Option<&'v Value> {
    match j {
        Value::Object(m) => {
            if let Some(v) = m.get(key) {
                return Some(v);
            }
            for (_, v) in m {
                if let Some(r) = find_key(v, key) {
                    return Some(r);
                }
            }
            None
        }
        Value::Array(a) => {
            for v in a {
                if let Some(r) = find_key(v, key) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn find_balance_like(j: &Value) -> Option<String> {
    match j {
        Value::Object(m) => {
            for (k, v) in m {
                let kl = k.to_lowercase();
                if kl.contains("balance")
                    || kl.contains("points")
                    || kl.contains("credits")
                    || kl.contains("remain")
                    || kl.contains("score")
                {
                    if let Some(s) = as_balance_str(v) {
                        return Some(s);
                    }
                }
                if let Some(r) = find_balance_like(v) {
                    return Some(r);
                }
            }
            None
        }
        Value::Array(a) => {
            for v in a {
                if let Some(r) = find_balance_like(v) {
                    return Some(r);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn as_balance_str(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i.to_string())
            } else if let Some(f) = n.as_f64() {
                Some(format!("{:.2}", f))
            } else {
                None
            }
        }
        Value::String(s) if !s.is_empty() && s.parse::<f64>().is_ok() => Some(s.clone()),
        _ => None,
    }
}

/// 按 catalog.gwextra 的余额动作拉取单个账号的**原始响应**。
/// 余额动作 id 各平台不统一：wb 系=points_balance、WPS=credits_balance、web-glm=score_balance。
/// 拉不到返回 None（调用方保留原值或置空）。
pub async fn fetch_raw(platform: &str, secret: &str, typ: &str, gwextra: &Value) -> Option<Value> {
    let g = gwextra.as_object()?;
    let top_base = g
        .get("base")
        .and_then(|b| b.as_object())
        .and_then(|b| b.get(platform))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let acts = g.get("actions")?.get(platform)?.as_array()?;
    // ★ 动作 id 各平台不统一：wb 系=points_balance、WPS=credits_balance、web-glm=score_balance
    let act = acts.iter().find(|a| {
        let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
        matches!(id, "points_balance" | "credits_balance" | "score_balance")
    })?;
    let base = act
        .get("base")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(top_base)?;
    let path = act.get("path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    let method = act
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_uppercase();
    let body_spec = act.get("body").cloned();
    let url = format!("{}{}", base.trim_end_matches('/'), path);

    let client = Client::builder().timeout(Duration::from_secs(15)).build().ok()?;
    let mut req = if method == "POST" || body_spec.is_some() {
        client
            .post(&url)
            .json(&body_spec.unwrap_or_else(|| json!({})))
    } else {
        client.get(&url)
    };
    req = req
        .header("User-Agent", UA)
        .header("Accept", "application/json, text/plain, */*")
        .header("Referer", format!("{}/", base));

    // 鉴权：cookie 类走 Cookie 头，其余走 Bearer（含 token / accessToken 等）
    req = if typ.eq_ignore_ascii_case("cookie") {
        req.header("Cookie", secret)
    } else {
        req.header("Authorization", format!("Bearer {}", secret))
    };

    // 平台专属请求头（不覆盖鉴权头）
    if let Some(vh) = g
        .get("vendor_headers")
        .and_then(|v| v.get(platform))
        .and_then(|v| v.as_object())
    {
        for (k, v) in vh {
            let kl = k.to_lowercase();
            if kl == "authorization" || kl == "cookie" {
                continue;
            }
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }
    }

    let r = req.send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().await.ok()?;
    Some(v)
}

/// 拉取单个账号余额（标量）。内部走 `fetch_raw` 再解析。
pub async fn fetch_balance(
    platform: &str,
    secret: &str,
    typ: &str,
    gwextra: &Value,
) -> Option<String> {
    let v = fetch_raw(platform, secret, typ, gwextra).await?;
    extract_balance_value(&v)
}
