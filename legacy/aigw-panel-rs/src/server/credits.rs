//! 积分到期看板。
//!
//! 动机（抄自参考框架 xiaofan6ya/workbuddy2api 的「积分到期」面板）：
//! 免费额度不是"有多少"的问题，而是"还剩几天"的问题 —— 一个 6000 积分的账号
//! 如果 3 天后清零，比一个 800 积分但 30 天后才到期的账号更该先烧掉。
//! 所以这里把**每个资源包**拆成一行，按到期时间升序排，并给出 ≤1/3/7/14/30 天的风险分布。
//!
//! 数据源：`balance::fetch_raw` 的原始响应。腾讯系（wb-gateway / wb-gateway-intl）的
//! 资源包形状已实测确认（2026-10-15）：
//!   Accounts[]: { PackageName, ProductName, CapacityUnit, CapacityRemain, CapacitySize,
//!                 CycleEndTime:"2026-10-31 23:59:59", ExpiredTime:"", DeductionEndTime:<ms>,
//!                 RemainCycles, TotalCycles, Status }
//! ★ 到期时间优先级：CycleEndTime（月度周期） > ExpiredTime > DeductionEndTime。
//!   三个都没有就当"无到期信息"，不参与风险分桶（不能凭空造一个到期日）。

use serde_json::{json, Value};

/// 单个资源包（已归一化为展示行）。
#[derive(Debug, Clone)]
pub struct Pkg {
    pub platform: String,
    pub account: String,
    /// 账号池行 id（accounts.json 的 "id"），用于把紧迫度写回具体账号
    pub account_id: String,
    pub name: String,
    pub unit: String,
    pub remain: f64,
    pub size: f64,
    /// 到期时间戳（毫秒 UTC），无到期信息为 None。
    pub expire_ms: Option<i64>,
    /// 到期时间的来源字段名（便于排错）。
    pub expire_src: String,
}

fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_i64().map(|i| i as f64))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
        .unwrap_or(0.0)
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
}

/// 把 "2026-10-31 23:59:59" 这类本地时间字符串解析成毫秒时间戳。
/// 只用标准库手算（不引 chrono，保持依赖最小）。
fn parse_local_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // 支持 "YYYY-MM-DD HH:MM:SS" / "YYYY-MM-DDTHH:MM:SSZ" / "YYYY-MM-DD"
    let (date, time) = if let Some(pos) = s.find('T').or_else(|| s.find(' ')) {
        (&s[..pos], s[pos + 1..].trim())
    } else {
        (s, "00:00:00")
    };
    let d: Vec<&str> = date.split('-').collect();
    if d.len() != 3 {
        return None;
    }
    let y: i64 = d[0].parse().ok()?;
    let m: i64 = d[1].parse().ok()?;
    let dd: i64 = d[2].parse().ok()?;
    if !(1970..=2200).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&dd) {
        return None;
    }
    let t = time.trim_end_matches('Z');
    let tp: Vec<&str> = t.split(':').collect();
    let hh: i64 = tp.first().and_then(|x| x.parse().ok()).unwrap_or(0);
    let mm: i64 = tp.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    let ss: i64 = tp.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);

    // 日期 → 天数（Howard Hinnant 的 civil_from_days 逆运算）
    let days = civil_to_days(y, m, dd);
    let secs = days * 86400 + hh * 3600 + mm * 60 + ss;
    // 该时间串是**本地时间**（北京时间 UTC+8），转成 UTC 毫秒要减 8 小时
    Some((secs - 8 * 3600) * 1000)
}

