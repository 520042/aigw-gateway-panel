//! aigw-store：SQLite(WAL) 用量存储 + 旧版数据只读导入。
//!
//! 机制对齐旧项目 S5（db.rs/usage.rs，2026-10-16 沉淀）：
//! - `panel.db`：WAL + synchronous=NORMAL + busy_timeout=2000；
//! - 单条 INSERT，每 500 条触发 prune（90 天 / 5000 条上限）；
//! - DB 不可用时整体降级为 JSON 全量重写（内存 + `usage_events.json`）；
//! - `import_legacy`：从旧版数据目录**只读导入** usage（不动旧目录，旧版并行期安全）。

pub mod archive;
pub mod atomic;
pub use atomic::{write_atomic, write_json_atomic};

pub mod rotlog;

pub mod secret;

pub mod accounts;
pub use accounts::{Account, AccountStore};

pub mod custom;
pub use custom::{mask_key, CustomApi, CustomStore};

pub mod flags;
pub use flags::{bare_model, FlagStore, Flags};

pub mod free;
pub use free::{FreeSite, FreeStore};

pub mod keys;
pub use keys::{AccessKey, KeyStore};

pub mod combos;
pub use combos::{Combo, ComboStore};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

const PRUNE_EVERY: u32 = 500;
const CAP_DAYS: f64 = 90.0;
/// 台账最多保留多少条事件（超过按 rowid 淘汰最旧的）。
/// `pub` 是为了让上层（/api/requests 的 offset 夹取）用同一个数，不另写一份魔数。
pub const CAP_EVENTS: i64 = 5000;

/// 一条用量事件。内部字段 tin/tout；旧版 JSON 里叫 `in`/`out`（serde alias 兼容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub ts: f64,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub model: String,
    /// 账号名（人工可读）。旧台账没有这一列/这一字段，默认空串。
    #[serde(default)]
    pub account: String,
    #[serde(default, alias = "in")]
    pub tin: i64,
    #[serde(default, alias = "out")]
    pub tout: i64,
    #[serde(default)]
    pub ms: i64,
    /// ★ 2026-10-26（P3）：首字节耗时（TTFB，毫秒）。流式请求才有意义——
    /// 它衡量「上游多久开始出字」，与 ms（整轮耗时）是两个不同的体验指标：
    /// 一个大模型推理 30 秒但 0.8 秒就出第一个字，用户感知远好于 8 秒才出字。
    /// 0 = 未测（非流式或旧台账）。
    #[serde(default)]
    pub ttfb: i64,
    #[serde(default)]
    pub ok: bool,
    /// 这次请求是**客户端主动取消**的（用户点停止、客户端超时断开）。
    ///
    /// 台账上它仍然 ok=false（上游确实没跑完，成功率不该被美化），但要单列：
    /// 否则用户每点一次停止，那个平台在「近 24h 失败率」里就多黑一格，
    /// 把「用户自己的操作」显示成「平台坏了」。
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub upstream: String,
    /// 这条记录属于哪一类动作：`chat`（正常对话，默认）或 `checkin` / `growth` 等
    /// 后台任务。
    ///
    /// ★ 为什么必须有：后台任务也往同一张台账里记一行（model 写成 "checkin"），
    ///   如果不打标，它会以「模型」的身份出现在模型榜上，把真模型挤出前 N 名。
    ///   旧库没有这一列，迁移时一律回填成 chat，再把历史上 model 就是动作名的
    ///   行改标出来。
    #[serde(default)]
    pub kind: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS usage_events(
    ts       REAL    NOT NULL,
    platform TEXT    NOT NULL DEFAULT '',
    model    TEXT    NOT NULL DEFAULT '',
    account  TEXT    NOT NULL DEFAULT '',
    tin      INTEGER NOT NULL DEFAULT 0,
    tout     INTEGER NOT NULL DEFAULT 0,
    ms       INTEGER NOT NULL DEFAULT 0,
    -- ★ P3：首字节耗时（TTFB 毫秒）。旧库没有这列，load 时按需 ALTER。
    ttfb     INTEGER NOT NULL DEFAULT 0,
    ok        INTEGER NOT NULL DEFAULT 0,
    cancelled INTEGER NOT NULL DEFAULT 0,
    upstream  TEXT    NOT NULL DEFAULT '',
    kind      TEXT    NOT NULL DEFAULT 'chat'
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

/// 构造 JSON 降级后端，并**把已有 usage_events.json 读进内存**。
///
/// ★ 旧实现恒用 `Mutex::new(Vec::new())` 且从不读盘，带来两个连锁问题：
/// ① 重启后第一条 usage_record 就用「1 条」全量覆盖文件 → 降级模式的历史只剩最后 1 条；
/// ② `usage_max_ts` 从内存取到 0.0 → `import_legacy` 的 `ts > max_ts` 幂等门失效，
///    每次启动都把旧目录全量重复导入，台账成倍膨胀。
fn json_backend(data_dir: &Path) -> Backend {
    let path = data_dir.join("usage_events.json");
    let mem: Vec<UsageEvent> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            // 兼容 {"events":[...]} 与裸数组两种历史格式
            let arr = v.get("events").cloned().unwrap_or(v);
            serde_json::from_value::<Vec<UsageEvent>>(arr).ok()
        })
        .unwrap_or_default();
    Backend::Json {
        mem: Mutex::new(mem),
        path,
    }
}

/// 把一行 `usage_events` 映射成事件（usage_all / usage_recent 共用，避免两处
/// 列序漂移——列序写错只会读到错的值而不会报错，是最难发现的一类 bug）。
fn map_usage_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UsageEvent> {
    Ok(UsageEvent {
        ts: r.get(0)?,
        platform: r.get(1)?,
        model: r.get(2)?,
        account: r.get(9)?,
        tin: r.get(3)?,
        tout: r.get(4)?,
        ms: r.get(5)?,
        // ★ P3：列序 ms → ttfb → ok → upstream → account → cancelled → kind
        //（SELECT 已同步改成 ms,ttfb,ok,…；索引必须一一对齐——列序写错只会读到
        //  错值而不报错，是最难发现的一类 bug）
        ttfb: r.get(6)?,
        ok: r.get::<_, i64>(7)? != 0,
        upstream: r.get(8)?,
        cancelled: r.get::<_, i64>(10)? != 0,
        kind: r.get(11)?,
    })
}

/// 窗口内的分组维度（列名是编译期白名单，绝不吃外部输入）。
#[derive(Clone, Copy)]
enum GroupBy {
    Platform,
    Model,
    Account,
}

impl GroupBy {
    fn col(self) -> &'static str {
        match self {
            GroupBy::Platform => "platform",
            GroupBy::Model => "model",
            GroupBy::Account => "account",
        }
    }
}

/// 记录的动作类型（空串 = 正常对话）。
///
/// 单独抽出来是因为写库与写 JSON 两条路径都要用，而「空串」在 JSON 降级文件里
/// 与 undefined 同义，写库时必须落成明确的值。
pub fn kind_of(ev: &UsageEvent) -> String {
    if ev.kind.trim().is_empty() {
        "chat".to_string()
    } else {
        ev.kind.trim().to_string()
    }
}

