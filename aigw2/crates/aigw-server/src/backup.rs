//! 数据备份与恢复（`GET /api/backup`、`POST /api/backup`、`POST /api/backup/export`）。
//!
//! **为什么独立成模块**：这一族有一半是**数据安全边界**（哪些文件可以明文导出、哪些
//! 必须脱敏、恢复时的文件名穿越校验、数据库文件必须走暂存通道否则覆盖运行中的库），
//! 另一半是**阻塞 IO**（扫目录、逐个读写、WAL checkpoint）。它们原本混在 `lib.rs`
//! 的 handler 堆里，既难单独审，也难单独测。
//!
//! 两条纪律写在最前面：
//! 1. **导出默认脱敏**：只有白名单里的台账文件明文化，其余一律掩码 —— 黑名单每新增
//!    一个凭据文件都会默认落进「可明文导出」集合，典型的失败开放。
//! 2. **恢复必须留退路**：目标文件已存在就先复制 `.bak-<ts>`，写失败不静默；
//!    数据库文件不直接覆盖（服务正持着句柄），而是写成 `.restore-pending` 由下次
//!    启动原子替换，这样响应里的 `restart_required: true` 才真的生效。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::{now_ms, AppState};

pub(crate) fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}


/// 备份导出（M5 交付链，对齐 cline2api tmp+rename 数据纪律）：GET /api/backup
/// 扫描 data 目录全部运行态文件（*.json + panel.db），打包为 JSON 数组供前端下载/保存。
/// 敏感文件识别：备份内容掩码用（凭据不进导出包，但要让人看得见清单）。
pub(crate) fn is_secret_file(name: &str) -> bool {
    // ★ 白名单制（旧实现是子串黑名单）：只有确认不含凭据的台账文件不脱敏，其余一律脱敏。
    //   黑名单每新增一个凭据文件（oauth.json / cookies.json / token.json）都会默认落进
    //   「可明文导出」集合 —— 典型的失败开放。
    const PLAIN: &[&str] = &["usage_events.json", "usage.json", "requests.json"];
    !PLAIN.contains(&name)
}

pub(crate) async fn backup_export(State(st): State<AppState>) -> Json<Value> {
    backup_export_impl(st, false).await
}

/// 完整导出（含明文凭据）。仅供「备份数据」按钮使用：面板本身与备份文件都在同一台
/// 机器的同一用户下，账号文件本来就是明文，这里不额外扩大攻击面；默认的
/// `GET /api/backup` 仍然脱敏，避免随手一取就泄露。
pub(crate) async fn backup_export_full(State(st): State<AppState>) -> Json<Value> {
    backup_export_impl(st, true).await
}

/// 备份导出的公共实现（脱敏 / 含凭据只差一个开关）。
///
/// ★ 整段跑在 `spawn_blocking` 里：`collect_backup` 要 `read_dir` + 逐个
///   `metadata`/`read`（凭据文件动辄几十 KB 到几 MB），再加上 SQLite 的 WAL
///   checkpoint —— 这些都是阻塞调用。旧实现直接在 async handler 里做，备份期间
///   会把 tokio worker 线程占住：同一个 worker 上排队的**所有** /v1 对话请求一起
///   卡住，用户看到的是「点一下备份，全站对话停摆」。数据目录典型几十 MB，
///   在机械盘上能到秒级。
async fn backup_export_impl(st: AppState, include_secrets: bool) -> Json<Value> {
    let dd = aigw_relay::proxy::default_data_dir();
    // ★ 导出前先把 WAL 并回主库：WAL 模式下新写入只在 panel.db-wal 里，主库可能
    //   长期停在 4KB 空壳（实测 4096 vs 1.8MB）。不 checkpoint 就备份 = 备份出
    //   一个没有任何用量台账的空库，恢复回去还以为「备份好了」。
    let is_db = st.store.is_db();
    let store = st.store.clone();
    let out = tokio::task::spawn_blocking(move || {
        let ck = store.checkpoint();
        collect_backup(&dd, include_secrets, is_db, ck)
    })
    .await;
    match out {
        Ok(v) => v,
        Err(e) => {
            // panic 被捕获时不能装作备份成功（前端会提示「已导出 N 个文件」）
            tracing::error!("备份导出任务异常：{e}");
            Json(json!({
                "ok": false,
                "message": format!("备份导出失败（内部任务异常）：{e}"),
                "files": [],
            }))
        }
    }
}