fn civil_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 从资源包对象里取到期时间（毫秒）与来源字段。
fn pick_expire(o: &serde_json::Map<String, Value>) -> (Option<i64>, String) {
    // ① 月度周期结束时间（腾讯系主口径）
    if let Some(v) = o.get("CycleEndTime") {
        if let Some(s) = v.as_str() {
            if let Some(ms) = parse_local_datetime(s) {
                return (Some(ms), "CycleEndTime".into());
            }
        }
        if let Some(ms) = v.as_i64() {
            if ms > 1_000_000_000_000 {
                return (Some(ms), "CycleEndTime".into());
            }
        }
    }
    // ② 显式过期时间（空串是常态，表示不过期）
    for k in ["ExpiredTime", "expiredTime", "expireTime", "ExpireTime"] {
        if let Some(v) = o.get(k) {
            if let Some(s) = v.as_str() {
                if let Some(ms) = parse_local_datetime(s) {
                    return (Some(ms), k.to_string());
                }
            }
            if let Some(ms) = v.as_i64() {
                if ms > 1_000_000_000_000 {
                    return (Some(ms), k.to_string());
                }
            }
        }
    }
    // ③ 扣减截止时间（很远的兜底值，如 2033 年）
    for k in ["DeductionEndTime", "deductionEndTime", "endTime", "EndTime"] {
        if let Some(ms) = o.get(k).and_then(|v| v.as_i64()) {
            if ms > 1_000_000_000_000 {
                return (Some(ms), k.to_string());
            }
        }
        if let Some(s) = o.get(k).and_then(|v| v.as_str()) {
            if let Some(ms) = parse_local_datetime(s) {
                return (Some(ms), k.to_string());
            }
        }
    }
    (None, "".into())
}

/// 从原始余额响应里拆出资源包数组。
fn accounts_array(raw: &Value) -> Option<&Vec<Value>> {
    crate::server::balance::find_key(raw, "Accounts")?.as_array()
}

