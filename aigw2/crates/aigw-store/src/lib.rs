//! aigw-store：SQLite(WAL) 用量存储 + 旧版数据只读导入。
//!
//! 机制对齐旧项目 S5（db.rs/usage.rs，2026-10-16 沉淀）：
//! - `panel.db`：WAL + synchronous=NORMAL + busy_timeout=2000；
//! - 单条 INSERT，每 500 条触发 prune（90 天 / 5000 条上限）；
//! - DB 不可用时整体降级为 JSON 全量重写（内存 + `usage_events.json`）；
//! - `import_legacy`：从旧版数据目录**只读导入** usage（不动旧目录，旧版并行期安全）。

pub mod accounts;
pub use accounts::{Account, AccountStore};

pub mod custom;
pub use custom::{CustomApi, CustomStore};

pub mod free;
pub use free::{FreeSite, FreeStore};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const PRUNE_EVERY: u32 = 500;
const CAP_DAYS: f64 = 90.0;
const CAP_EVENTS: i64 = 5000;

/// 一条用量事件。内部字段 tin/tout；旧版 JSON 里叫 `in`/`out`（serde alias 兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub ts: f64,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub model: String,
    #[serde(default, alias = "in")]
    pub tin: i64,
    #[serde(default, alias = "out")]
    pub tout: i64,
    #[serde(default)]
    pub ms: i64,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub upstream: String,
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

enum Backend {
    Db(Mutex<Connection>),
    /// 降级：内存 + JSON 全量重写
    Json {
        mem: Mutex<Vec<UsageEvent>>,
        path: PathBuf,
    },
}

pub struct Store {
    backend: Backend,
    since_prune: AtomicU32,
}

