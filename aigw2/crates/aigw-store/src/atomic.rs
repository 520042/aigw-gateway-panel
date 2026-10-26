//! 原子落盘工具：临时文件 + fsync + rename，替换 `File::create` 直写。
//!
//! 为什么需要：`File::create` 会先截断目标文件，若进程在写入中途崩溃/被 taskkill，
//! `accounts.json` 会变成 0 字节或半截 JSON —— 账号池全部凭据丢失且无法自愈。
//! 临时文件 + rename 保证「要么旧内容完整，要么新内容完整」。
//!
//! Windows 语义：`std::fs::rename` 底层是 `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，
//! 可覆盖已存在文件；若目标被其他进程独占（杀软/编辑器）导致 rename 失败，
//! **不直接截断目标**（那正是本模块要消灭的破坏模式）：
//! 先重试若干次，仍失败则把原文件留档成 `.bak-<ts>` 再直写，并把上次内容一起返回。
//!
//! 临时文件名带 pid + 线程 id + 自增序号 + 纳秒，避免同进程两个线程写同一路径时撞名
//! （撞名在 Windows 上表现为 sharing violation → 旧版会掉进直写兜底）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn tmp_path(path: &Path) -> Option<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name()?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut n = std::ffi::OsString::from(".");
    n.push(name);
    n.push(format!(
        ".tmp-{}-{:?}-{seq}-{nanos}",
        std::process::id(),
        std::thread::current().id()
    ));
    Some(dir.join(n))
}

/// 原子写入 `data` 到 `path`（同目录临时文件 → fsync → rename）。
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    if !dir.as_os_str().is_empty() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Some(tmp) = tmp_path(path) else {
        return write_direct(path, data);
    };
    // 写临时文件 + fsync：确保数据真正落到磁盘后再 rename
    let write_res = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        // ★ 不退回直写：写临时文件都失败（磁盘满/无权限）时直写只会把原文件截断。
        return Err(e);
    }

    // rename 重试：Windows 上杀软/索引服务短暂占用目标文件是常见现象
    let mut last_err = None;
    for attempt in 0..5u32 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                fsync_dir(dir);
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    // 重试仍失败：把原文件留档一份再直写，最坏情况与修复前一致但至少可回滚
    let backup = backup_existing(path);
    tracing::warn!(
        "原子写 rename 失败（{} → {}）：{}，原文件已留档 {:?}，改直写",
        tmp.display(),
        path.display(),
        last_err
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".into()),
        backup
    );
    write_direct(path, data)
}

/// 最近一次落盘失败的现场（文件名 + 时间 + 原因）。
///
/// 为什么要有这张表：各 Store 的 save_locked 历史上只在失败时 tracing::warn! 一句，
/// 调用方**拿不到任何信号** —— 面板回「已保存」、内存里也确实改了，但盘上还是旧内容。
/// 重启之后，刚停用的模型又活了、刚删掉的 key 又回来了，而用户全程没看到过一句提示。
/// 这里在唯一的落盘出口做登记，再由 /api/health 暴露，让「你的配置没写进磁盘」
/// 这件事至少是可查的（不必把 17 处调用点的签名全部改一遍）。
static LAST_FAIL: std::sync::Mutex<Option<(String, u64, String)>> = std::sync::Mutex::new(None);

fn note_fail(path: &Path, e: &std::io::Error) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut g = LAST_FAIL.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some((name, at, e.to_string()));
}

/// 只清掉**同名文件**的失败记录：flags.json 写成功不代表 keys.json 写进去了。
/// （早期实现是无条件清空，一个无关文件写成功就能把真正的问题抹掉。）
fn note_ok(path: &Path) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let mut g = LAST_FAIL.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(g.as_ref(), Some((n, _, _)) if *n == name) {
        *g = None;
    }
}