/// 把原始余额响应拆成资源包行。拆不出包就退化成"单包"（用标量余额，无到期信息）。
pub fn extract_packages(platform: &str, account: &str, account_id: &str, raw: &Value) -> Vec<Pkg> {
    let mut out: Vec<Pkg> = Vec::new();
    if let Some(arr) = accounts_array(raw) {
        for it in arr {
            let Some(o) = it.as_object() else { continue };
            let unit = str_of(o.get("CapacityUnit"));
            // 只统计积分单位（并发/次数类资源包不参与积分到期看板）
            if !unit.is_empty() && !unit.eq_ignore_ascii_case("credits") {
                continue;
            }
            let remain = num(o.get("CapacityRemain").unwrap_or(&Value::Null));
            let size = num(o.get("CapacitySize").unwrap_or(&Value::Null));
            let name = str_of(o.get("PackageName"));
            let name = if name.is_empty() {
                str_of(o.get("ProductName"))
            } else {
                name
            };
            let (expire_ms, src) = pick_expire(o);
            out.push(Pkg {
                platform: platform.to_string(),
                account: account.to_string(),
                account_id: account_id.to_string(),
                name: if name.is_empty() { "积分包".to_string() } else { name },
                unit: if unit.is_empty() { "credits".to_string() } else { unit },
                remain,
                size,
                expire_ms,
                expire_src: src,
            });
        }
    }
    if !out.is_empty() {
        return out;
    }
    // 退化：标量余额，无到期信息
    if let Some(s) = crate::server::balance::extract_balance_value(raw) {
        if let Ok(f) = s.parse::<f64>() {
            out.push(Pkg {
                platform: platform.to_string(),
                account: account.to_string(),
                account_id: account_id.to_string(),
                name: "余额".to_string(),
                unit: "credits".to_string(),
                remain: f,
                size: f,
                expire_ms: None,
                expire_src: "".to_string(),
            });
        }
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 风险分桶：已到期 / ≤1 天 / ≤3 天 / ≤7 天 / ≤14 天 / ≤30 天 / 更远 / 无到期信息。
fn bucket_of(expire_ms: Option<i64>, now: i64) -> &'static str {
    let Some(ms) = expire_ms else { return "none" };
    let left = ms - now;
    let day = 86_400_000i64;
    if left <= 0 {
        "expired"
    } else if left <= day {
        "d1"
    } else if left <= 3 * day {
        "d3"
    } else if left <= 7 * day {
        "d7"
    } else if left <= 14 * day {
        "d14"
    } else if left <= 30 * day {
        "d30"
    } else {
        "later"
    }
}

/// 拉全部账号的积分快照。
/// `accounts` = 账号池行（含 platform/secret/type/enabled/name）。
pub async fn snapshot(accounts: &[Value], gwextra: &Value) -> Value {
    let now = now_ms();
    let mut pkgs: Vec<Pkg> = Vec::new();
    let mut scanned = 0usize;

    for a in accounts {
        let platform = str_of(a.get("platform"));
        let secret = str_of(a.get("secret"));
        if platform.is_empty() || secret.is_empty() {
            continue;
        }
        if a.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let typ = str_of(a.get("type"));
        let label = str_of(a.get("name").or(a.get("label")));
        let label = if label.is_empty() { platform.clone() } else { label };
        let acct_id = str_of(a.get("id"));
        scanned += 1;
        let Some(raw) =
            crate::server::balance::fetch_raw(&platform, &secret, &typ, gwextra).await
        else {
            continue;
        };
        pkgs.extend(extract_packages(&platform, &label, &acct_id, &raw));
    }

    let mut rows: Vec<Value> = pkgs
        .iter()
        .map(|p| {
            let left_ms = p.expire_ms.map(|ms| ms - now);
            let days = left_ms.map(|d| (d as f64 / 86_400_000.0).ceil() as i64);
            json!({
                "platform": p.platform,
                "account": p.account,
                "account_id": p.account_id,
                "name": p.name,
                "unit": p.unit,
                "remain": p.remain,
                "size": p.size,
                "expire_ms": p.expire_ms,
                "expire_src": p.expire_src,
                "days_left": days,
                "bucket": bucket_of(p.expire_ms, now),
            })
        })
        .collect();

    // 排序：**还有余额的排前面**，组内按到期升序（最先清零的最靠上）。
    // ★ 实测 wb-gateway 有 49 个资源包，其中四十多个已用完（remain=0），
    //   纯按到期排会让「0 积分的包」霸占首屏 —— 用户真正要看的是"还有多少快没了"。
    rows.sort_by(|x, y| {
        let rx = x.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ry = y.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let live_x = if rx > 0.0 { 0 } else { 1 };
        let live_y = if ry > 0.0 { 0 } else { 1 };
        let ex = x.get("expire_ms").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
        let ey = y.get("expire_ms").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
        live_x
            .cmp(&live_y)
            .then(ex.cmp(&ey))
            .then_with(|| ry.partial_cmp(&rx).unwrap_or(std::cmp::Ordering::Equal))
    });

    // 分桶统计（按包数 + 按积分数双口径）
    let mut buckets: std::collections::BTreeMap<&str, (i64, f64)> = std::collections::BTreeMap::new();
    for r in &rows {
        let b = r.get("bucket").and_then(|v| v.as_str()).unwrap_or("none");
        let e = buckets.entry(b).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
    }
    // 已用完的包（remain<=0）不计入风险提示，但单独给个数，避免"49 个包 1 天后到期"的误读
    let depleted = rows
        .iter()
        .filter(|r| r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0) <= 0.0)
        .count() as i64;
    let live = rows.len() as i64 - depleted;
    let order = ["expired", "d1", "d3", "d7", "d14", "d30", "later", "none"];
    let dist: Vec<Value> = order
        .iter()
        .map(|k| {
            let (n, sum) = buckets.get(k).cloned().unwrap_or((0, 0.0));
            json!({"key": k, "packages": n, "credits": sum})
        })
        .collect();

    // 按平台聚合（紧迫度排行：最近到期日 + 该平台总剩余）
    let mut byp: std::collections::BTreeMap<String, (f64, i64, i64)> =
        std::collections::BTreeMap::new();
    for r in &rows {
        let p = r.get("platform").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let e = byp.entry(p).or_insert((0.0, 0, i64::MAX));
        e.0 += r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
        e.1 += 1;
        if let Some(ms) = r.get("expire_ms").and_then(|v| v.as_i64()) {
            if ms < e.2 {
                e.2 = ms;
            }
        }
    }
    let mut by_plat: Vec<Value> = byp
        .into_iter()
        .map(|(p, (remain, n, soonest))| {
            json!({
                "platform": p,
                "remain": remain,
                "packages": n,
                "expire_ms": if soonest == i64::MAX { Value::Null } else { json!(soonest) },
                "days_left": if soonest == i64::MAX { Value::Null }
                             else { json!(((soonest - now) as f64 / 86_400_000.0).ceil() as i64) },
            })
        })
        .collect();
    by_plat.sort_by(|x, y| {
        let a = x.get("expire_ms").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
        let b = y.get("expire_ms").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
        a.cmp(&b)
    });

    let total_remain: f64 = rows
        .iter()
        .map(|r| r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum();
    let at_risk: f64 = rows
        .iter()
        .filter(|r| matches!(r.get("bucket").and_then(|v| v.as_str()).unwrap_or(""),
                             "expired" | "d1" | "d3" | "d7"))
        .map(|r| r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0))
        .sum();

    json!({
        "ok": true,
        "ts": now,
        "scanned": scanned,
        "packages": rows.len(),
        "live": live,
        "depleted": depleted,
        "total_remain": total_remain,
        "at_risk": at_risk,
        "dist": dist,
        "by_platform": by_plat,
        "rows": rows,
    })
}