/// 给已有库补一列（老版本 panel.db 没有 account 列，而 `CREATE TABLE IF NOT EXISTS`
/// 对已存在的表什么都不做）：不补就会在 INSERT 时整条台账写失败。
fn ensure_column(c: &Connection, table: &str, col: &str, decl: &str) -> bool {
    let mut has = false;
    if let Ok(mut stmt) = c.prepare(&format!("PRAGMA table_info({table})")) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) {
            for name in rows.flatten() {
                if name == col {
                    has = true;
                }
            }
        }
    }
    if has {
        return true;
    }
    match c.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"), []) {
        Ok(_) => {
            tracing::info!("store：已为 {table} 补列 {col}");
            true
        }
        Err(e) => {
            tracing::warn!("store：为 {table} 补列 {col} 失败（{e}）");
            false
        }
    }
}
/// ★ 2026-10-26（P3）：延迟分位统计（P50 / P95 / 平均 / 样本数）。
/// 入参会就地排序；空样本回全 0（前端显示「暂无」而不是 NaN）。
fn percentiles(v: &mut Vec<i64>) -> serde_json::Value {
    if v.is_empty() {
        return json!({"p50": 0, "p95": 0, "avg": 0, "samples": 0});
    }
    v.sort_unstable();
    let pick = |p: f64| -> i64 {
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[idx.min(v.len() - 1)]
    };
    let avg = (v.iter().sum::<i64>() as f64 / v.len() as f64).round() as i64;
    json!({"p50": pick(0.50), "p95": pick(0.95), "avg": avg, "samples": v.len()})
}

impl Store {
    /// 打开数据目录（不存在则创建）；panel.db 打不开则降级 JSON。
    pub fn open(data_dir: &Path) -> Store {
        let _ = std::fs::create_dir_all(data_dir);
        // 上次恢复备份时留下的 panel.db*.restore-pending 在这里落地（此时还没有
        // 任何连接打开该文件，替换必然成功）。
        Store::apply_pending_restore(data_dir);
        let db_path = data_dir.join("panel.db");
        let backend = match Connection::open(&db_path) {
            Ok(c) => {
                let init = c.execute_batch(
                    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=2000;",
                );
                let schema = c.execute_batch(SCHEMA);
                match (init, schema) {
                    (Ok(_), Ok(_)) => {
                        ensure_column(&c, "usage_events", "account", "TEXT NOT NULL DEFAULT ''");
                        ensure_column(
                            &c,
                            "usage_events",
                            "cancelled",
                            "INTEGER NOT NULL DEFAULT 0",
                        );
                        // ★ P3：TTFB 列（老库自动补，新库建表时已含）
                        ensure_column(
                            &c,
                            "usage_events",
                            "ttfb",
                            "INTEGER NOT NULL DEFAULT 0",
                        );
                        // 动作类型：老库补列时默认回填 chat（它们都是那次改版之前的
                        // 对话记录），再把「model 就是动作名」的历史行改正过来。
                        ensure_column(&c, "usage_events", "kind", "TEXT NOT NULL DEFAULT 'chat'");
                        let _ = c.execute(
                            "UPDATE usage_events SET kind='checkin'
                              WHERE kind='chat' AND model='checkin'",
                            [],
                        );
                        tracing::info!("store：SQLite 就绪（{}）", db_path.display());
                        Backend::Db(Mutex::new(c))
                    }
                    (i, s) => {
                        tracing::warn!("store：SQLite 初始化失败 init={i:?} schema={s:?}，降级 JSON");
                        json_backend(data_dir)
                    }
                }
            }
            Err(e) => {
                tracing::warn!("store：SQLite 打开失败（{e}），降级 JSON");
                json_backend(data_dir)
            }
        };
        Store {
            backend,
            since_prune: AtomicU32::new(0),
        }
    }

    /// 应用「恢复备份时暂存的数据库文件」。
    ///
    /// ★ 为什么需要：面板在跑的时候 panel.db 被 SQLite 独占打开，Windows 上覆盖写
    ///   必然失败。旧实现只能把失败写进 skipped，用户重启后发现用量台账还是旧的。
    ///   现在恢复接口把 panel.db* 写成 `<name>.restore-pending`，下次 Store::open
    ///   之前（服务启动时）在这里原子替换掉真文件，`restart_required: true` 就真的
    ///   意味着「重启后生效」。
    ///
    /// 替换 panel.db 时必须连带删掉旧的 -wal / -shm：否则新主库配旧 WAL 会读到
    /// 完全不相干的页（SQLite 只校验 WAL 头里的 salt，不校验主库内容）。
    /// 返回实际应用的数据库文件个数。
    pub fn apply_pending_restore(data_dir: &Path) -> usize {
        const DB_FILES: &[&str] = &["panel.db", "panel.db-wal", "panel.db-shm"];
        let mut staged: Vec<&str> = Vec::new();
        for n in DB_FILES {
            if data_dir.join(format!("{n}.restore-pending")).exists() {
                staged.push(n);
            }
        }
        if staged.is_empty() {
            return 0;
        }
        // 顺序很关键：必须先把 panel.db 换好，成功之后才能删旧的 -wal/-shm。
        // 反过来（先删后换）如果 rename 失败，旧主库配上被删掉的 WAL = 台账直接丢一半。
        let mut applied = 0usize;
        let mut db_replaced = false;
        for n in staged {
            let src = data_dir.join(format!("{n}.restore-pending"));
            let dst = data_dir.join(n);
            // 换主库时先清掉旧日志：SQLite 只校验 WAL 头里的 salt，不校验主库内容，
            // 新主库配旧 WAL 会读到完全不相干的页。
            if n == "panel.db" {
                for stale in ["panel.db-wal", "panel.db-shm"] {
                    if data_dir.join(format!("{stale}.restore-pending")).exists() {
                        continue; // 这个旧日志马上会被暂存版本替换，别在这里删
                    }
                    let _ = std::fs::remove_file(data_dir.join(stale));
                }
            }
            match std::fs::rename(&src, &dst) {
                Ok(_) => {
                    applied += 1;
                    if n == "panel.db" {
                        db_replaced = true;
                    }
                    tracing::info!("store：已应用恢复备份里的 {n}");
                }
                Err(e) => tracing::warn!("store：应用恢复备份里的 {n} 失败（{e}），保留 .restore-pending 待下次启动重试"),
            }
        }
        let _ = db_replaced;
        applied
    }

    pub fn is_db(&self) -> bool {
        matches!(self.backend, Backend::Db(_))
    }