pub(crate) fn collect_backup(
    dd: &std::path::Path,
    include_secrets: bool,
    is_db: bool,
    checkpoint_ok: bool,
) -> Json<Value> {
    let mut files: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dd) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // ★ 以 '.' 开头的是恢复时留下的旧文件副本（.bak-<ts>-accounts.json 之类）。
            //   它们同样以 .json 结尾，不排除的话每备份一次就把上一轮的备份再打包一遍，
            //   备份包越滚越大，且恢复端 safe_restore_name 本来就会拒这些名字（白收）。
            if name.starts_with('.') {
                continue;
            }
            // ★ panel.db-wal 必须一并导出：checkpoint 有可能失败（连接被占、SQLite busy），
            //   这时真实台账还在 -wal 里。旧实现只收 panel.db → 备份包里的主库是 4KB
            //   空壳，而恢复端虽然允许 -wal 却永远收不到它，备份/恢复对用量台账不闭环。
            //   -shm 故意不导出：它只是共享内存索引，SQLite 打开库时会重建；把旧 -shm
            //   塞回去反而可能与新的主库/日志不匹配。
            let ok_ext = name.ends_with(".json") || name == "panel.db" || name == "panel.db-wal";
            if !ok_ext {
                continue;
            }
            // 凭据文件一律不出内容：旧实现把 accounts.json（明文 Cookie/Token）整份
            // base64 回传，等于任何本机进程都能 GET 到全部凭据。导出只给文件名/大小，
            // 供人确认备份范围；真实离线备份请直接拷贝 data 目录。
            if is_secret_file(&name) && !include_secrets {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                files.push(json!({
                    "name": name,
                    "size": size,
                    "redacted": true,
                    "note": "含凭据，已省略内容（请直接拷贝 data 目录做离线备份）",
                }));
                continue;
            }
            match std::fs::read(&p) {
                Ok(bytes) => {
                    files.push(json!({"name": name, "bytes": base64_encode(&bytes), "size": bytes.len()}))
                }
                Err(e) => {
                    // 读失败不能静默跳过：备份里少一份文件 = 恢复后凭据/设置缺失，而用户
                    // 以为「已经备份好了」。宁可整个导出明确失败（前端 backupNow 见到
                    // ok===false 会抛错）。
                    return Json(json!({
                        "ok": false,
                        "message": format!("读取 {name} 失败：{e}"),
                        "files": [],
                        "count": 0,
                        "ts": now_ms(),
                    }));
                }
            }
        }
    }
    files.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut out = json!({
        "ok": true,
        "ts": now_ms(),
        "dir": dd.to_string_lossy(),
        "files": files,
        "count": files.len(),
        "db": is_db,
        "checkpoint": checkpoint_ok,
    });
    if is_db && !checkpoint_ok {
        // 明确告知：这次备份里 panel.db 可能不含最新台账，靠 -wal 兜底。
        out["checkpoint_warning"] = json!(
            "WAL checkpoint 未成功：panel.db 可能不含最新用量台账，本次已把 panel.db-wal 一并导出以保留数据；恢复时请把 panel.db 与 panel.db-wal 一起恢复（-shm 只是索引，SQLite 打开时会重建，不用带）"
        );
    }
    if include_secrets {
        out["include_secrets"] = json!(true);
        out["warning"] = json!("此文件含明文凭据（Cookie / Token / API Key），请勿上传或分享");
    }
    Json(out)
}

/// 恢复时允许写入的文件名判定。
///
/// 只用 `contains('/')` + `contains("..")` 挡不住两种真实写法：
/// - `C:x`：Windows 盘符**相对**路径，`Path::join` 遇到盘符前缀会丢掉基点，
///   于是写到「当前盘根目录」而不是数据目录；
/// - `panel.db:evil`：NTFS 备用数据流（ADS），能往任意文件里塞内容。
/// 这里要求：纯文件名、不含盘符/流分隔符、扩展名在白名单内。
pub(crate) fn safe_restore_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.contains(['/', '\\', ':', '\0'])
        && (name.ends_with(".json")
            || name == "panel.db"
            || name == "panel.db-wal"
            || name == "panel.db-shm")
}

