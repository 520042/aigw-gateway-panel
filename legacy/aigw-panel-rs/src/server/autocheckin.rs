//! APP 平台自动签到 + 后台调度器（移植自 app/autocheckin.py + app/scheduler.py 的自动化部分）。
//!
//! Rust 面板无 EXE，因此**去掉网关守护**那一层，只保留「自动化面板」的核心卖点：
//!   - 按平台独立开关 + 独立时间（默认错峰）
//!   - 失败自动重试（可配次数与间隔）
//!   - 每次执行落 usage_events，可查历史
//!   - 严格幂等：同一天同一平台只跑一次（除非 force）
//!   - 无凭据 / 未开放签到 明确区分，不算失败
//!
//! 配置落在 store 的 `autocheckin` 键；「今天已跑」落在 `autocheckin_done`。
use crate::server::state::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

/// 平台 → (显示名, mode, 默认时, 默认分)。mode 仅供前端展示提示，执行按 pid 分派。
pub const PLATFORM_CHECKIN: &[(&str, &str, &str, u32, u32)] = &[
    ("wb-gateway", "WorkBuddy 国内", "direct", 9, 5),
    ("wb-gateway-intl", "WorkBuddy 国际", "direct", 9, 10),
    ("apk-trae", "Trae", "flow", 9, 15),
    ("apk-raccoon", "小浣熊", "bonus", 9, 20),
    ("apk-kuku", "百度文库", "kuku", 9, 25),
    ("apk-wps", "WPS AI", "wps", 9, 30),
];

fn mode_hint(mode: &str) -> &'static str {
    match mode {
        "flow" => "先查状态再领（Trae）",
        "direct" => "单接口直接领（WorkBuddy）",
        "bonus" => "登录即送积分（小浣熊）",
        "kuku" => "两段式：先取 bdstoken 会话参数再领积分",
        "wps" => "先查任务状态，未签则领（已签幂等）",
        _ => "公开接口里没有签到端点，需先用「探测端点」扫出来",
    }
}

fn default_cfg() -> Value {
    json!({
        "enabled": true,
        "default_time": "09:10",
        "stagger_sec": 45,
        "retry_times": 2,
        "retry_delay_min": 10,
        "notify": true,
        "platforms": {},
    })
}

pub fn load_cfg() -> Value {
    let mut cfg = default_cfg();
    let saved = crate::server::store::load("autocheckin");
    if let (Some(dst), Some(src)) = (cfg.as_object_mut(), saved.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    if !cfg.get("platforms").map(|v| v.is_object()).unwrap_or(false) {
        cfg["platforms"] = json!({});
    }
    cfg
}

pub fn save_cfg(cfg: &Value) {
    crate::server::store::save("autocheckin", cfg);
}

/// 本地时间 (日, 时, 分)——CST = UTC+8。
pub fn local_dhm() -> (String, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + 8 * 3600;
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let sod = (secs % 86400) as u32;
    (
        format!("{:04}-{:02}-{:02}", y, m, d),
        sod / 3600,
        (sod % 3600) / 60,
    )
}

fn hm(s: &str, dft: (u32, u32)) -> (u32, u32) {
    let mut it = s.split(':');
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => match (a.trim().parse::<u32>(), b.trim().parse::<u32>()) {
            (Ok(x), Ok(y)) => (x, y),
            _ => dft,
        },
        _ => dft,
    }
}

/// 给前端用：每个平台的开关与时间。
pub fn platform_list() -> Vec<Value> {
    let cfg = load_cfg();
    let plats = cfg.get("platforms").cloned().unwrap_or(json!({}));
    PLATFORM_CHECKIN
        .iter()
        .map(|(pid, name, mode, dh, dm)| {
            let p = plats.get(pid).cloned().unwrap_or(json!({}));
            json!({
                "platform": pid,
                "name": name,
                "mode": mode,
                "has_public_checkin": *mode != "none",
                "on": p.get("on").and_then(|v| v.as_bool()).unwrap_or(true),
                "time": p.get("time").and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{:02}:{:02}", dh, dm)),
                "hint": mode_hint(mode),
            })
        })
        .collect()
}

fn done_today_set() -> std::collections::HashSet<String> {
    let (today, _, _) = local_dhm();
    let stored = crate::server::store::load("autocheckin_done");
    if stored.get("date").and_then(|v| v.as_str()) == Some(today.as_str()) {
        stored
            .get("platforms")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    } else {
        std::collections::HashSet::new()
    }
}