/// 账号紧迫度排序键（**越小越该先用**）。
///
/// 抄参考框架 `admin/pool.py::urgency_rank` 的思路：免费额度不是"有多少"的问题，
/// 而是"还剩几天"的问题 —— 一个 6000 积分 3 天后清零的账号，比 800 积分 30 天后
/// 才到期的账号更该先烧。所以：
///
/// 1. **有到期信息且还有余额** → 键 = `days_left * 1000`（最先清零的最靠前）
/// 2. **无到期信息但有余额** → 键 = `10_000_000 - min(balance, 10_000_000)`
///    （余额越多越靠前，把大头先花掉，避免集中过期）
/// 3. **什么都没有** → 键 = `i64::MAX`（排最后，等价于不参与优先）
///
/// 只按账号维度聚合（取该账号**还有余额的包**里最早的到期日），不逐包展开 ——
/// 选号只需要一个可比的值。
pub fn urgency_score(rows: &[Value]) -> i64 {
    let mut soonest_days: Option<i64> = None;
    let mut remain_sum = 0.0f64;
    for r in rows {
        let remain = r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if remain <= 0.0 {
            continue;
        }
        remain_sum += remain;
        if let Some(d) = r.get("days_left").and_then(|v| v.as_i64()) {
            soonest_days = Some(match soonest_days {
                Some(x) => x.min(d),
                None => d,
            });
        }
    }
    match soonest_days {
        Some(d) => d.saturating_mul(1000),
        None if remain_sum > 0.0 => {
            let cap = remain_sum.min(10_000_000.0) as i64;
            10_000_000 - cap
        }
        None => i64::MAX,
    }
}