/// 恢复某个文件时的落盘目标。
///
/// ★ 数据库文件不能就地覆盖：面板运行时 SQLite 独占打开 panel.db，Windows 上
///   覆盖写/重命名都会以共享冲突失败。所以改成写 `<name>.restore-pending`，
///   由下次启动的 `Store::open → apply_pending_restore` 原子替换。这样响应里的
///   `restart_required: true` 才是真的生效，而不是只剩一句提示。
pub(crate) fn restore_target(dd: &std::path::Path, name: &str) -> std::path::PathBuf {
    if name == "panel.db" || name == "panel.db-wal" || name == "panel.db-shm" {
        dd.join(format!("{name}.restore-pending"))
    } else {
        dd.join(name)
    }
}

/// 该文件名是否属于数据库文件（恢复时走暂存通道）。
pub(crate) fn is_db_file_name(name: &str) -> bool {
    name == "panel.db" || name == "panel.db-wal" || name == "panel.db-shm"
}

/// 备份恢复（先写 .bak-<ts> 旧文件再覆盖，损坏不静默）：POST /api/backup
///   {"files":[{"name":"accounts.json","bytes":"<b64>"}]}
pub(crate) async fn backup_restore(_st: State<AppState>, body: Bytes) -> Response {
    let dd = aigw_relay::proxy::default_data_dir();
    // ★ 与导出同理：恢复要写几十个文件（每个都先落 .bak 副本再覆盖），
    //   放 async handler 里做会把 tokio worker 占住，全站对话跟着卡。
    let ts = now_ms();
    let data = body.to_vec();
    match tokio::task::spawn_blocking(move || restore_into_at(&dd, &data, ts)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("备份恢复任务异常：{e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"ok": false, "message": format!("恢复失败（内部任务异常）：{e}")})),
            )
                .into_response()
        }
    }
}