/// 最近一次落盘失败：(文件名, 毫秒时间戳, 原因)。成功写盘之后自动清空。
pub fn last_write_failure() -> Option<(String, u64, String)> {
    LAST_FAIL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// ★ 测试专用：让所有会读写 `LAST_FAIL` 的测试互斥。
///
/// `LAST_FAIL` 是**进程级**静态：cargo test 默认多线程跑，同一 crate 里的其它测试
/// 只要写过一次 json（write_json_atomic → note_fail / note_ok），就能把本测试的登记
/// 抹掉或覆盖。实测症状是「单独跑 3 次全绿、全量跑偶发 FAILED」——
/// 这种 flaky 比真 bug 更耗人，因为它看起来像随机失败。
#[cfg(test)]
pub(crate) fn test_serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 原子写入 JSON。序列化失败返回 Err —— 旧版写 `"[]"`，对对象型结构（如 flags.json）
/// 下次 open 解析失败会被当成损坏文件重置，属于用一个新 bug 掩盖旧 bug。
pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let r = write_atomic(path, text.as_bytes());
    match &r {
        Ok(()) => note_ok(path),
        Err(e) => note_fail(path, e),
    }
    r
}

/// 尽力 fsync 父目录（Windows 上打开目录句柄会失败，失败即忽略）。
fn fsync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(f) = std::fs::File::open(dir) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// 把已存在的原文件复制一份 `.bak-<unix秒>`，返回备份路径。
fn backup_existing(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut n = path.file_name()?.to_os_string();
    n.push(format!(".bak-{ts}"));
    let bak = path.with_file_name(n);
    std::fs::copy(path, &bak).ok().map(|_| bak)
}

fn write_direct(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ 落盘失败必须留下可查的现场：各 Store 的 save_locked 只 warn 不返回错误，
    /// 面板因此照样回「已保存」。这里盯住「失败就登记、同名成功就清掉」。
    #[test]
    fn json_write_failure_is_recorded_and_cleared_by_a_later_success() {
        let _guard = test_serial();
        let tmp = tempfile::tempdir().unwrap();
        // 让目标路径被非空目录占住 → rename/直写都失败
        let p = tmp.path().join("probe.json");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("keep"), b"x").unwrap();
        assert!(write_json_atomic(&p, &vec![1, 2, 3]).is_err());
        let (name, at, err) = last_write_failure().expect("写失败必须登记");
        assert_eq!(name, "probe.json");
        assert!(at > 0);
        assert!(!err.is_empty(), "原因不能是空串");

        // 别的文件写成功不该把它抹掉
        let other = tmp.path().join("other.json");
        write_json_atomic(&other, &vec![9]).unwrap();
        assert!(
            last_write_failure().is_some(),
            "无关文件写成功不能抹掉别的文件的失败记录"
        );

        // 同一个文件写成功后清空
        std::fs::remove_dir_all(&p).unwrap();
        write_json_atomic(&p, &vec![1]).unwrap();
        assert!(last_write_failure().is_none(), "同名文件写成功后应清空");
    }

    #[test]
    fn writes_and_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.json");
        write_atomic(&p, b"first").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first");
        write_atomic(&p, b"second-longer-content").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "second-longer-content");
    }

    #[test]
    fn creates_missing_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nested").join("deep").join("b.json");
        write_atomic(&p, b"x").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x");
    }

    #[test]
    fn no_tmp_file_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("c.json");
        write_atomic(&p, b"payload").unwrap();
        let names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["c.json".to_string()], "不应残留临时文件：{names:?}");
    }

    #[test]
    fn write_json_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("d.json");
        write_json_atomic(&p, &vec![1, 2, 3]).unwrap();
        let v: Vec<i32> = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    /// 并发写同一路径不得因临时文件同名而失败或互相截断。
    #[test]
    fn concurrent_writes_to_same_path_are_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("conc.json");
        let mut hs = Vec::new();
        for i in 0..8 {
            let p = p.clone();
            hs.push(std::thread::spawn(move || {
                for k in 0..20 {
                    let payload = format!("writer-{i}-round-{k}");
                    write_atomic(&p, payload.as_bytes()).unwrap();
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        // 最终内容必须是某一次完整写入，而不是半截/空
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.starts_with("writer-"), "内容被截断：{s:?}");
        assert!(s.contains("-round-"), "内容被截断：{s:?}");
    }

    /// 序列化失败必须返回 Err，不能把 "[]" 写进目标文件。
    #[test]
    fn serialize_failure_does_not_write_garbage() {
        struct Bad;
        impl serde::Serialize for Bad {
            fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("e.json");
        std::fs::write(&p, b"{\"keep\":1}").unwrap();
        assert!(write_json_atomic(&p, &Bad).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"keep\":1}");
    }
}