fn mark_done(platform: &str) {
    let (today, _, _) = local_dhm();
    let mut set = done_today_set();
    set.insert(platform.to_string());
    let list: Vec<String> = set.into_iter().collect();
    crate::server::store::save("autocheckin_done", &json!({"date": today, "platforms": list}));
}

/// 状态卡（对齐 Python autocheckin.status）。
pub fn status(st: &AppState) -> Value {
    let cfg = load_cfg();
    let (today, hh, mm) = local_dhm();
    let done = done_today_set();
    let mut pls = platform_list();
    for p in pls.iter_mut() {
        let pid = p.get("platform").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let logged = !st.inner.login.secrets_for(&pid).is_empty();
        p["logged_in"] = json!(logged);
        p["done_today"] = json!(done.contains(&pid));
    }
    json!({
        "enabled": cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
        "default_time": cfg.get("default_time").and_then(|v| v.as_str()).unwrap_or("09:10"),
        "stagger_sec": cfg.get("stagger_sec").and_then(|v| v.as_i64()).unwrap_or(45),
        "retry_times": cfg.get("retry_times").and_then(|v| v.as_i64()).unwrap_or(2),
        "retry_delay_min": cfg.get("retry_delay_min").and_then(|v| v.as_i64()).unwrap_or(10),
        "notify": cfg.get("notify").and_then(|v| v.as_bool()).unwrap_or(true),
        "platforms": pls,
        "now": format!("{} {:02}:{:02}:00", today, hh, mm),
        "thread_alive": true,
        "running": RUNNING.load(Ordering::Relaxed),
    })
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// 保存配置（顶层开关 + 平台级 on/time），返回最新 status。
pub fn save_from_body(st: &AppState, b: &Value) -> Value {
    let mut cfg = load_cfg();
    for k in ["enabled", "stagger_sec", "retry_times", "retry_delay_min", "notify", "default_time"] {
        if let Some(v) = b.get(k) {
            cfg[k] = v.clone();
        }
    }
    if let Some(obj) = b.get("platforms").and_then(|v| v.as_object()) {
        let mut cur = cfg.get("platforms").and_then(|v| v.as_object()).cloned().unwrap_or_default();
        for (pid, v) in obj {
            if let Some(vo) = v.as_object() {
                let mut e = cur.get(pid).and_then(|x| x.as_object()).cloned().unwrap_or_default();
                e.insert("on".to_string(), json!(vo.get("on").and_then(|x| x.as_bool()).unwrap_or(true)));
                e.insert(
                    "time".to_string(),
                    json!(vo.get("time").and_then(|x| x.as_str()).unwrap_or("09:10")),
                );
                cur.insert(pid.clone(), Value::Object(e));
            }
        }
        cfg["platforms"] = Value::Object(cur);
    }
    save_cfg(&cfg);
    status(st)
}

/// 当前时刻应跑的平台（按时间排序）。
fn due_list(cfg: &Value) -> Vec<(String, String, u32, u32)> {
    let (_, hh, mm) = local_dhm();
    let now_min = hh * 60 + mm;
    let plats = cfg.get("platforms").cloned().unwrap_or(json!({}));
    let mut out: Vec<(String, String, u32, u32)> = Vec::new();
    for (pid, name, _mode, dh, dm) in PLATFORM_CHECKIN {
        let p = plats.get(pid).cloned().unwrap_or(json!({}));
        if !p.get("on").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        let t = p
            .get("time")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{:02}:{:02}", dh, dm));
        let (h, m) = hm(&t, (*dh, *dm));
        if now_min >= h * 60 + m {
            out.push((pid.to_string(), name.to_string(), h, m));
        }
    }
    out.sort_by_key(|(_, _, h, m)| h * 60 + m);
    out
}

fn is_auth_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "401", "403", "未登录", "登录", "凭据", "cookie", "token", "账号", "unauthorized",
        "forbidden", "缺少", "no_credential", "无效", "authenticate", "接口拒绝", "未认证", "1001",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// 跑一轮自动签到（对齐 Python run_round）。returns results payload.
pub async fn run_round(st: &AppState, force: bool, only: Option<Vec<String>>) -> Value {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return json!({"ok": false, "message": "上一轮签到还在跑"});
    }
    let result = run_round_inner(st, force, only).await;
    RUNNING.store(false, Ordering::SeqCst);
    result
}