impl Store {
    /// 打开数据目录（不存在则创建）；panel.db 打不开则降级 JSON。
    pub fn open(data_dir: &Path) -> Store {
        let _ = std::fs::create_dir_all(data_dir);
        let db_path = data_dir.join("panel.db");
        let backend = match Connection::open(&db_path) {
            Ok(c) => {
                let init = c.execute_batch(
                    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=2000;",
                );
                let schema = c.execute_batch(SCHEMA);
                match (init, schema) {
                    (Ok(_), Ok(_)) => {
                        tracing::info!("store：SQLite 就绪（{}）", db_path.display());
                        Backend::Db(Mutex::new(c))
                    }
                    (i, s) => {
                        tracing::warn!("store：SQLite 初始化失败 init={i:?} schema={s:?}，降级 JSON");
                        Backend::Json {
                            mem: Mutex::new(Vec::new()),
                            path: data_dir.join("usage_events.json"),
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("store：SQLite 打开失败（{e}），降级 JSON");
                Backend::Json {
                    mem: Mutex::new(Vec::new()),
                    path: data_dir.join("usage_events.json"),
                }
            }
        };
        Store {
            backend,
            since_prune: AtomicU32::new(0),
        }
    }

    pub fn is_db(&self) -> bool {
        matches!(self.backend, Backend::Db(_))
    }

    /// 记录一条用量。每 PRUNE_EVERY 条触发一次 prune。
    pub fn usage_record(&self, ev: UsageEvent) {
        match &self.backend {
            Backend::Db(conn) => {
                if let Ok(c) = conn.lock() {
                    let _ = c.execute(
                        "INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ok,upstream)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                        rusqlite::params![
                            ev.ts,
                            ev.platform,
                            ev.model,
                            ev.tin,
                            ev.tout,
                            ev.ms,
                            ev.ok as i64,
                            ev.upstream
                        ],
                    );
                }
                if self.since_prune.fetch_add(1, Ordering::Relaxed) + 1 >= PRUNE_EVERY {
                    self.since_prune.store(0, Ordering::Relaxed);
                    self.usage_prune();
                }
            }
            Backend::Json { mem, path } => {
                if let Ok(mut list) = mem.lock() {
                    list.push(ev.clone());
                    let cutoff = ev.ts - CAP_DAYS * 86400.0;
                    list.retain(|x| x.ts >= cutoff);
                    if list.len() as i64 > CAP_EVENTS {
                        let over = list.len() as i64 - CAP_EVENTS;
                        list.drain(0..over as usize);
                    }
                    let payload = json!({ "events": list.iter().collect::<Vec<_>>() });
                    if let Ok(mut f) = std::fs::File::create(path) {
                        let _ = f
                            .write_all(serde_json::to_string_pretty(&payload).unwrap_or_default().as_bytes());
                    } else {
                        tracing::warn!("store：降级 JSON 写入失败（{}）", path.display());
                    }
                }
            }
        }
    }

    fn usage_prune(&self) {
        if let Backend::Db(conn) = &self.backend {
            if let Ok(c) = conn.lock() {
                let cutoff = now_unix() - CAP_DAYS * 86400.0;
                let _ = c.execute("DELETE FROM usage_events WHERE ts < ?1", [cutoff]);
                let _ = c.execute(
                    "DELETE FROM usage_events WHERE ts NOT IN (
                        SELECT ts FROM usage_events ORDER BY ts DESC LIMIT ?1)",
                    [CAP_EVENTS],
                );
            }
        }
    }

    fn usage_max_ts(&self) -> f64 {
        match &self.backend {
            Backend::Db(conn) => {
                conn.lock()
                    .ok()
                    .and_then(|c| {
                        c.query_row("SELECT MAX(ts) FROM usage_events", [], |r| {
                            r.get::<_, Option<f64>>(0)
                        })
                        .ok()
                    })
                    .flatten()
                    .unwrap_or(0.0)
            }
            Backend::Json { mem, .. } => {
                mem.lock()
                    .map(|l| l.iter().map(|e| e.ts).fold(0.0_f64, f64::max))
                    .unwrap_or(0.0)
            }
        }
    }

    /// 全量读取（升序）。
    pub fn usage_all(&self) -> Vec<UsageEvent> {
        match &self.backend {
            Backend::Db(conn) => {
                let mut out = Vec::new();
                if let Ok(c) = conn.lock() {
                    if let Ok(mut stmt) =
                        c.prepare("SELECT ts,platform,model,tin,tout,ms,ok,upstream FROM usage_events ORDER BY ts ASC")
                    {
                        let _ = stmt
                            .query_map([], |r| {
                                Ok(UsageEvent {
                                    ts: r.get(0)?,
                                    platform: r.get(1)?,
                                    model: r.get(2)?,
                                    tin: r.get(3)?,
                                    tout: r.get(4)?,
                                    ms: r.get(5)?,
                                    ok: r.get::<_, i64>(6)? != 0,
                                    upstream: r.get(7)?,
                                })
                            })
                            .map(|rows| {
                                for row in rows.flatten() {
                                    out.push(row);
                                }
                            });
                    }
                }
                out
            }
            Backend::Json { mem, .. } => mem.lock().map(|l| l.clone()).unwrap_or_default(),
        }
    }

    /// 最近 N 条请求（ts 倒序，M6 请求历史）。
    pub fn usage_recent(&self, limit: usize) -> Vec<UsageEvent> {
        let mut all = self.usage_all();
        all.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(limit);
        all
    }

    /// 按平台聚合（M6 成本归因雏形：调用数/成败/token）。
    pub fn usage_by_platform(&self) -> serde_json::Value {
        let evs = self.usage_all();
        let mut agg: std::collections::BTreeMap<String, serde_json::Value> = Default::default();
        for e in &evs {
            let d = agg
                .entry(e.platform.clone())
                .or_insert_with(|| json!({"requests":0,"success":0,"failed":0,"tokensIn":0,"tokensOut":0}));
            d["requests"] = json!(d["requests"].as_i64().unwrap_or(0) + 1);
            if e.ok {
                d["success"] = json!(d["success"].as_i64().unwrap_or(0) + 1);
            } else {
                d["failed"] = json!(d["failed"].as_i64().unwrap_or(0) + 1);
            }
            d["tokensIn"] = json!(d["tokensIn"].as_i64().unwrap_or(0) + e.tin);
            d["tokensOut"] = json!(d["tokensOut"].as_i64().unwrap_or(0) + e.tout);
        }
        json!(agg)
    }

    /// 汇总（前端用量页 M1 简版）。
    pub fn usage_totals(&self) -> serde_json::Value {        let evs = self.usage_all();
        let (mut req, mut ok_n, mut fail_n, mut ti, mut to) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for e in &evs {
            req += 1;
            if e.ok {
                ok_n += 1;
            } else {
                fail_n += 1;
            }
            ti += e.tin.max(0) as u64;
            to += e.tout.max(0) as u64;
        }
        json!({
            "requests": req, "success": ok_n, "failed": fail_n,
            "tokensIn": ti, "tokensOut": to,
            "db": self.is_db(),
        })
    }

    /// 从旧版数据目录**只读导入** usage（panel.db + usage_events.json）。
    /// 幂等：只导 `ts > 当前库 max_ts` 的事件；重复调用自动跳过。
    pub fn import_legacy(&self, legacy_dir: &Path) -> Result<ImportReport, String> {
        let max_ts = self.usage_max_ts();
        let mut events: Vec<UsageEvent> = Vec::new();
        let mut seen: std::collections::HashSet<(u64, String, String)> = Default::default();
        let key = |e: &UsageEvent| ((e.ts * 1000.0) as u64, e.platform.clone(), e.model.clone());

        // 1) 旧 panel.db（只读；表不存在/打不开则跳过）
        let db_path = legacy_dir.join("panel.db");
        if db_path.exists() {
            match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                Ok(conn) => {
                    let stmt = conn.prepare(
                        "SELECT ts,platform,model,tin,tout,ms,ok,upstream FROM usage_events WHERE ts > ?1 ORDER BY ts ASC",
                    );
                    if let Ok(mut stmt) = stmt {
                        let rows = stmt.query_map([max_ts], |r| {
                            Ok(UsageEvent {
                                ts: r.get(0)?,
                                platform: r.get(1)?,
                                model: r.get(2)?,
                                tin: r.get(3)?,
                                tout: r.get(4)?,
                                ms: r.get(5)?,
                                ok: r.get::<_, i64>(6)? != 0,
                                upstream: r.get(7)?,
                            })
                        });
                        if let Ok(rows) = rows {
                            for row in rows.flatten() {
                                if seen.insert(key(&row)) {
                                    events.push(row);
                                }
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!("import：旧 panel.db 只读打开失败（跳过）：{e}"),
            }
        }

        // 2) 旧 usage_events.json（{"events":[{ts,platform,model,in,out,ms,ok,upstream}]}）
        let json_path = legacy_dir.join("usage_events.json");
        if json_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&json_path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(list) = v.get("events").and_then(|x| x.as_array()) {
                        for item in list {
                            if let Ok(e) = serde_json::from_value::<UsageEvent>(item.clone()) {
                                if e.ts > max_ts && seen.insert(key(&e)) {
                                    events.push(e);
                                }
                            }
                        }
                    }
                }
            }
        }

        let report = ImportReport {
            imported: events.len(),
            from_max_ts: max_ts,
        };
        for e in events {
            self.usage_record(e);
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub imported: usize,
    pub from_max_ts: f64,
}

pub fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: f64, model: &str, ok: bool) -> UsageEvent {
        UsageEvent {
            ts,
            platform: "anon-zen".into(),
            model: model.into(),
            tin: 11,
            tout: 22,
            ms: 123,
            ok,
            upstream: "opencode.ai".into(),
        }
    }

    #[test]
    fn open_record_totals_db() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        assert!(st.is_db());
        st.usage_record(ev(1000.0, "m-1", true));
        st.usage_record(ev(1001.0, "m-1", false));
        let t = st.usage_totals();
        assert_eq!(t["requests"], 2);
        assert_eq!(t["success"], 1);
        assert_eq!(t["failed"], 1);
        assert_eq!(t["tokensOut"], 44);
    }

