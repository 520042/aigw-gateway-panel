//! 数据目录里的「留档文件」治理。
//!
//! 面板会不断产生三类留档，但从来没有清理过：
//! - 恢复备份时把原文件改名成 .bak-<毫秒时间戳>-<名字>（每恢复一次一组）；
//! - 原子写 rename 失败时复制一份 <名字>.bak-<秒时间戳>；
//! - JSON 解析失败时把坏文件留档成 <名字>.bad-<时间戳>。
//!
//! 这些文件是排障救命用的，不能一删了之；但无限增长会让数据目录越来越难看清，
//! 也会被备份导出无谓地拖大。这里的策略是按「留档方式 + 原文件名」分族，每族只留
//! 最新 N 份，其余删除。真实数据文件（accounts.json / panel.db / *.restore-pending）
//! 永远不碰 —— 它们不符合任何留档命名。

use std::path::Path;

/// 每族保留的份数。6 份足够覆盖「连着试几次恢复」的场景，磁盘占用也可控。
pub const KEEP_PER_FAMILY: usize = 6;

/// 留档文件的统计（面板 /api/health 展示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveStats {
    pub files: usize,
    pub bytes: u64,
}

/// 识别一个留档文件名，返回 (族标记, 原文件名, 时间戳)。
///
/// 时间戳必须是至少 8 位数字：点名接受 .bak-<ts>-x.json（前缀式）与
/// x.json.bad-<ts>（后缀式）两种真实命名，同时把凭据迁移留下的
/// accounts.bak.json、panel.db.pre-account-col.bak（没有数字时间戳）
/// 排除在外 —— 那是人工留的一次性文件，不该被自动轮转吃掉。
fn classify(name: &str) -> Option<(&'static str, String, u64)> {
    fn split_ts(tail: &str) -> Option<(u64, &str)> {
        let end = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(tail.len());
        if end < 8 {
            return None;
        }
        let ts = tail[..end].parse::<u64>().ok()?;
        Some((ts, &tail[end..]))
    }
    // 1) .bak-<ts>-<原名>
    if let Some(tail) = name.strip_prefix(".bak-") {
        let (ts, rest) = split_ts(tail)?;
        let base = rest.strip_prefix('-').unwrap_or(rest);
        if !base.is_empty() {
            return Some(("bak", base.to_string(), ts));
        }
        return None;
    }
    // 2) <原名>.bad-<ts>
    if let Some((prefix, tail)) = name.split_once(".bad-") {
        let (ts, _) = split_ts(tail)?;
        return Some(("bad", prefix.to_string(), ts));
    }
    // 3) <原名>.bak-<ts>
    if let Some((prefix, tail)) = name.split_once(".bak-") {
        let (ts, _) = split_ts(tail)?;
        return Some(("bak", prefix.to_string(), ts));
    }
    // 4) <原名>.pre-dpapi-<日期>
    if let Some((prefix, tail)) = name.split_once(".pre-dpapi-") {
        let (ts, _) = split_ts(tail)?;
        return Some(("pre-dpapi", prefix.to_string(), ts));
    }
    None
}

/// 统计留档文件的数量与总字节数。
pub fn stats(dir: &Path) -> ArchiveStats {
    let mut out = ArchiveStats { files: 0, bytes: 0 };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if classify(&name).is_some() {
            out.files += 1;
            out.bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        }
    }
    out
}