/// 从快照 rows 聚出「账号 → 紧迫度键」，供写回 accounts.json。
/// 返回 `[(account_id, score, remain, days_left)]`，无 account_id 的行跳过。
pub fn account_urgency(rows: &[Value]) -> Vec<(String, i64, f64, Option<i64>)> {
    let mut by: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    for r in rows {
        if let Some(id) = r.get("account_id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                by.entry(id.to_string()).or_default().push(r.clone());
            }
        }
    }
    by.into_iter()
        .map(|(id, rs)| {
            let remain: f64 = rs
                .iter()
                .map(|r| r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0))
                .sum();
            let days = rs
                .iter()
                .filter(|r| r.get("remain").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0)
                .filter_map(|r| r.get("days_left").and_then(|v| v.as_i64()))
                .min();
            (id, urgency_score(&rs), remain, days)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datetime_works() {
        let ms = parse_local_datetime("2026-10-31 23:59:59").unwrap();
        // 北京时间 2026-10-31 23:59:59 = UTC 15:59:59
        let expect = civil_to_days(2026, 10, 31) * 86400 + 15 * 3600 + 59 * 60 + 59;
        assert_eq!(ms, expect * 1000);
        assert!(parse_local_datetime("").is_none());
        assert!(parse_local_datetime("不是时间").is_none());
    }

    #[test]
    fn bucket_boundaries() {
        let now = 1_700_000_000_000i64;
        let d = 86_400_000i64;
        assert_eq!(bucket_of(Some(now - 1), now), "expired");
        assert_eq!(bucket_of(Some(now + d), now), "d1");
        assert_eq!(bucket_of(Some(now + 3 * d), now), "d3");
        assert_eq!(bucket_of(Some(now + 8 * d), now), "d14");
        assert_eq!(bucket_of(Some(now + 40 * d), now), "later");
        assert_eq!(bucket_of(None, now), "none");
    }

    #[test]
    fn urgency_prefers_earliest_expiry() {
        // 同样有余额：3 天后到期的账号必须比 30 天后到期的更紧迫
        let soon = json!([{"remain":100.0,"days_left":3}]);
        let late = json!([{"remain":100.0,"days_left":30}]);
        assert!(urgency_score(soon.as_array().unwrap()) < urgency_score(late.as_array().unwrap()));

        // 有到期信息的永远优先于「余额多但没到期日」的
        let tiny_expiring = json!([{"remain":1.0,"days_left":7}]);
        let big_no_expiry = json!([{"remain":9999.0,"days_left":null}]);
        assert!(urgency_score(tiny_expiring.as_array().unwrap())
                < urgency_score(big_no_expiry.as_array().unwrap()));

        // 已用完的包不参与：只剩 remain=0 时应落到最低优先
        let dead = json!([{"remain":0.0,"days_left":1}]);
        assert_eq!(urgency_score(dead.as_array().unwrap()), i64::MAX);
    }

    #[test]
    fn account_urgency_groups_by_account() {
        let rows = json!([
            {"account_id":"a","remain":10.0,"days_left":2},
            {"account_id":"a","remain":0.0,"days_left":1},
            {"account_id":"b","remain":500.0,"days_left":null},
            {"account_id":"","remain":9.0,"days_left":1}
        ]);
        let m = account_urgency(rows.as_array().unwrap());
        assert_eq!(m.len(), 2, "空 account_id 应被跳过");
        let a = m.iter().find(|x| x.0=="a").unwrap();
        let b = m.iter().find(|x| x.0=="b").unwrap();
        assert_eq!(a.3, Some(2), "a 的最早到期应是还有余额的那个（2 天），不是已用完的 1 天");
        assert_eq!(a.1, 2000);
        assert!(a.1 < b.1, "有到期日的 a 必须比无到期日的 b 更紧迫");
    }

    #[test]
    fn extract_packages_filters_non_credits() {
        let raw = json!({"Data":{"Accounts":[
            {"PackageName":"体验包","CapacityUnit":"credits","CapacityRemain":500,
             "CapacitySize":500,"CycleEndTime":"2026-10-31 23:59:59"},
            {"PackageName":"并发包","CapacityUnit":"concurrency","CapacityRemain":3}
        ]}});
        let p = extract_packages("wb-gateway", "acc", "acct-1", &raw);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "体验包");
        assert!(p[0].expire_ms.is_some());
    }
}