    #[test]
    fn json_fallback_when_db_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = tmp.path().to_path_buf();
        // panel.db 位置被一个目录占用 → SQLite 打不开 → 降级 JSON（目录本身可写）
        std::fs::create_dir_all(dd.join("panel.db")).unwrap();
        let st = Store::open(&dd);
        assert!(!st.is_db());
        st.usage_record(ev(2000.0, "m-2", true));
        let t = st.usage_totals();
        assert_eq!(t["requests"], 1);
        assert_eq!(t["db"], false);
        // JSON 已落盘
        assert!(dd.join("usage_events.json").exists());
    }

    #[test]
    fn import_legacy_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();
        // 造旧库：2 行
        {
            let c = Connection::open(legacy.path().join("panel.db")).unwrap();
            c.execute_batch(SCHEMA).unwrap();
            for (ts, ok) in [(500.0, 1i64), (600.0, 0i64)] {
                c.execute(
                    "INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ok,upstream) VALUES(?1,'old','gpt',1,2,3,?2,'x')",
                    rusqlite::params![ts, ok],
                )
                .unwrap();
            }
        }
        // 造旧 JSON：1 行更新的（ts=700 > DB max 600）
        std::fs::write(
            legacy.path().join("usage_events.json"),
            r#"{"events":[{"ts":700.0,"platform":"old","model":"gpt","in":5,"out":6,"ms":7,"ok":true,"upstream":"x"}]}"#,
        )
        .unwrap();

        let st = Store::open(tmp.path());
        let r1 = st.import_legacy(legacy.path()).unwrap();
        assert_eq!(r1.imported, 3, "db 2 行 + json 1 行");
        assert_eq!(st.usage_totals()["requests"], 3);

        // 再导一次：max_ts 已到 700 → 幂等 0 条
        let r2 = st.import_legacy(legacy.path()).unwrap();
        assert_eq!(r2.imported, 0);
        assert_eq!(st.usage_totals()["requests"], 3);
    }

    #[test]
    fn import_legacy_without_db_uses_json_only() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tempfile::tempdir().unwrap();
        std::fs::write(
            legacy.path().join("usage_events.json"),
            r#"{"events":[{"ts":9.0,"platform":"p","model":"m","in":1,"out":2,"ms":3,"ok":false,"upstream":"u"}]}"#,
        )
        .unwrap();
        let st = Store::open(tmp.path());
        let r = st.import_legacy(legacy.path()).unwrap();
        assert_eq!(r.imported, 1);
        let all = st.usage_all();
        assert_eq!(all[0].tin, 1);
        assert!(!all[0].ok);
    }
}
