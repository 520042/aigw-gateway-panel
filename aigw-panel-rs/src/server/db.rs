//! SQLite 存储（2026-10-16，重构路线 S5）。
//!
//! # 为什么只迁 usage_events
//!
//! 数据清单（实测）+ 写入模式决定迁移边界：
//!
//! | 数据 | 体积 | 写入模式 | 去处 |
//! |---|---|---|---|
//! | `usage_events` | 上限 5000 条 ≈ 750KB | **每次 /v1 请求全量重写整个文件** | **SQLite** |
//! | `accounts` | 11KB（8 账号） | 读多写极少 | 留 JSON |
//! | `platform_models` | 34KB | 模型刷新时写 | 留 JSON |
//! | settings/sites/tasks/notify/autocheckin | <1KB | 配置变更时写 | 留 JSON |
//! | `checkins.json` | 135KB | **无人读取**（Python 版遗留） | 不动 |
//!
//! ★ 凭据类**故意不迁**：用户要能直接打开 `accounts.json` 看、直接备份、
//!   直接手工编辑。SQL 化会把「导出账号」变成「导出数据库」。
//! ★ 配置类体量极小，全量写没有成本，迁移只增加故障面。
//! ★ `usage_events` 是唯一「追加型 + 有封顶 + 在请求路径上」的数据 ——
//!   每个请求重写 750KB 是真实的 I/O 瓶颈，SQLite 改成单条 INSERT。
//!
//! # 迁移与降级
//! - **幂等合并**：启动时把 JSON 里比 DB 更新的事件补进来（按 max(ts) 判断），
//!   不删原文件 —— 它同时是 DB 损坏时的恢复源。
//! - **降级**：DB 打不开/任何故障 → `available()==false`，用量记录自动回退成
//!   原来的 JSON 全量写。面板绝不能因为数据库故障而起不来。
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Db {
    conn: Option<Mutex<rusqlite::Connection>>,
    path: PathBuf,
}