async fn run_round_inner(st: &AppState, force: bool, only: Option<Vec<String>>) -> Value {
    let cfg = load_cfg();
    let mut todo: Vec<(String, String, String)> = Vec::new();
    let has_only = only.is_some();
    if force || has_only {
        let wanted: Option<Vec<String>> = only;
        for (pid, name, mode, _, _) in PLATFORM_CHECKIN {
            let include = match &wanted {
                Some(w) => w.iter().any(|x| x == pid),
                None => true, // force 且未指定 only → 全部
            };
            if include {
                todo.push((pid.to_string(), name.to_string(), mode.to_string()));
            }
        }
    } else {
        for (pid, name, _, _) in due_list(&cfg) {
            let mode = PLATFORM_CHECKIN
                .iter()
                .find(|(p, _, _, _, _)| *p == pid)
                .map(|(_, _, m, _, _)| m.to_string())
                .unwrap_or_else(|| "direct".to_string());
            todo.push((pid, name, mode));
        }
    }
    if todo.is_empty() {
        return json!({"ok": true, "message": "没有到点的平台", "results": []});
    }
    let rt = cfg.get("retry_times").and_then(|v| v.as_i64()).unwrap_or(0).max(0);
    let delay = cfg.get("retry_delay_min").and_then(|v| v.as_f64()).unwrap_or(0.0) * 60.0;
    let stagger = cfg.get("stagger_sec").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let force_all = force || has_only;

    let mut results: Vec<Value> = Vec::new();
    let n = todo.len();
    for (i, (pid, name, _mode)) in todo.iter().enumerate() {
        if !force_all && done_today_set().contains(pid) {
            results.push(json!({"platform": pid, "name": name, "ok": true, "skipped": true,
                                "message": "今天已执行过"}));
            continue;
        }
        let (mut okk, mut msg, mut skipped) = crate::server::checkin::run_platform(st, pid).await;
        let mut tries = 0i64;
        while !okk && !skipped && rt > 0 && tries < rt && delay > 0.0 {
            tries += 1;
            tokio::time::sleep(std::time::Duration::from_secs_f64(delay)).await;
            let r = crate::server::checkin::run_platform(st, pid).await;
            okk = r.0;
            msg = r.1;
            skipped = r.2;
        }
        // 无凭据导致的失败如实标 skipped，不算失败
        if !okk && !skipped && is_auth_error(&msg) {
            skipped = true;
        }
        if okk || skipped {
            mark_done(pid);
        }
        st.inner.usage.record(pid, "checkin", 0, 0, 0, okk || skipped, &format!("auto:{}", pid));
        results.push(json!({
            "platform": pid, "name": name, "ok": okk, "skipped": skipped,
            "message": msg, "retries": tries,
        }));
        if stagger > 0.0 && i < n - 1 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(stagger)).await;
        }
    }
    let ok_n = results.iter().filter(|r| r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)).count();
    let sk_n = results.iter().filter(|r| r.get("skipped").and_then(|v| v.as_bool()).unwrap_or(false)).count();
    let fail_n = results.iter().filter(|r| {
        !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
            && !r.get("skipped").and_then(|v| v.as_bool()).unwrap_or(false)
    }).count();
    json!({
        "ok": fail_n == 0,
        "summary": format!("APP 自动签到：成功 {} / 跳过 {} / 失败 {}", ok_n, sk_n, fail_n),
        "success": ok_n, "skipped": sk_n, "failed": fail_n,
        "results": results,
    })
}

/// 后台调度器：每 60s 检查一次，按配置时间跑签到。
pub fn spawn_scheduler(st: AppState) {
    tokio::spawn(async move {
        tracing::info!("APP 自动签到调度器已启动（60s 轮询，配置时间自动触发）");
        // 启动后静默观察 50s，避免刚开面板就补跑当天的签到
        tokio::time::sleep(std::time::Duration::from_secs(50)).await;
        loop {
            let cfg = load_cfg();
            if cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
                let r = run_round(&st, false, None).await;
                if r.get("results").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false) {
                    tracing::info!(summary = %r.get("summary").and_then(|v| v.as_str()).unwrap_or(""), "自动签到");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}
