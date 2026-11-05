//! WB 任务执行台账（参考 Agent2API 的 checkin-records）。
//!
//! 与「日志」页不同：这里只记**一次任务执行的可读结论**，按账号一行落盘到
//! `data/task-records.json`，供面板「签到记录」页做筛选 / 分页。不记请求体、
//! 不记凭据、不记上游原始响应 —— 后者可能包含 Cookie / Authorization。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;

/// 只保留最近 600 条：每天几个账号跑两轮，足够覆盖半年以上。
const CAP: usize = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// unix 秒。
    pub ts: f64,
    /// manual / schedule / activity / health。
    pub source: String,
    /// checkin / growth / growth:&lt;action&gt; 等。
    pub task: String,
    pub platform: String,
    pub account: String,
    /// success / already / failed。
    pub outcome: String,
    pub detail: String,
}

impl TaskRecord {
    pub fn new(
        source: &str,
        task: &str,
        platform: &str,
        account: &str,
        outcome: &str,
        detail: &str,
    ) -> Self {
        Self {
            ts: aigw_store::now_unix(),
            source: source.to_string(),
            task: task.to_string(),
            platform: platform.to_string(),
            account: account.to_string(),
            outcome: outcome.to_string(),
            detail: safe_detail(detail),
        }
    }
}

/// 上游报错可能回显 Authorization / Cookie 或长 token：台账只留人能读懂的结论，
/// 命中敏感标记时整段隐去，长串也按「已隐去」处理，绝不把原文写进面板数据。
fn safe_detail(raw: &str) -> String {
    let flat = raw.replace(['\r', '\n'], " ").trim().to_string();
    let low = flat.to_ascii_lowercase();
    if ["bearer ", "authorization", "cookie", "set-cookie", "api_key", "apikey", "sk-", "x-token"]
        .iter()
        .any(|k| low.contains(k))
    {
        return "上游信息含敏感字段，已隐去".to_string();
    }
    let mut out = Vec::new();
    for part in flat.split_whitespace() {
        let compact: String = part
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if compact.len() >= 32 {
            out.push("[已隐去]".to_string());
        } else {
            out.push(part.to_string());
        }
    }
    out.join(" ").chars().take(220).collect()
}

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn file_of(data_dir: &Path) -> PathBuf {
    data_dir.join("task-records.json")
}

fn read(path: &Path) -> Vec<TaskRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            let arr = v.get("records").cloned().unwrap_or(v);
            serde_json::from_value::<Vec<TaskRecord>>(arr).ok()
        })
        .unwrap_or_default()
}

/// 追加一批执行结果。没有数据目录（测试态）时只丢掉台账，不影响任务本身。
pub fn append(data_dir: Option<&Path>, rows: Vec<TaskRecord>) {
    let Some(dd) = data_dir else { return };
    if rows.is_empty() {
        return;
    }
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let path = file_of(dd);
    let mut all = read(&path);
    all.extend(rows);
    if all.len() > CAP {
        all = all.split_off(all.len() - CAP);
    }
    if let Err(e) = aigw_store::write_json_atomic(&path, &json!({ "records": all })) {
        tracing::warn!(file = %path.display(), "WB 任务执行台账落盘失败：{e}");
    }
}

/// 面板读取：`range`（today/7/30/all）、`outcome`、`task`、`q`、`limit`、`offset`。
/// 后端做筛选与分页，返回 `{ok,rows,total,shown,limit,offset}`。
pub fn query(data_dir: Option<&Path>, params: &HashMap<String, String>) -> Value {
    let Some(dd) = data_dir else {
        return json!({"ok": true, "rows": [], "total": 0, "shown": 0, "limit": 20, "offset": 0});
    };
    let _g = lock().lock().unwrap_or_else(|e| e.into_inner());
    let all = read(&file_of(dd));

    // 北京时间 UTC+8 的自然日下界（沿用 tasks.rs 的口径，不引 chrono）。
    let now = aigw_store::now_unix();
    let local = now + 8.0 * 3600.0;
    let today_local = local - local.rem_euclid(86400.0);
    let days = match params.get("range").map(|s| s.as_str()).unwrap_or("all") {
        "today" => 0.0,
        "7" => 6.0,
        "30" => 29.0,
        _ => f64::INFINITY,
    };
    let from = if days.is_finite() {
        today_local - days * 86400.0 - 8.0 * 3600.0
    } else {
        f64::NEG_INFINITY
    };
    let outcome = params.get("outcome").map(|s| s.trim().to_string()).unwrap_or_default();
    let task = params.get("task").map(|s| s.trim().to_string()).unwrap_or_default();
    let q = params.get("q").map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();

    let mut matched: Vec<&TaskRecord> = all
        .iter()
        .filter(|r| r.ts >= from)
        .filter(|r| outcome.is_empty() || outcome == "all" || r.outcome == outcome)
        .filter(|r| task.is_empty() || task == "all" || r.task == task)
        .filter(|r| {
            q.is_empty()
                || [&r.platform, &r.account, &r.detail, &r.task, &r.source]
                    .iter()
                    .any(|s| s.to_ascii_lowercase().contains(&q))
        })
        .collect();
    matched.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    let total = matched.len();
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(20)
        .min(200);
    let offset = params
        .get("offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(CAP);
    let page: Vec<Value> = matched
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|r| {
            json!({
                "ts": r.ts, "source": r.source, "task": r.task,
                "platform": r.platform, "account": r.account,
                "outcome": r.outcome, "detail": r.detail,
            })
        })
        .collect();
    let shown = page.len();
    json!({"ok": true, "rows": page, "total": total, "shown": shown, "limit": limit, "offset": offset})
}