/// 真正的恢复逻辑，时间戳由调用方给（测试要能构造「同名回滚副本已存在」的场景）。
pub(crate) fn restore_into_at(dd: &std::path::Path, body: &[u8], ts: u128) -> Response {
    let v: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": format!("JSON 解析失败：{e}")}))).into_response()
        }
    };
    let Some(files) = v.get("files").and_then(|x| x.as_array()) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "message": "缺 files 数组"}))).into_response();
    };
    // ★ 备份包体积：整体 body 已有 32MB 上限，但那是给聊天用的。往数据目录倒 32MB 的
    //   「accounts.json」既没有任何用途，也会把面板后续的每个请求拖慢（读配置、算容量）。
    //   单个文件 8MB 足够覆盖真实的 panel.db + 台账。
    const MAX_RESTORE_FILE: usize = 8 * 1024 * 1024;
    let mut restored = 0usize;
    let mut skipped = Vec::new();
    // 被成功改名成 .bak-<ts>-* 的原文件。响应里逐条报出来：文案承诺了「旧文件保留以便回滚」，
    // 那就必须能证明每一条真的留下来了。
    let mut backed_up: Vec<String> = Vec::new();
    // 数据库文件在服务运行时是 SQLite 独占打开的，Windows 上覆盖写必然失败。
    // 单独收集起来，在响应里给出可执行的下一步，而不是混在「写入失败」里让用户猜。
    let mut db_pending: Vec<String> = Vec::new();
    // 成功暂存（写进 .restore-pending、等下次启动落地）的数据库文件。
    let mut db_staged: Vec<String> = Vec::new();
    for f in files {
        let Some(name) = f.get("name").and_then(|x| x.as_str()) else { continue };
        if !safe_restore_name(name) {
            skipped.push(format!("{name}（非法名跳过）"));
            continue;
        }
        let Some(b64) = f.get("bytes").and_then(|x| x.as_str()) else { continue };
        let bytes = match base64_decode(b64) {
            Some(b) => b,
            None => {
                skipped.push(format!("{name}（b64 解码失败跳过）"));
                continue;
            }
        };
        if bytes.len() > MAX_RESTORE_FILE {
            skipped.push(format!(
                "{name}（{} 字节，超过单个文件 {} 字节的上限；真实备份的 panel.db 远小于此）",
                bytes.len(),
                MAX_RESTORE_FILE
            ));
            continue;
        }
        let is_db_file = is_db_file_name(name);
        let target = restore_target(&dd, name);
        if target.exists() {
            // ★ 回滚副本必须真的留下来才能覆盖：旧实现把 rename 的错误 let _ 吞掉，
            //   而响应文案照旧承诺「旧文件保留为 .bak-<ts>-* 以便回滚」—— 一旦改名失败
            //   （目录只读、副本已存在、被别的进程占用），用户以为还能回滚，其实旧文件
            //   已经被新内容覆盖，只剩备份包这一份了。宁可拒绝这次恢复，也不许静默丢数据。
            let bak = dd.join(format!(".bak-{ts}-{name}"));
            if let Err(e) = std::fs::rename(&target, &bak) {
                skipped.push(format!(
                    "{name}（旧文件备份失败：{e}；为了不把这个文件弄丢，本次没有覆盖它）"
                ));
                continue;
            }
            backed_up.push(name.to_string());
        }
        match aigw_store::write_atomic(&target, &bytes) {
            Ok(_) => {
                restored += 1;
                if is_db_file {
                    db_staged.push(name.to_string());
                }
            }
            Err(e) => {
                // ★ 数据库文件在服务运行时是被 SQLite 独占打开的（Windows 上 rename 覆盖
                //   会以共享冲突失败）。旧实现把失败整个吞掉：restored 不加、skipped 也不加，
                //   用户只看到「已恢复 N 个文件」，其中并不包含 panel.db —— 重启后用量台账
                //   还是旧的。这里必须逐文件说明原因。
                if is_db_file {
                    db_pending.push(name.to_string());
                    skipped.push(format!(
                        "{name}（暂存失败：{e}；请先停止 aigw 服务再恢复，或手动替换 data 目录下的 {name}）"
                    ));
                } else {
                    skipped.push(format!("{name}（写入失败：{e}）"));
                }
            }
        }
    }
    // 一个都没恢复成功时必须报错：原实现一律 200「已恢复文件」，用户以为恢复成功、
    // 重启后配置还是旧的（备份格式不对/文件名非法时最容易踩）。
    if restored == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "ok": false,
                "restored": 0,
                "skipped": skipped,
                "message": "没有任何文件被恢复；请检查备份包内的文件名与内容（允许：*.json / panel.db / panel.db-wal / panel.db-shm）",
            })),
        )
            .into_response();
    }
    // 恢复后重启服务即全量生效（custom/free/accounts 均以 data 目录文件为源）
    let mut out = json!({
        "ok": true,
        "restored": restored,
        "skipped": skipped,
        "dir": dd.to_string_lossy(),
        "restart_required": true,
        "note": if skipped.is_empty() {
            "已恢复文件；重启服务后全量生效。旧文件保留为 .bak-<ts>-* 以便回滚。"
        } else {
            "部分文件已恢复，但仍有文件被跳过（见 skipped）；重启服务后生效，被跳过的文件需要先停服务再手动处理。"
        },
    });
    // 数据库文件的落地语义要单独说清楚：它们被写成 *.restore-pending，由下次启动时
    // Store::open 原子替换（服务运行时 SQLite 独占打开 panel.db，就地覆盖必失败）。
    out["db_staged"] = json!(db_staged);
    out["backed_up"] = json!(backed_up);
    out["db_restored"] = json!(db_pending.is_empty());
    if !db_pending.is_empty() {
        out["db_pending"] = json!(db_pending);
        out["message"] = json!(format!(
            "配置已恢复，但数据库文件（{}）连暂存都没成功：请先停止 aigw 服务，再用同一备份包恢复一次（或手动替换 {} 目录下的 panel.db*）。",
            db_pending.join("、"),
            dd.to_string_lossy()
        ));
    } else if !db_staged.is_empty() {
        out["message"] = json!(format!(
            "已恢复 {} 个文件；数据库（{}）已暂存为 .restore-pending，重启 aigw 服务后生效（服务运行中无法直接覆盖 panel.db）。",
            restored,
            db_staged.join("、")
        ));
    }
    Json(out).into_response()
}