    /// 台账事件条数（health / 「关于」页数据占用展示用）。
    ///
    /// 有 90 天 / 5000 条的 prune 上限，所以这个 COUNT 永远不会大到拖慢请求；
    /// JSON 降级后端直接数内存里的条数。
    pub fn usage_count(&self) -> u64 {
        match &self.backend {
            Backend::Db(conn) => {
                let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                c.query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get::<_, i64>(0))
                    .unwrap_or(0)
                    .max(0) as u64
            }
            Backend::Json { mem, .. } => {
                mem.lock().unwrap_or_else(|e| e.into_inner()).len() as u64
            }
        }
    }

    /// 把 WAL 内容并回主库并把 `panel.db-wal` 截断为 0 字节。
    ///
    /// ★ 为什么必须显式做：WAL 模式下新写入先落 `-wal`，**主库文件本身可以长期停在
    /// 初始的 4KB 空壳**。实测部署目录 `panel.db`=4096 字节而 `panel.db-wal`=1.8MB
    /// —— 此时只拷 `panel.db` 得到的是一个没有任何台账的空库，恢复回去还以为
    /// 「备份好了」，重启后用量历史全部消失。
    ///
    /// TRUNCATE 档位会把 WAL 完整并回主库并把 `-wal` 截到 0 字节，之后单拷主库即为
    /// 完整快照。返回 false 表示没做成（JSON 降级后端、连接被占、SQLite 报错）：
    /// 调用方应把 `-wal` 一并导出兜底，而不是假装备份完整。
    pub fn checkpoint(&self) -> bool {
        let Backend::Db(conn) = &self.backend else {
            return false; // JSON 降级后端没有 WAL
        };
        // 锁中毒不能让「备份前 checkpoint」静默返回 false（调用方会连带导出 -wal 兜底，
        // 但那只是补救；真正该做的是继续用连接）。
        let c = conn.lock().unwrap_or_else(|e| e.into_inner());
        match c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0)) {
            Ok(0) => true,
            Ok(busy) => {
                tracing::warn!("store：wal_checkpoint 未完成（busy={busy}），调用方需连带导出 -wal");
                false
            }
            Err(e) => {
                tracing::warn!("store：wal_checkpoint 失败（{e}），调用方需连带导出 -wal");
                false
            }
        }
    }

    /// 记录一条用量。每 PRUNE_EVERY 条触发一次 prune。
    pub fn usage_record(&self, ev: UsageEvent) {
        match &self.backend {
            Backend::Db(conn) => {
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    // ★ 写失败不能静默：旧实现 `let _ = c.execute(...)` 把 SQLite 错误
                    //   （库被锁满、磁盘满、schema 被旧版本改坏）整个吞掉 —— 调用方照常
                    //   收到成功的对话响应，面板上的调用次数却不再增长，用户永远查不到
                    //   原因。丢的是一条台账，但至少要留下日志说明丢了。
                    if let Err(e) = c.execute(
                        "INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ttfb,ok,upstream,account,cancelled,kind)
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                        rusqlite::params![
                            ev.ts,
                            ev.platform,
                            ev.model,
                            ev.tin,
                            ev.tout,
                            ev.ms,
                            ev.ttfb,
                            ev.ok as i64,
                            ev.upstream,
                            ev.account,
                            ev.cancelled as i64,
                            kind_of(&ev)
                        ],
                    ) {
                        tracing::warn!("store：写入用量事件失败（{e}），该条台账会缺失");
                    }
                }
                if self.since_prune.fetch_add(1, Ordering::Relaxed) + 1 >= PRUNE_EVERY {
                    self.since_prune.store(0, Ordering::Relaxed);
                    self.usage_prune();
                }
            }
            Backend::Json { mem, path } => {
                // 锁中毒时旧实现整段跳过：用量事件**静默丢失**（面板数字不再增长，
                // 而调用方仍收到成功响应）。中毒不是数据坏了，继续用里面的列表。
                {
                    let mut list = mem.lock().unwrap_or_else(|e| e.into_inner());
                    let mut ev = ev.clone();
                    // JSON 降级文件里空串与「没有这个字段」同义，落盘前统一成明文动作名，
                    // 否则下次读回来又得靠推断。
                    ev.kind = kind_of(&ev);
                    list.push(ev.clone());
                    let cutoff = ev.ts - CAP_DAYS * 86400.0;
                    list.retain(|x| x.ts >= cutoff);
                    if list.len() as i64 > CAP_EVENTS {
                        let over = list.len() as i64 - CAP_EVENTS;
                        list.drain(0..over as usize);
                    }
                    let payload = json!({ "events": list.iter().collect::<Vec<_>>() });
                    let text = serde_json::to_string_pretty(&payload).unwrap_or_default();
                    if let Err(e) = crate::atomic::write_atomic(path, text.as_bytes()) {
                        tracing::warn!("store：降级 JSON 写入失败（{}）：{e}", path.display());
                    }
                }
            }
        }
    }

    fn usage_prune(&self) {
        if let Backend::Db(conn) = &self.backend {
            {
                let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                let cutoff = now_unix() - CAP_DAYS * 86400.0;
                if let Err(e) = c.execute("DELETE FROM usage_events WHERE ts < ?1", [cutoff]) {
                    tracing::warn!("usage_prune 清理过期事件失败：{e}");
                }
                // ★ 用 rowid 而不是 ts 做「保留最近 N 条」的判据：ts 是 REAL 秒，
                //   同一秒内的多条事件（并发请求）ts 完全相同，NOT IN 会把它们
                //   一起删掉——超出上限时误删刚写入的新数据。
                if let Err(e) = c.execute(
                    "DELETE FROM usage_events WHERE rowid NOT IN (
                        SELECT rowid FROM usage_events ORDER BY ts DESC, rowid DESC LIMIT ?1)",
                    [CAP_EVENTS],
                ) {
                    tracing::warn!("usage_prune 清理超限事件失败：{e}");
                }
            }
        }
    }

    fn usage_max_ts(&self) -> f64 {
        match &self.backend {
            Backend::Db(conn) => {
                let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                c.query_row("SELECT MAX(ts) FROM usage_events", [], |r| {
                    r.get::<_, Option<f64>>(0)
                })
                .ok()
                .flatten()
                .unwrap_or(0.0)
            }
            Backend::Json { mem, .. } => {
                mem.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|e| e.ts)
                    .fold(0.0_f64, f64::max)
            }
        }
    }

    /// 全量读取（升序）。
    pub fn usage_all(&self) -> Vec<UsageEvent> {
        match &self.backend {
            Backend::Db(conn) => {
                let mut out = Vec::new();
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) =
                        c.prepare("SELECT ts,platform,model,tin,tout,ms,ttfb,ok,upstream,account,cancelled,kind FROM usage_events ORDER BY ts ASC")
                    {
                        let _ = stmt
                            .query_map([], map_usage_row)
                            .map(|rows| {
                                for row in rows.flatten() {
                                    out.push(row);
                                }
                            });
                    };
                }
                out
            }
            Backend::Json { mem, .. } => mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// 最近 N 条请求（ts 倒序，M6 请求历史）。
    ///
    /// ★ DB 后端走 `ORDER BY ts DESC LIMIT ?1`（走 idx_ue_ts），不再把整张台账
    ///   读进内存再排序截断：上限 CAP_EVENTS=5000 时，面板每 5 秒轮询一次
    ///   /api/requests 就要为「60 行」搬运 5000 行并分配 5000 个 String。
    pub fn usage_recent(&self, limit: usize) -> Vec<UsageEvent> {
        match &self.backend {
            Backend::Db(conn) => {
                let mut out = Vec::new();
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT ts,platform,model,tin,tout,ms,ttfb,ok,upstream,account,cancelled,kind FROM usage_events
                         ORDER BY ts DESC, rowid DESC LIMIT ?1",
                    ) {
                        if let Ok(rows) = stmt.query_map([limit as i64], map_usage_row) {
                            for row in rows.flatten() {
                                out.push(row);
                            }
                        }
                    };
                }
                out
            }
            Backend::Json { mem, .. } => {
                let mut all = mem
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                all.sort_by(|a, b| {
                    b.ts.partial_cmp(&a.ts)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                all.truncate(limit);
                all
            }
        }
    }