/// 一段任务文字结论 → 一条台账行（成长任务 / 健康检查共用）。
pub fn from_summary(
    source: &str,
    task: &str,
    platform: &str,
    account: &str,
    ok: bool,
    detail: &str,
) -> TaskRecord {
    TaskRecord::new(
        source,
        task,
        platform,
        account,
        if ok { "success" } else { "failed" },
        detail,
    )
}

/// 把签到结果转成台账行，判据与 `summarize_checkin` 同源，避免界面和汇总各判一遍。
pub fn from_checkin(source: &str, results: &[Value]) -> Vec<TaskRecord> {
    results
        .iter()
        .map(|r| {
            let platform = r.get("platform").and_then(|x| x.as_str()).unwrap_or("");
            let account = r.get("account").and_then(|x| x.as_str()).unwrap_or("");
            let d = r.get("data").cloned().unwrap_or(Value::Null);
            let transport = r.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
            let biz_ok = d.get("ok").and_then(|x| x.as_bool()).unwrap_or(false)
                || d.get("granted").and_then(|x| x.as_bool()).unwrap_or(false);
            let code = d.get("code").and_then(|x| x.as_i64());
            let text = d
                .get("message")
                .or_else(|| d.get("error"))
                .or_else(|| d.get("status"))
                .or_else(|| d.get("msg"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let low = text.to_ascii_lowercase();
            let already = matches!(code, Some(2) | Some(10001))
                || d.get("already").and_then(|x| x.as_bool()) == Some(true)
                || low.contains("今日已")
                || low.contains("已领取")
                || low.contains("already");
            let outcome = if already {
                "already"
            } else if transport && biz_ok {
                // ★ 传输成功只代表 HTTP 200：上游还会用 `{ok:false,status:"...",msg:"..."}`
                //   回风控/业务失败。判据必须看业务码，不能见 JSON 对象就算成功，
                //   否则「签到中心」会把 200 + 失败页面记成「成功领取」。
                "success"
            } else {
                "failed"
            };
            let detail = if text.is_empty() {
                match outcome {
                    "already" => "今日已领取（幂等跳过）",
                    "success" => "签到完成",
                    _ => "上游请求失败",
                }
                .to_string()
            } else {
                text.to_string()
            };
            TaskRecord::new(source, "checkin", platform, account, outcome, &detail)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_detail_is_redacted() {
        let s = safe_detail("HTTP 401 Bearer abc.def.ghi cookie=session");
        assert!(s.contains("已隐去"));
        assert!(!s.contains("abc.def.ghi"));
    }

    #[test]
    fn query_filters_and_paginates() {
        let tmp = tempfile::tempdir().unwrap();
        let now = aigw_store::now_unix();
        append(
            Some(tmp.path()),
            vec![
                TaskRecord { ts: now, source: "manual".into(), task: "checkin".into(),
                    platform: "wb-gateway".into(), account: "a".into(),
                    outcome: "success".into(), detail: "签到完成".into() },
                TaskRecord { ts: now, source: "schedule".into(), task: "growth".into(),
                    platform: "wb-gateway".into(), account: "b".into(),
                    outcome: "failed".into(), detail: "过期".into() },
            ],
        );
        let mut p = HashMap::new();
        p.insert("limit".into(), "1".into());
        let v = query(Some(tmp.path()), &p);
        assert_eq!(v["total"], json!(2));
        assert_eq!(v["shown"], json!(1));
        p.insert("outcome".into(), "failed".into());
        assert_eq!(query(Some(tmp.path()), &p)["total"], json!(1));
    }

    #[test]
    fn checkin_rows_mark_already_claimed() {
        let r = json!({"platform":"p","account":"a","ok":false,
                       "data":{"code":10001,"message":"今天已签到"}});
        let rows = from_checkin("manual", &[r]);
        assert_eq!(rows[0].outcome, "already");
    }

    #[test]
    fn http_200_with_business_failure_is_not_success() {
        let r = json!({"platform":"p","account":"a","ok":true,
                       "data":{"ok":false,"status":"风控拦截","msg":"请稍后再试"}});
        let rows = from_checkin("manual", &[r]);
        assert_eq!(rows[0].outcome, "failed");
        assert!(rows[0].detail.contains("风控拦截"));
    }
}
