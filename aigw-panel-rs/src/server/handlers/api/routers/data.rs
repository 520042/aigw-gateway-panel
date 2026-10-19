//! 数据备份与恢复（D 需求）。
//!
//! 备份 = 把数据目录里的关键文件快照到 `data/backups/<UTC时间戳>/`
//! （目录快照，零新依赖、无压缩损坏风险），并写 `manifest.json`。
//! 恢复 = 把某次备份拷回 `data/`，随后需重启面板让内存态与磁盘一致。
//!
//! 端点：
//!   POST /api/data/backup        立即备份，返回 {ok,id,ts,files}
//!   GET  /api/data/backups       列出已有备份
//!   POST /api/data/restore?ts=xx 恢复指定备份，返回 {ok,need_restart:true}

use crate::core::config;
use axum::{extract::Query, response::IntoResponse, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// 纳入备份的数据文件（accounts.json 含未脱敏凭据；panel.db 为用量 SQLite）。
const DATA_FILES: &[&str] = &[
    "settings.json",
    "accounts.json",
    "panel.db",
    "disabled_models.json",
    "sites.json",
    "checkins.json",
    "tasks.json",
    "notify.json",
];

fn backups_dir() -> std::path::PathBuf {
    config::data_dir().join("backups")
}

/// 立即备份：复制 DATA_FILES 中存在的文件到 backups/<ts>/，写 manifest。
pub async fn data_backup() -> impl IntoResponse {
    let data = config::data_dir();
    let backups = backups_dir();
    if let Err(e) = std::fs::create_dir_all(&backups) {
        return Json(json!({"ok": false, "message": format!("建备份目录失败：{e}")}));
    }
    // UTC 时间戳（避免本地时区/夏令时歧义）
    let ts = chrono_ts();
    let dest = backups.join(&ts);
    if let Err(e) = std::fs::create_dir_all(&dest) {
        return Json(json!({"ok": false, "message": format!("建备份子目录失败：{e}")}));
    }
    let mut copied: Vec<String> = Vec::new();
    for name in DATA_FILES {
        let src = data.join(name);
        if src.is_file() {
            if let Err(e) = std::fs::copy(&src, dest.join(name)) {
                return Json(json!({"ok": false, "message": format!("复制 {} 失败：{e}", name)}));
            }
            copied.push(name.to_string());
        }
    }
    let manifest = json!({
        "id": ts,
        "created_at": ts,
        "files": copied,
        "app": "aigw-panel-rs",
    });
    if let Ok(s) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::write(dest.join("manifest.json"), s);
    }
    Json(json!({
        "ok": true,
        "id": ts,
        "ts": ts,
        "files": copied,
        "message": format!("已备份 {} 个文件到 backups/{}", copied.len(), ts),
    }))
}

/// 列出已有备份（按 manifest 排序，最新在前）。
pub async fn data_backups() -> impl IntoResponse {
    let backups = backups_dir();
    let mut list: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&backups) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let mut item = json!({
                "id": p.file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default(),
                "files": Vec::<String>::new(),
                "created_at": null,
            });
            let mf = p.join("manifest.json");
            if let Ok(s) = std::fs::read_to_string(&mf) {
                if let Ok(v) = serde_json::from_str::<Value>(&s) {
                    if let Some(f) = v.get("files").and_then(|x| x.as_array()) {
                        item["files"] = json!(
                            f.iter()
                                .map(|x| x.as_str().unwrap_or("").to_string())
                                .collect::<Vec<_>>()
                        );
                    }
                    if let Some(c) = v.get("created_at").and_then(|x| x.as_str()) {
                        item["created_at"] = json!(c);
                    }
                }
            }
            // 兜底：没有 manifest 也列出来（统计目录内文件数）
            if item["files"].as_array().map(|x| x.is_empty()).unwrap_or(true) {
                if let Ok(files) = std::fs::read_dir(&p) {
                    let n = files
                        .flatten()
                        .filter(|x| x.file_type().map(|t| t.is_file()).unwrap_or(false))
                        .count();
                    item["files"] = json!(vec!["…".to_string(); n]);
                }
            }
            list.push(item);
        }
    }
    // 最新在前
    list.sort_by(|a, b| {
        b.get("created_at")
            .and_then(|x| x.as_str())
            .cmp(&a.get("created_at").and_then(|x| x.as_str()))
    });
    Json(json!({"ok": true, "list": list}))
}

/// 恢复指定备份：把 backups/<ts> 的文件拷回 data/，返回需重启标记。
pub async fn data_restore(q: Query<HashMap<String, String>>) -> impl IntoResponse {
    let ts = q.get("ts").map(|s| s.trim().to_string()).unwrap_or_default();
    if ts.is_empty() {
        return Json(json!({"ok": false, "message": "缺少 ts 参数"}));
    }
    // 防目录穿越：ts 必须是单纯子目录名
    if !is_safe_name(&ts) {
        return Json(json!({"ok": false, "message": "非法的备份标识"}));
    }
    let src = backups_dir().join(&ts);
    if !src.is_dir() {
        return Json(json!({"ok": false, "message": "备份不存在"}));
    }
    let data = config::data_dir();
    if let Err(e) = std::fs::create_dir_all(&data) {
        return Json(json!({"ok": false, "message": format!("建数据目录失败：{e}")}));
    }
    let mut restored: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(&src) {
        Ok(e) => e,
        Err(e) => {
            return Json(json!({"ok": false, "message": format!("读备份失败：{e}")}));
        }
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == "manifest.json" {
            continue;
        }
        if let Err(e) = std::fs::copy(&p, data.join(&name)) {
            return Json(json!({"ok": false, "message": format!("恢复 {} 失败：{e}", name)}));
        }
        restored.push(name);
    }
    Json(json!({
        "ok": true,
        "restored": restored,
        "need_restart": true,
        "message": format!("已恢复 {} 个文件，请重启面板生效", restored.len()),
    }))
}

/// 简易 UTC 时间戳（形如 20261016T103500Z），无需引入 chrono 依赖。
fn chrono_ts() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = utc_from_secs(now);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// 把 Unix 秒拆成 UTC 日历分量（避免 chrono 依赖，简单除法足够备份命名用）。
fn utc_from_secs(sec: u64) -> (u64, u64, u64, u64, u64, u64) {
    let days = sec / 86400;
    let rem = sec % 86400;
    let h = rem / 3600;
    let rem = rem % 3600;
    let mi = rem / 60;
    let s = rem % 60;
    // 由天数推年月日（经典 civil 算法）
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 365 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m as u64, d as u64, h, mi, s)
}

/// 备份标识只允许安全字符，杜绝路径穿越。
fn is_safe_name(s: &str) -> bool {
    !s.is_empty()
        && Path::new(s)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == 'T' || c == 'Z')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_rejects_traversal() {
        assert!(is_safe_name("20261016T103500Z"));
        assert!(!is_safe_name("../etc"));
        assert!(!is_safe_name("a/b"));
        assert!(!is_safe_name(".."));
    }

    #[test]
    fn ts_roundtrip_components() {
        // 2026-01-01T00:00:00Z ≈ 1767225600
        let (y, mo, _d, _h, _mi, _s) = utc_from_secs(1767225600);
        assert_eq!(y, 2026);
        assert_eq!(mo, 1);
    }
}