/// 窗口内的用量事件（ts 在 [from, to] 区间，ts 倒序；不做 limit，由调用方自行截断）。
    pub fn usage_window(&self, from: f64, to: f64) -> Vec<UsageEvent> {
        match &self.backend {
            Backend::Db(conn) => {
                let mut out = Vec::new();
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT ts,platform,model,tin,tout,ms,ttfb,ok,upstream,account,cancelled,kind FROM usage_events
                         WHERE ts >= ?1 AND ts <= ?2
                         ORDER BY ts DESC, rowid DESC",
                    ) {
                        if let Ok(rows) = stmt.query_map([from, to], map_usage_row) {
                            for row in rows.flatten() {
                                out.push(row);
                            }
                        }
                    };
                }
                out
            }
            Backend::Json { mem, .. } => {
                let all = mem
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let mut v: Vec<UsageEvent> = all
                    .into_iter()
                    .filter(|e| e.ts >= from && e.ts <= to)
                    .collect();
                v.sort_by(|a, b| {
                    b.ts.partial_cmp(&a.ts)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                v
            }
        }
    }

    /// 窗口内的一页事件（ts 倒序，`limit` 条、跳过 `offset` 条）。
    ///
    /// ★ 与 `usage_window` 的区别：分页下推到 SQL（`LIMIT ?3 OFFSET ?4`）。
    ///   面板要的是「这一页」，不是「窗口里的全部」——长窗口下把整窗搬进内存
    ///   再截断，既慢又和上面的统计数字对不上（统计是全窗 COUNT/SUM）。
    pub fn usage_window_page(&self, from: f64, to: f64, limit: usize, offset: usize) -> Vec<UsageEvent> {
        let limit = limit.max(1) as i64;
        // ★ `offset as i64` 对超界值（如 u64::MAX）是**有符号饱和**：得到 i64::MAX。
        //   把 i64::MAX 直接绑进 `LIMIT ?3 OFFSET ?4` 会让 SQLite 返回 SQLITE_TOOBIG，
        //   而调用方（/api/requests）只 `if let Ok(rows)` 丢掉了错误 → 连接上什么都没写，
        //   客户端看到的是「Empty reply from server」(curl 52)。夹一个安全上界即可：
        //   任何超过 CAP_EVENTS 的 offset 都只会得到空页，行为不变但没有异常路径。
        // 用 u64 比较再转 i64：直接 `offset as i64` 只对「大到饱和」的值安全，
        // 精确等于 i64::MAX 时 min 不会生效（那正是 SQLITE_TOOBIG 的触发值）。
        let offset = offset.min(CAP_EVENTS as usize) as i64;
        match &self.backend {
            Backend::Db(conn) => {
                let mut out = Vec::new();
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT ts,platform,model,tin,tout,ms,ttfb,ok,upstream,account,cancelled,kind FROM usage_events
                         WHERE ts >= ?1 AND ts <= ?2
                         ORDER BY ts DESC, rowid DESC LIMIT ?3 OFFSET ?4",
                    ) {
                        if let Ok(rows) = stmt.query_map((from, to, limit, offset), map_usage_row) {
                            for row in rows.flatten() {
                                out.push(row);
                            }
                        }
                    };
                }
                out
            }
            Backend::Json { mem, .. } => {
                let all = mem.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let mut v: Vec<UsageEvent> = all
                    .into_iter()
                    .filter(|e| e.ts >= from && e.ts <= to)
                    .collect();
                v.sort_by(|a, b| {
                    b.ts.partial_cmp(&a.ts)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                v.into_iter().skip(offset.max(0) as usize).take(limit as usize).collect()
            }
        }
    }

    /// 窗口内的事件总数：就是统计里 `requests` 的来源。
    ///
    /// 列表只显示一页时，必须同时给出总数，否则「列表 60 条 / 统计 500 条」
    /// 会让人以为台账丢了数据。
    pub fn usage_window_count(&self, from: f64, to: f64) -> u64 {
        match &self.backend {
            Backend::Db(conn) => {
                let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                if let Ok(mut stmt) = c.prepare(
                    "SELECT COUNT(*) FROM usage_events WHERE ts >= ?1 AND ts <= ?2",
                ) {
                    if let Ok(n) = stmt.query_row((from, to), |r| r.get::<_, i64>(0)) {
                        return n.max(0) as u64;
                    }
                }
                0
            }
            Backend::Json { mem, .. } => mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|e| e.ts >= from && e.ts <= to)
                .count() as u64,
        }
    }

    /// 窗口内按某个维度聚合（platform / model / account 共用一套实现，避免三份
    /// 手写 SQL 慢慢漂移——列名拼错在 SQL 里是报错，但 `SUM(tin)` 换成 `SUM(tout)`
    /// 之类只会静默算错）。
    fn usage_window_group(&self, from: f64, to: f64, by: GroupBy) -> serde_json::Value {
        let col = by.col();
        // 模型维度只看对话记录：后台任务（checkin / growth）也往同一张表里记账，
        // 它们的 model 字段是动作名而不是模型名。若不排除，模型榜上会出现一行
        // 「checkin 17 次」并挤掉真实模型的前 N 名位置。
        let chat_only = matches!(by, GroupBy::Model);
        let mut agg: std::collections::BTreeMap<String, serde_json::Value> = Default::default();
        match &self.backend {
            Backend::Db(conn) => {
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(&format!(
                        "SELECT {col},
                                 COUNT(*),
                                 SUM(CASE WHEN ok<>0 THEN 1 ELSE 0 END),
                                 SUM(CASE WHEN ok=0 AND cancelled=0 THEN 1 ELSE 0 END),
                                 SUM(CASE WHEN cancelled<>0 THEN 1 ELSE 0 END),
                                 SUM(MAX(tin,0)),
                                 SUM(MAX(tout,0))
                          FROM usage_events
                          WHERE ts >= ?1 AND ts <= ?2{}
                          GROUP BY {col}",
                        if chat_only { " AND kind='chat'" } else { "" }
                    )) {
                        if let Ok(rows) = stmt.query_map([from, to], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, i64>(1)?,
                                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                            ))
                        }) {
                            for (key, req, ok_n, fail_n, cancelled, tin, tout) in rows.flatten() {
                                agg.insert(
                                    key,
                                    serde_json::json!({
                                        "requests": req, "success": ok_n, "failed": fail_n,
                                        "cancelled": cancelled,
                                        "tokensIn": tin, "tokensOut": tout,
                                    }),
                                );
                            }
                        }
                    };
                }
            }
            Backend::Json { mem, .. } => {
                let evs = mem
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                for e in &evs {
                    if e.ts < from || e.ts > to {
                        continue;
                    }
                    if chat_only && kind_of(e) != "chat" {
                        continue;
                    }
                    let key = match by {
                        GroupBy::Platform => e.platform.clone(),
                        GroupBy::Model => e.model.clone(),
                        GroupBy::Account => e.account.clone(),
                    };
                    let d = agg.entry(key).or_insert_with(|| {
                        serde_json::json!({"requests":0,"success":0,"failed":0,"cancelled":0,"tokensIn":0,"tokensOut":0})
                    });
                    d["requests"] = serde_json::json!(d["requests"].as_i64().unwrap_or(0) + 1);
                    if e.ok {
                        d["success"] = serde_json::json!(d["success"].as_i64().unwrap_or(0) + 1);
                    } else if e.cancelled {
                        d["cancelled"] = serde_json::json!(d["cancelled"].as_i64().unwrap_or(0) + 1);
                    } else {
                        d["failed"] = serde_json::json!(d["failed"].as_i64().unwrap_or(0) + 1);
                    }
                    d["tokensIn"] = serde_json::json!(d["tokensIn"].as_i64().unwrap_or(0) + e.tin.max(0));
                    d["tokensOut"] = serde_json::json!(d["tokensOut"].as_i64().unwrap_or(0) + e.tout.max(0));
                }
            }
        }
        agg.into_iter().collect()
    }

    /// 窗口内按平台聚合。
    pub fn usage_window_by_platform(&self, from: f64, to: f64) -> serde_json::Value {
        self.usage_window_group(from, to, GroupBy::Platform)
    }

    /// 窗口内按模型聚合：定位「哪个模型在烧额度」。
    pub fn usage_window_by_model(&self, from: f64, to: f64) -> serde_json::Value {
        self.usage_window_group(from, to, GroupBy::Model)
    }

    /// 窗口内按账号聚合：定位「哪个账号失败率高 / 已经被上游拉黑」。
    pub fn usage_window_by_account(&self, from: f64, to: f64) -> serde_json::Value {
        self.usage_window_group(from, to, GroupBy::Account)
    }

    /// 窗口内用量汇总。
    pub fn usage_window_totals(&self, from: f64, to: f64) -> serde_json::Value {
        let (req, ok_n, fail_n, cancelled_n, ti, to_) = match &self.backend {
            Backend::Db(conn) => {
                let mut acc = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT COUNT(*),
                                 SUM(CASE WHEN ok<>0 THEN 1 ELSE 0 END),
                                 SUM(CASE WHEN ok=0 AND cancelled=0 THEN 1 ELSE 0 END),
                                 SUM(MAX(tin,0)),
                                 SUM(MAX(tout,0)),
                                 SUM(CASE WHEN cancelled<>0 THEN 1 ELSE 0 END)
                          FROM usage_events
                          WHERE ts >= ?1 AND ts <= ?2",
                    ) {
                        if let Ok(row) = stmt.query_row([from, to], |r| {
                            Ok((
                                r.get::<_, i64>(0)?.max(0) as u64,
                                r.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as u64,
                                // 列序：COUNT, ok, failed, tin, tout, cancelled ——
                                // 而 acc 的元组顺序是 (req, ok, fail, cancelled, ti, tout)，
                                // 这里必须按名字对回位置，不能顺着抄（抄错只会静默算错数）。
                                r.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u64,
                            ))
                        }) {
                            acc = row;
                        }
                    };
                }
                acc
            }
            Backend::Json { mem, .. } => {
                let evs = mem
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let (mut req, mut ok_n, mut fail_n, mut cancelled_n, mut ti, mut to_) =
                    (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
                for e in &evs {
                    if e.ts < from || e.ts > to {
                        continue;
                    }
                    req += 1;
                    if e.ok {
                        ok_n += 1;
                    } else if e.cancelled {
                        cancelled_n += 1;
                    } else {
                        fail_n += 1;
                    }
                    ti += e.tin.max(0) as u64;
                    to_ += e.tout.max(0) as u64;
                }
                (req, ok_n, fail_n, cancelled_n, ti, to_)
            }
        };
        // ★ 2026-10-26（P3）：TTFB 与总耗时延迟分位（P50/P95/平均）。
        // 单独一趟查询取数再排序算分位——SQLite 没有 PERCENTILE 函数，
        // 而在 Rust 侧排序几百条延迟样本的代价可以忽略。
        let (mut tt, mut total): (Vec<i64>, Vec<i64>) = match &self.backend {
            Backend::Db(conn) => {
                let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                let mut tt = Vec::new();
                let mut total = Vec::new();
                if let Ok(mut stmt) =
                    c.prepare("SELECT ms, ttfb FROM usage_events WHERE ts >= ?1 AND ts <= ?2")
                {
                    if let Ok(rows) = stmt.query_map([from, to], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                    }) {
                        for (ms, ttfb) in rows.flatten() {
                            if ttfb > 0 {
                                tt.push(ttfb);
                            }
                            if ms > 0 {
                                total.push(ms);
                            }
                        }
                    }
                }
                (tt, total)
            }
            Backend::Json { mem, .. } => {
                let mut tt = Vec::new();
                let mut total = Vec::new();
                for e in mem.lock().unwrap_or_else(|x| x.into_inner()).iter() {
                    if e.ts < from || e.ts > to {
                        continue;
                    }
                    if e.ttfb > 0 {
                        tt.push(e.ttfb);
                    }
                    if e.ms > 0 {
                        total.push(e.ms);
                    }
                }
                (tt, total)
            }
        };
        let ttfb_stats = percentiles(&mut tt);
        let total_stats = percentiles(&mut total);
        json!({
            "requests": req, "success": ok_n, "failed": fail_n,
            "cancelled": cancelled_n,
            "tokensIn": ti, "tokensOut": to_,
            "db": self.is_db(),
            // 延迟分位：ttfb = 首字节（流式体验），latency = 整轮耗时
            "ttfb": ttfb_stats,
            "latency": total_stats,
        })
    }

    pub fn usage_by_platform(&self) -> serde_json::Value {        let mut agg: std::collections::BTreeMap<String, serde_json::Value> = Default::default();
        match &self.backend {
            Backend::Db(conn) => {
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT platform,
                                COUNT(*),
                                SUM(CASE WHEN ok<>0 THEN 1 ELSE 0 END),
                                SUM(CASE WHEN ok=0 AND cancelled=0 THEN 1 ELSE 0 END),
                                SUM(tin),
                                SUM(tout)
                         FROM usage_events GROUP BY platform",
                    ) {
                        if let Ok(rows) = stmt.query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, i64>(1)?,
                                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                                r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                            ))
                        }) {
                            for (platform, req, ok_n, fail_n, tin, tout) in rows.flatten() {
                                agg.insert(
                                    platform,
                                    json!({
                                        "requests": req, "success": ok_n, "failed": fail_n,
                                        "tokensIn": tin, "tokensOut": tout,
                                    }),
                                );
                            }
                        }
                    };
                }
            }
            Backend::Json { mem, .. } => {
                let evs = mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
                for e in &evs {
                    let d = agg.entry(e.platform.clone()).or_insert_with(
                        || json!({"requests":0,"success":0,"failed":0,"cancelled":0,"tokensIn":0,"tokensOut":0}),
                    );
                    d["requests"] = json!(d["requests"].as_i64().unwrap_or(0) + 1);
                    if e.ok {
                        d["success"] = json!(d["success"].as_i64().unwrap_or(0) + 1);
                    } else if e.cancelled {
                        d["cancelled"] = json!(d["cancelled"].as_i64().unwrap_or(0) + 1);
                    } else {
                        d["failed"] = json!(d["failed"].as_i64().unwrap_or(0) + 1);
                    }
                    d["tokensIn"] = json!(d["tokensIn"].as_i64().unwrap_or(0) + e.tin);
                    d["tokensOut"] = json!(d["tokensOut"].as_i64().unwrap_or(0) + e.tout);
                }
            }
        }
        json!(agg)
    }

    /// 汇总（前端用量页 M1 简版）。
    ///
    /// ★ DB 后端用一条聚合 SELECT（COUNT + SUM），不再全表读。
    pub fn usage_totals(&self) -> serde_json::Value {
        let (req, ok_n, fail_n, cancelled_n, ti, to) = match &self.backend {
            Backend::Db(conn) => {
                let mut acc = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
                {
                    let c = conn.lock().unwrap_or_else(|e| e.into_inner());
                    if let Ok(mut stmt) = c.prepare(
                        "SELECT COUNT(*),
                                SUM(CASE WHEN ok<>0 THEN 1 ELSE 0 END),
                                SUM(CASE WHEN ok=0 AND cancelled=0 THEN 1 ELSE 0 END),
                                SUM(MAX(tin,0)),
                                SUM(MAX(tout,0)),
                                SUM(CASE WHEN cancelled<>0 THEN 1 ELSE 0 END)
                         FROM usage_events",
                    ) {
                        if let Ok(row) = stmt.query_row([], |r| {
                            Ok((
                                r.get::<_, i64>(0)?.max(0) as u64,
                                r.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(2)?.unwrap_or(0).max(0) as u64,
                                // 列序是 COUNT, ok, failed, tin, tout, cancelled，
                                // 而元组是 (req, ok, fail, cancelled, ti, tout)。
                                r.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(3)?.unwrap_or(0).max(0) as u64,
                                r.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u64,
                            ))
                        }) {
                            acc = row;
                        }
                    };
                }
                acc
            }
            Backend::Json { mem, .. } => {
                let evs = mem
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
                let (mut req, mut ok_n, mut fail_n, mut cancelled_n, mut ti, mut to) =
                    (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
                for e in &evs {
                    req += 1;
                    if e.ok {
                        ok_n += 1;
                    } else if e.cancelled {
                        cancelled_n += 1;
                    } else {
                        fail_n += 1;
                    }
                    ti += e.tin.max(0) as u64;
                    to += e.tout.max(0) as u64;
                }
                (req, ok_n, fail_n, cancelled_n, ti, to)
            }
        };
        json!({
            "requests": req, "success": ok_n, "failed": fail_n,
            "cancelled": cancelled_n,
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
                        "SELECT ts,platform,model,tin,tout,ms,ttfb,ok,upstream FROM usage_events WHERE ts > ?1 ORDER BY ts ASC",
                    );
                    if let Ok(mut stmt) = stmt {
                        let rows = stmt.query_map([max_ts], |r| {
                            Ok(UsageEvent {
                                ts: r.get(0)?,
                                platform: r.get(1)?,
                                model: r.get(2)?,
                                account: String::new(),
                                tin: r.get(3)?,
                                tout: r.get(4)?,
                                ms: r.get(5)?,
                                // ★ P3：SELECT 现为 ms,ttfb,ok,upstream
                                ttfb: r.get(6)?,
                                ok: r.get::<_, i64>(7)? != 0,
                                cancelled: false,
                                upstream: r.get(8)?,
                                kind: String::new(),
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
            account: "acc-1".into(),
            tin: 11,
            tout: 22,
            ms: 123,
            ttfb: 0,
            ok,
            cancelled: false,
            upstream: "opencode.ai".into(),
            kind: String::new(),
        }
    }

    /// 客户端取消的那一条：`ok = false` 且 `cancelled = true`。
    fn evc(ts: f64, model: &str) -> UsageEvent {
        UsageEvent {
            cancelled: true,
            ..ev(ts, model, false)
        }
    }

    /// 后台任务（model 写成动作名）绝不能出现在模型维度里，但平台/账号维度要保留
    /// —— 签到失败是「这个平台有问题」的信号，不该被一起抹掉。
    #[test]
    fn background_task_rows_stay_out_of_the_model_breakdown() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        st.usage_record(ev(100.0, "real-model", true));
        st.usage_record(ev(101.0, "real-model", true));
        st.usage_record(UsageEvent {
            kind: "checkin".into(),
            model: "checkin".into(),
            ..ev(102.0, "checkin", true)
        });
        st.usage_record(UsageEvent {
            kind: "growth".into(),
            model: "growth".into(),
            ..ev(103.0, "growth", false)
        });
        let by_model = st.usage_window_by_model(0.0, 1e18);
        let o = by_model.as_object().unwrap();
        assert_eq!(o.len(), 1, "模型榜只该有一个真模型：{by_model}");
        assert_eq!(o["real-model"]["requests"], 2);
        assert!(o.get("checkin").is_none(), "签到不该出现在模型榜：{by_model}");
        assert!(o.get("growth").is_none(), "成长任务不该出现在模型榜：{by_model}");
        // 平台与账号维度照旧把它们算进去
        let by_plat = st.usage_window_by_platform(0.0, 1e18);
        assert_eq!(by_plat["anon-zen"]["requests"], 4);
        let totals = st.usage_window_totals(0.0, 1e18);
        assert_eq!(totals["requests"], 4);
    }

    /// JSON 降级后端必须与 SQLite 后端同口径：模型维度也要剔掉后台任务行。
    /// （只在 DB 后端跑测试的话，下面这个分支永远不会被覆盖到。）
    #[test]
    fn json_backend_also_keeps_background_rows_out_of_the_model_breakdown() {
        let tmp = tempfile::tempdir().unwrap();
        // 把 panel.db 造成目录：Connection::open 必然失败 → 降级 JSON 后端
        std::fs::create_dir_all(tmp.path().join("panel.db")).unwrap();
        let st = Store::open(tmp.path());
        assert!(!st.is_db(), "这一条必须跑在 JSON 降级后端上");
        st.usage_record(ev(100.0, "real-model", true));
        st.usage_record(UsageEvent {
            kind: "checkin".into(),
            model: "checkin".into(),
            ..ev(101.0, "checkin", true)
        });
        let by_model = st.usage_window_by_model(0.0, 1e18);
        let o = by_model.as_object().unwrap();
        assert_eq!(o.len(), 1, "JSON 后端也要排除后台任务：{by_model}");
        assert_eq!(o["real-model"]["requests"], 1);
        let by_plat = st.usage_window_by_platform(0.0, 1e18);
        assert_eq!(by_plat["anon-zen"]["requests"], 2, "平台维度照旧全算");
        // 落盘时也必须把空串统一成明文动作名：磁盘上留空，下次读回来就得靠推断
        let raw = std::fs::read_to_string(tmp.path().join("usage_events.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let kinds: Vec<String> = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap_or("<缺字段>").to_string())
            .collect();
        assert_eq!(kinds, vec!["chat".to_string(), "checkin".to_string()], "{raw}");
    }

    /// 老库迁移：补列时默认回填 chat，并把「model 就是动作名」的历史行改标成 checkin。
    #[test]
    fn legacy_rows_with_action_names_are_relabelled_on_migration() {
        let tmp = tempfile::tempdir().unwrap();
        {
            // 手工造一张没有 kind 列的老表，并塞一条 checkin 行
            let c = Connection::open(tmp.path().join("panel.db")).unwrap();
            c.execute_batch(
                "CREATE TABLE usage_events(
                     ts REAL NOT NULL, platform TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
                     account TEXT NOT NULL DEFAULT '', tin INTEGER NOT NULL DEFAULT 0,
                     tout INTEGER NOT NULL DEFAULT 0, ms INTEGER NOT NULL DEFAULT 0,
                     ok INTEGER NOT NULL DEFAULT 0, cancelled INTEGER NOT NULL DEFAULT 0,
                     upstream TEXT NOT NULL DEFAULT '');
                 INSERT INTO usage_events(ts,platform,model,ok,upstream)
                   VALUES(1.0,'p','checkin',1,'auto:p');
                 INSERT INTO usage_events(ts,platform,model,ok,upstream)
                   VALUES(2.0,'p','real-model',1,'p');",
            )
            .unwrap();
        }
        let st = Store::open(tmp.path());
        let by_model = st.usage_window_by_model(0.0, 1e18);
        let o = by_model.as_object().unwrap();
        assert!(o.get("checkin").is_none(), "历史 checkin 行要被打标排除：{by_model}");
        assert_eq!(o["real-model"]["requests"], 1, "{by_model}");
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
    fn old_db_without_account_column_is_migrated_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("panel.db");
        // 造一个「老版本」库：usage_events 没有 account 列。CREATE TABLE IF NOT EXISTS
        // 对已存在的表什么都不做，所以升级时必须真的 ALTER 补列，否则每一条台账
        // 都会在 INSERT 时报错（旧实现会把这个错误写进日志然后静默丢数据）。
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE usage_events(
                     ts REAL NOT NULL, platform TEXT NOT NULL DEFAULT '',
                     model TEXT NOT NULL DEFAULT '', tin INTEGER NOT NULL DEFAULT 0,
                     tout INTEGER NOT NULL DEFAULT 0, ms INTEGER NOT NULL DEFAULT 0,
                     ok INTEGER NOT NULL DEFAULT 0, upstream TEXT NOT NULL DEFAULT '');
                 INSERT INTO usage_events(ts,platform,model,tin,tout,ms,ok,upstream)
                 VALUES(500.0,'old','m-old',3,4,5,1,'u-old');",
            )
            .unwrap();
        }
        let st = Store::open(tmp.path());
        assert!(st.is_db(), "补列后应仍是 SQLite 后端");
        st.usage_record(ev(1000.0, "m-new", true));
        let all = st.usage_all();
        assert_eq!(all.len(), 2, "老行 + 新行都在：{all:?}");
        let newbie = all.iter().find(|e| e.model == "m-new").expect("新行要写进去");
        assert_eq!(newbie.account, "acc-1", "新行的账号要落库");
        let old = all.iter().find(|e| e.model == "m-old").expect("老行要读出来");
        assert_eq!(old.account, "", "老行没有账号信息，降级为空串而不是报错");
        assert_eq!(old.tin, 3);
    }

    #[test]
    fn window_group_by_model_and_account_aggregates() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        let mut a = ev(1000.0, "m-a", true);
        a.tin = 10;
        a.tout = 1;
        let mut b = ev(1001.0, "m-a", false);
        b.tin = 0;
        b.tout = 0;
        b.account = "acc-2".into();
        let c = ev(1002.0, "m-b", true);
        st.usage_record(a);
        st.usage_record(b);
        st.usage_record(c);
        let by_model = st.usage_window_by_model(0.0, 2000.0);
        assert_eq!(by_model["m-a"]["requests"], 2, "{by_model}");
        assert_eq!(by_model["m-a"]["failed"], 1);
        assert_eq!(by_model["m-a"]["tokensIn"], 10);
        assert_eq!(by_model["m-b"]["requests"], 1);
        // 窗口外不应计入
        let empty = st.usage_window_by_model(3000.0, 4000.0);
        assert!(empty.as_object().unwrap().is_empty(), "{empty}");
        let by_acc = st.usage_window_by_account(0.0, 2000.0);
        assert_eq!(by_acc["acc-1"]["requests"], 2, "{by_acc}");
        assert_eq!(by_acc["acc-2"]["failed"], 1);
    }

    /// 客户端取消不算「平台失败」，但要能数出来：汇总与分组都把 cancelled 单列。
    #[test]
    fn cancelled_events_are_counted_apart_from_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        assert!(st.is_db());
        st.usage_record(ev(1000.0, "m-1", true));
        st.usage_record(ev(1001.0, "m-1", false));
        st.usage_record(evc(1002.0, "m-1"));
        let t = st.usage_window_totals(0.0, 2000.0);
        assert_eq!(t["requests"], 3, "{t}");
        assert_eq!(t["success"], 1);
        assert_eq!(t["failed"], 1, "取消不能算进 failed：{t}");
        assert_eq!(t["cancelled"], 1, "取消要单独数出来：{t}");
        let g = st.usage_window_by_platform(0.0, 2000.0);
        assert_eq!(g["anon-zen"]["requests"], 3, "{g}");
        assert_eq!(g["anon-zen"]["failed"], 1, "{g}");
        assert_eq!(g["anon-zen"]["cancelled"], 1, "{g}");
    }

    /// 老库没有 cancelled 列：补列后老行 OLD 一律算「未取消」，新写入的取消行照样数得出来。
    #[test]
    fn old_db_without_cancelled_column_counts_old_rows_as_not_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("panel.db");
        {
            let c = Connection::open(&db).unwrap();
            c.execute_batch(
                "CREATE TABLE usage_events(
                     ts REAL NOT NULL, platform TEXT NOT NULL DEFAULT '',
                     model TEXT NOT NULL DEFAULT '', account TEXT NOT NULL DEFAULT '',
                     tin INTEGER NOT NULL DEFAULT 0, tout INTEGER NOT NULL DEFAULT 0,
                     ms INTEGER NOT NULL DEFAULT 0, ok INTEGER NOT NULL DEFAULT 0,
                     upstream TEXT NOT NULL DEFAULT '');
                 INSERT INTO usage_events(ts,platform,model,account,tin,tout,ms,ok,upstream)
                 VALUES(500.0,'anon-zen','m-old','acc-1',1,2,3,0,'u-old');",
            )
            .unwrap();
        }
        let st = Store::open(tmp.path());
        assert!(st.is_db(), "补列后应仍是 SQLite 后端");
        st.usage_record(evc(1000.0, "m-new"));
        let t = st.usage_window_totals(0.0, 2000.0);
        assert_eq!(t["requests"], 2, "{t}");
        assert_eq!(t["failed"], 1, "老行是普通失败（列不存在 → 0）：{t}");
        assert_eq!(t["cancelled"], 1, "新行是取消：{t}");
    }

    /// JSON 降级后端同样区分：字段靠 serde(default) 兼容旧文件。
    #[test]
    fn json_fallback_counts_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = tmp.path().to_path_buf();
        std::fs::create_dir_all(dd.join("panel.db")).unwrap();
        let st = Store::open(&dd);
        assert!(!st.is_db());
        st.usage_record(ev(1000.0, "m-1", false));
        st.usage_record(evc(1001.0, "m-1"));
        let t = st.usage_totals();
        assert_eq!(t["failed"], 1, "{t}");
        assert_eq!(t["cancelled"], 1, "{t}");
        // 重新 open 读回 JSON 之后仍在（字段要落盘）
        let st2 = Store::open(&dd);
        let g = st2.usage_window_by_platform(0.0, 2000.0);
        assert_eq!(g["anon-zen"]["cancelled"], 1, "{g}");
    }
    #[test]
    fn checkpoint_folds_wal_into_main_db() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        assert!(st.is_db());
        // 灌够数据，让 WAL 明显大于空库（4KB）
        for i in 0..200 {
            st.usage_record(ev(1000.0 + i as f64, "m-1", true));
        }
        let wal = tmp.path().join("panel.db-wal");
        let wal_before = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert!(wal_before > 0, "写入后应有 -wal 文件");
        assert!(st.checkpoint(), "checkpoint 应成功");
        let wal_after = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
        assert_eq!(wal_after, 0, "TRUNCATE 档位应把 -wal 截断为 0，实际 {wal_after}");
        // 数据仍在（并已并回主库）
        assert_eq!(st.usage_totals()["requests"], 200);
        // JSON 降级后端没有 WAL，checkpoint 返回 false 而不是谎报成功
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp2.path().join("panel.db")).unwrap();
        assert!(!Store::open(tmp2.path()).checkpoint());
    }
    /// ★ 回归：恢复备份时暂存的 panel.db*.restore-pending 必须在下次打开时落地，
    /// 并且替换 panel.db 时要连带清掉旧的 -wal / -shm（新主库配旧 WAL 会读到不相干的页）。
    #[test]
    fn pending_restore_is_applied_on_open_and_clears_stale_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = tmp.path();
        // 造一个「旧库」并写入数据，让它产生 -wal/-shm
        {
            let st = Store::open(dd);
            assert!(st.is_db());
            st.usage_record(ev(1000.0, "old", true));
            assert_eq!(st.usage_totals()["requests"], 1);
        }
        // 造一个「新库」快照（只有 3 条记录），放到 .restore-pending
        let other = tempfile::tempdir().unwrap();
        {
            let st2 = Store::open(other.path());
            for i in 0..3 {
                st2.usage_record(ev(2000.0 + i as f64, "new", true));
            }
            assert!(st2.checkpoint(), "快照前先 checkpoint 才能单拷主库");
        }
        std::fs::copy(other.path().join("panel.db"), dd.join("panel.db.restore-pending")).unwrap();
        // 旧的 -wal / -shm 还在（模拟服务运行中遗留）
        std::fs::write(dd.join("panel.db-wal"), b"stale-wal-bytes").unwrap();
        assert_eq!(Store::apply_pending_restore(dd), 1);
        assert!(!dd.join("panel.db.restore-pending").exists(), "暂存文件应被消费掉");
        assert!(!dd.join("panel.db-wal").exists(), "换主库时必须清掉旧 -wal");
        let st = Store::open(dd);
        assert_eq!(st.usage_totals()["requests"], 3, "应读到恢复进来的新库");
        // 没有暂存文件时是空操作
        assert_eq!(Store::apply_pending_restore(dd), 0);
    }

    /// ★ 回归：换主库时若备份包里同时带了 panel.db-wal，必须**替换**而不是删掉 ——
    /// 先删后换的话，rename 失败就会变成「新主库 + 没有日志」或「旧主库 + 删掉的日志」。
    #[test]
    fn pending_restore_replaces_wal_when_backup_has_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dd = tmp.path();
        {
            let st = Store::open(dd);
            st.usage_record(ev(1000.0, "old", true));
            assert!(st.checkpoint());
        }
        // 备份包里的主库（空库）+ 一个空的 -wal
        let other = tempfile::tempdir().unwrap();
        {
            let st2 = Store::open(other.path());
            assert!(st2.checkpoint());
        }
        std::fs::copy(other.path().join("panel.db"), dd.join("panel.db.restore-pending")).unwrap();
        std::fs::write(dd.join("panel.db-wal.restore-pending"), b"").unwrap();
        std::fs::write(dd.join("panel.db-wal"), b"stale").unwrap();
        assert_eq!(Store::apply_pending_restore(dd), 2);
        assert!(!dd.join("panel.db-wal.restore-pending").exists());
        assert_eq!(std::fs::read(dd.join("panel.db-wal")).unwrap_or_default(), b"");
        let st = Store::open(dd);
        assert_eq!(st.usage_totals()["requests"], 0, "应读到备份里的空库");
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

    /// ★ 回归：usage_recent / usage_by_platform / usage_totals 走 SQL 之后
    /// 结果必须与「全表读 + 内存聚合」完全一致（含同秒并列的排序与限条）。
    #[test]
    fn sql_aggregates_match_memory_aggregation() {
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        assert!(st.is_db());
        // 三个平台、成对成功/失败、同秒并列，覆盖 COUNT/SUM/ORDER BY 的边界
        for i in 0..300 {
            let mut e = ev(1000.0 + (i / 3) as f64, &format!("m-{}", i % 3), i % 2 == 0);
            e.platform = match i % 3 {
                0 => "p-a".to_string(),
                1 => "p-b".to_string(),
                _ => "p-c".to_string(),
            };
            e.tin = i as i64;
            e.tout = (i * 2) as i64;
            st.usage_record(e);
        }
        // 最近 7 条：必须是 ts 最大的那批（ts 并列时按 rowid 倒序，即后写入的在前）
        let recent = st.usage_recent(7);
        assert_eq!(recent.len(), 7);
        let all = st.usage_all();
        let mut expect = all.clone();
        expect.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
        let max_ts = expect[0].ts;
        let tail_ts: Vec<f64> = recent.iter().map(|e| e.ts).collect();
        let mut want_ts: Vec<f64> = expect.iter().take(7).map(|e| e.ts).collect();
        assert_eq!(tail_ts, want_ts, "usage_recent 必须返回最新 7 条");
        assert!(recent.iter().all(|e| e.ts <= max_ts));
        want_ts.clear();

        // 平台聚合
        let agg = st.usage_by_platform();
        let mut mem: std::collections::BTreeMap<String, (i64, i64, i64, i64, i64)> = Default::default();
        for e in &all {
            let d = mem.entry(e.platform.clone()).or_insert((0, 0, 0, 0, 0));
            d.0 += 1;
            if e.ok { d.1 += 1 } else { d.2 += 1 }
            d.3 += e.tin;
            d.4 += e.tout;
        }
        for (p, (req, okn, failn, ti, to)) in mem {
            let row = &agg[&p];
            assert_eq!(row["requests"], json!(req), "{p} requests");
            assert_eq!(row["success"], json!(okn), "{p} success");
            assert_eq!(row["failed"], json!(failn), "{p} failed");
            assert_eq!(row["tokensIn"], json!(ti), "{p} tokensIn");
            assert_eq!(row["tokensOut"], json!(to), "{p} tokensOut");
        }

        // 总量
        let t = st.usage_totals();
        assert_eq!(t["requests"], json!(300));
        assert_eq!(t["success"], json!(150));
        assert_eq!(t["failed"], json!(150));
        let want_in: i64 = (0..300).sum();
        let want_out: i64 = (0..300).map(|i| i * 2).sum();
        assert_eq!(t["tokensIn"], json!(want_in));
        assert_eq!(t["tokensOut"], json!(want_out));
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

    #[test]
    fn usage_count_matches_recorded_events() {
        // health/「关于」页数据占用卡片依赖这个计数：必须与真实落盘一致，
        // 重开目录后还数得出来（不是只活在内存里）。
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        assert_eq!(st.usage_count(), 0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        st.usage_record(ev(now, "m1", true));
        st.usage_record(ev(now + 1.0, "m2", false));
        assert_eq!(st.usage_count(), 2);
        let st2 = Store::open(tmp.path());
        assert_eq!(st2.usage_count(), 2, "重开后必须还数得出来");
    }

    #[test]
    fn prune_keeps_exactly_cap_even_with_duplicate_timestamps() {
        // ts 是 REAL 秒：并发请求常常落在同一秒。旧实现用 `ts NOT IN (SELECT ts ... LIMIT N)`
        // 去重，边界秒上的整组事件会被「整批保留或整批删除」，条数不可控。
        // 改成按 rowid 取最近 N 条后，删除必须精确到 CAP_EVENTS。
        let tmp = tempfile::tempdir().unwrap();
        let st = Store::open(tmp.path());
        // 时间戳必须贴近「现在」：usage_prune 还会按 CAP_DAYS(90 天) 删老数据，
        // 用 1970 的 ts 写入会被直接当过期记录清掉。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        // 每 10 条共用一个 ts：5100 条 → 510 个不同秒值。
        for i in 0..5100 {
            st.usage_record(ev(now + (i / 10) as f64, "m", true));
        }
        // 自动 prune 是「每 500 条一次」，所以循环结束时最多超出上限 499 条；
        // 这里显式触发一次，验证压到**正好等于**上限（旧 ts 去重会多留/多删）。
        st.usage_prune();
        let n = st.usage_all().len() as i64;
        assert_eq!(n, CAP_EVENTS, "应精确保留 {CAP_EVENTS} 条，实际 {n}");
        // 保留的必须是**最新**的那批：最后写入的那条（ts 最大）一定还在。
        let last_ts = now + (5099 / 10) as f64;
        let all = st.usage_all();
        let max_ts = all.iter().map(|e| e.ts).fold(0.0_f64, f64::max);
        assert_eq!(max_ts, last_ts, "最新事件被误删");
    }
}