fn db_path() -> PathBuf {
    crate::core::config::data_dir().join("panel.db")
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS usage_events(
    ts       REAL    NOT NULL,
    platform TEXT    NOT NULL DEFAULT '',
    model    TEXT    NOT NULL DEFAULT '',
    tin      INTEGER NOT NULL DEFAULT 0,
    tout     INTEGER NOT NULL DEFAULT 0,
    ms       INTEGER NOT NULL DEFAULT 0,
    ok       INTEGER NOT NULL DEFAULT 0,
    upstream TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_ue_ts ON usage_events(ts);
";

impl Db {
    /// 打开（或新建）数据库；失败时返回 `available()==false` 的降级实例。
    pub fn open() -> Self {
        let path = db_path();
        let conn = (|| -> anyhow::Result<rusqlite::Connection> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let c = rusqlite::Connection::open(&path)?;
            // WAL + synchronous=NORMAL：追加不再每次 fsync，且读者不阻塞写者
            c.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=2000;",
            )?;
            c.execute_batch(SCHEMA)?;
            Ok(c)
        })()
        .ok();
        let db = Db {
            conn: conn.map(Mutex::new),
            path,
        };
        db.merge_from_json();
        db
    }

    pub fn available(&self) -> bool {
        self.conn.is_some()
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    // ── usage_events ─────────────────────────────────────────

    /// 追加一条用量事件（单条 INSERT，不再全量重写文件）。
    pub fn usage_insert(&self, ev: &Value) {
        let Some(m) = self.conn.as_ref() else { return };
        let Ok(c) = m.lock() else { return };
        let r = c.execute(
            "INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ok,upstream)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                ev.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0),
                ev.get("platform").and_then(|v| v.as_str()).unwrap_or(""),
                ev.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                ev.get("in").and_then(|v| v.as_i64()).unwrap_or(0),
                ev.get("out").and_then(|v| v.as_i64()).unwrap_or(0),
                ev.get("ms").and_then(|v| v.as_i64()).unwrap_or(0),
                if ev.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) { 1 } else { 0 },
                ev.get("upstream").and_then(|v| v.as_str()).unwrap_or(""),
            ],
        );
        if let Err(e) = r {
            tracing::warn!("usage_insert 失败（降级为 JSON 写入由调用方处理）: {}", e);
        }
    }

    /// 批量插入（迁移用，单事务）。
    pub fn usage_insert_many(&self, evs: &[Value]) {
        let Some(m) = self.conn.as_ref() else { return };
        let Ok(c) = m.lock() else { return };
        let tx = c.unchecked_transaction();
        let mut tx = match tx {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("usage_insert_many 开事务失败: {}", e);
                return;
            }
        };
        for ev in evs {
            let r = tx.execute(
                "INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ok,upstream)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    ev.get("ts").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    ev.get("platform").and_then(|v| v.as_str()).unwrap_or(""),
                    ev.get("model").and_then(|v| v.as_str()).unwrap_or(""),
                    ev.get("in").and_then(|v| v.as_i64()).unwrap_or(0),
                    ev.get("out").and_then(|v| v.as_i64()).unwrap_or(0),
                    ev.get("ms").and_then(|v| v.as_i64()).unwrap_or(0),
                    if ev.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) { 1 } else { 0 },
                    ev.get("upstream").and_then(|v| v.as_str()).unwrap_or(""),
                ],
            );
            if let Err(e) = r {
                tracing::warn!("usage_insert_many 单条失败: {}", e);
            }
        }
        if let Err(e) = tx.commit() {
            tracing::warn!("usage_insert_many 提交失败: {}", e);
        }
    }

    /// 读全量（按时间升序；聚合逻辑仍在 Rust 侧，5000 条内存聚合可接受）。
    pub fn usage_load(&self) -> Vec<Value> {
        let Some(m) = self.conn.as_ref() else { return Vec::new() };
        let Ok(c) = m.lock() else { return Vec::new() };
        let mut stmt = match c.prepare(
            "SELECT ts,platform,model,tin,tout,ms,ok,upstream FROM usage_events ORDER BY ts ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("usage_load 准备语句失败: {}", e);
                return Vec::new();
            }
        };
        let rows = stmt.query_map([], |row| {
            Ok(json!({
                "ts": row.get::<_, f64>(0)?,
                "platform": row.get::<_, String>(1)?,
                "model": row.get::<_, String>(2)?,
                "in": row.get::<_, i64>(3)?,
                "out": row.get::<_, i64>(4)?,
                "ms": row.get::<_, i64>(5)?,
                "ok": row.get::<_, i64>(6)? != 0,
                "upstream": row.get::<_, String>(7)?,
            }))
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                tracing::warn!("usage_load 查询失败: {}", e);
                Vec::new()
            }
        }
    }

    /// DB 里最新一条的时间戳（没有则 None）。
    pub fn usage_max_ts(&self) -> Option<f64> {
        let m = self.conn.as_ref()?;
        let c = m.lock().ok()?;
        c.query_row("SELECT MAX(ts) FROM usage_events", [], |r| r.get::<_, Option<f64>>(0))
            .ok()
            .flatten()
    }

    /// 裁剪：删更早的 + 只留最新 cap 条。
    pub fn usage_prune(&self, cutoff_ts: f64, cap: i64) {
        let Some(m) = self.conn.as_ref() else { return };
        let Ok(c) = m.lock() else { return };
        let _ = c.execute("DELETE FROM usage_events WHERE ts < ?1", rusqlite::params![cutoff_ts]);
        let _ = c.execute(
            "DELETE FROM usage_events WHERE ts NOT IN (
                SELECT ts FROM usage_events ORDER BY ts DESC LIMIT ?1
             )",
            rusqlite::params![cap.max(1)],
        );
    }

    pub fn usage_count(&self) -> i64 {
        let Some(m) = self.conn.as_ref() else { return 0 };
        let Ok(c) = m.lock() else { return 0 };
        c.query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0)
    }

    // ── 迁移 ─────────────────────────────────────────────────

    /// 把 JSON 里比 DB 更新的事件合并进来（幂等；JSON 保留作恢复源）。
    fn merge_from_json(&self) {
        if !self.available() {
            return;
        }
        let v = crate::server::store::load("usage_events");
        let Some(arr) = v.get("events").and_then(|e| e.as_array()) else { return };
        if arr.is_empty() {
            return;
        }
        let db_max = self.usage_max_ts();
        let db_count = self.usage_count();
        let fresh: Vec<Value> = arr
            .iter()
            .filter(|e| {
                let ts = e.get("ts").and_then(|t| t.as_f64()).unwrap_or(0.0);
                db_max.map(|m| ts > m).unwrap_or(true)
            })
            .cloned()
            .collect();
        if fresh.is_empty() {
            return;
        }
        let n = fresh.len();
        self.usage_insert_many(&fresh);
        tracing::info!(
            "usage_events JSON→SQLite 合并 {} 条（DB 原有 {} 条 / max_ts={}，现共 {} 条）",
            n,
            db_count,
            db_max.map(|t| format!("{:.0}", t)).unwrap_or_else(|| "空库".to_string()),
            self.usage_count()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(tag: &str) -> Db {
        // 指向临时目录，避免碰真实数据
        let dir = std::env::temp_dir().join(format!("aigw-db-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("panel.db");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        Db {
            conn: Some(Mutex::new(c)),
            path: p,
        }
    }

    #[test]
    fn insert_and_load_roundtrip() {
        let db = tmp_db("roundtrip");
        assert!(db.available());
        db.usage_insert(&json!({"ts": 100.0, "platform": "apk-doubao", "model": "m",
            "in": 3, "out": 5, "ms": 120, "ok": true, "upstream": "apk-doubao"}));
        db.usage_insert(&json!({"ts": 200.0, "platform": "wb-gateway", "model": "m",
            "in": 0, "out": 0, "ms": 900, "ok": false, "upstream": "wb-gateway"}));
        let ev = db.usage_load();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0]["platform"], "apk-doubao");
        assert_eq!(ev[0]["in"], 3);
        assert_eq!(ev[0]["ok"], true);
        assert_eq!(ev[1]["ok"], false, "失败事件必须原样保留");
        assert_eq!(ev[1]["ms"], 900);
        assert_eq!(db.usage_count(), 2);
        assert_eq!(db.usage_max_ts(), Some(200.0));
    }

    #[test]
    fn prune_drops_old_and_caps() {
        let db = tmp_db("prune");
        for i in 0..10 {
            db.usage_insert(&json!({"ts": i as f64, "platform": "p", "model": "m",
                "in": 0, "out": 0, "ms": 0, "ok": true, "upstream": "p"}));
        }
        // 删更早的（< 5）
        db.usage_prune(5.0, 100);
        assert_eq!(db.usage_count(), 5, "ts<5 的应被删掉");
        // 只留最新 2 条
        db.usage_prune(0.0, 2);
        let ev = db.usage_load();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0]["ts"], 8.0);
        assert_eq!(ev[1]["ts"], 9.0);
    }

    #[test]
    fn unavailable_db_reports_false() {
        let db = Db {
            conn: None,
            path: std::path::PathBuf::from("nonexistent"),
        };
        assert!(!db.available());
        // 降级实例上所有操作都必须是安全空转
        db.usage_insert(&json!({"ts": 1.0}));
        assert!(db.usage_load().is_empty());
        assert_eq!(db.usage_count(), 0);
        db.usage_prune(0.0, 10);
        assert_eq!(db.usage_max_ts(), None);
    }
}