/// 按族裁掉超出 keep 份的旧留档，返回删除的文件数。
///
/// 排序用文件名里解析出的时间戳而不是 mtime：留档名里的时间戳就是产生时刻，
/// 而 mtime 会被复制/恢复操作改写（把一份老文件拷回来会让它看起来最新）。
pub fn prune(dir: &Path, keep: usize) -> usize {
    let keep = keep.max(1);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    type Family = (&'static str, String);
    let mut families: std::collections::HashMap<Family, Vec<(u64, std::path::PathBuf)>> =
        std::collections::HashMap::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some((marker, base, ts)) = classify(&name) {
            families.entry((marker, base)).or_default().push((ts, p));
        }
    }
    let mut removed = 0usize;
    for (_, mut items) in families {
        if items.len() <= keep {
            continue;
        }
        // 新的在前；同时间戳时按路径名稳定排序，保证结果可复现。
        items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, path) in items.into_iter().skip(keep) {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let t = std::env::temp_dir().join(format!("aigw-arch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::create_dir_all(&t).unwrap();
        t
    }

    #[test]
    fn prunes_each_family_down_to_the_keep_limit() {
        let t = tmp("a");
        for ts in [1700000000001u64, 1700000000002, 1700000000003, 1700000000004] {
            touch(&t, &format!("accounts.json.bad-{ts}"), "x");
        }
        for ts in [1700000000011u64, 1700000000012] {
            touch(&t, &format!(".bak-{ts}-custom.json"), "x");
        }
        // 真实数据文件与一次性人工留档：绝不能删
        touch(&t, "accounts.json", "{}");
        touch(&t, "accounts.bak.json", "legacy");
        touch(&t, "panel.db.pre-account-col.bak", "db");
        touch(&t, "panel.db.restore-pending", "pending");
        touch(&t, "flags.json", "{}");
        let removed = prune(&t, 2);
        assert_eq!(removed, 2, "accounts 坏档删 2 份，custom 刚好 2 份不动");
        let got = names(&t);
        assert!(got.contains(&"accounts.json.bad-1700000000003".to_string()), "{got:?}");
        assert!(got.contains(&"accounts.json.bad-1700000000004".to_string()), "{got:?}");
        assert!(!got.contains(&"accounts.json.bad-1700000000001".to_string()), "{got:?}");
        for must in [
            "accounts.json",
            "accounts.bak.json",
            "panel.db.pre-account-col.bak",
            "panel.db.restore-pending",
            "flags.json",
        ] {
            assert!(got.contains(&must.to_string()), "{must} 不该被删：{got:?}");
        }
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn keeps_newest_by_name_timestamp_not_mtime() {
        let t = tmp("b");
        // 先写「新」的，后写「老」的：mtime 顺序与名字里的时间戳相反
        touch(&t, ".bak-1700000000200-proxy.json", "new");
        touch(&t, ".bak-1700000000100-proxy.json", "old");
        assert_eq!(prune(&t, 1), 1);
        assert_eq!(names(&t), vec![".bak-1700000000200-proxy.json".to_string()]);
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn stats_counts_only_archives_and_ignores_directories() {
        let t = tmp("c");
        fs::create_dir_all(t.join("logs")).unwrap();
        touch(&t, "free_sites.json.bad-1792743927319", "12345");
        touch(&t, ".bak-1792650997101-proxy.json", "123");
        touch(&t, "accounts.json", "1234567890");
        let st = stats(&t);
        assert_eq!(st.files, 2);
        assert_eq!(st.bytes, 8);
        let _ = fs::remove_dir_all(&t);
    }

    #[test]
    fn names_without_a_numeric_stamp_are_not_archives() {
        assert!(classify("accounts.json").is_none());
        assert!(classify("accounts.bak.json").is_none());
        assert!(classify("panel.db.pre-account-col.bak").is_none());
        assert!(classify("panel.db.restore-pending").is_none());
        assert!(classify(".bak-short-proxy.json").is_none());
        assert_eq!(
            classify("accounts.json.bad-1792743927319").map(|(m, b, ts)| (m, b, ts)),
            Some(("bad", "accounts.json".to_string(), 1792743927319))
        );
        assert_eq!(
            classify(".bak-1792651587138-accounts.json").map(|(m, b, _)| (m, b)),
            Some(("bak", "accounts.json".to_string()))
        );
        assert_eq!(
            classify("custom.json.bak-1792651587138").map(|(m, b, _)| (m, b)),
            Some(("bak", "custom.json".to_string()))
        );
        assert_eq!(
            classify("accounts.json.pre-dpapi-20261023-162130").map(|(m, _b, ts)| (m, ts)),
            Some(("pre-dpapi", 20261023))
        );
    }
}
