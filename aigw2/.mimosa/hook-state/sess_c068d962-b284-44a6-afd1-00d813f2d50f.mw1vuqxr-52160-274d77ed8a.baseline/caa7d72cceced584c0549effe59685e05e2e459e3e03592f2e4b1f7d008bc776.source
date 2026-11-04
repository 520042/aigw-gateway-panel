//! 在线配置热更新（对齐 workbuddy2api internal/livecfg）。
//!
//! **为什么需要**：网关的调优参数（上游超时、单账号并发、失败重试次数、限流）
//! 原本是编译期常量或启动参数——改一个值要停服务、改配置、再启动，中途所有在飞
//! 请求全断。运维时「把并发从 2 调成 3」这种小事变成了一次服务中断。
//!
//! 这里把可调参数收进一个 `Arc<RuntimeCfg>`（原子读写、无锁），面板改完立即生效，
//! 正在跑的请求不受影响（它们已经取过自己的值）。
//!
//! 持久化：`data/runtime.json`，重启后恢复。值有硬上下界，越界一律夹到边界
//! （运维不该因为填了 99999 就把上游打爆）。

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 可热更新的运行时参数（全部用整数存储，避免浮点原子性争议）。
#[derive(Debug)]
pub struct RuntimeCfg {
    /// 单账号在单个模型上的最大并发（原 MAX_INFLIGHT=2）。
    max_inflight: AtomicU64,
    /// 上游读写超时秒数。
    upstream_timeout_s: AtomicU64,
    /// 单次请求最多换几个账号重试（原 MAX_ACCOUNT_TRIES=3）。
    max_account_tries: AtomicU64,
    /// 每 IP 每分钟请求上限（0 = 不限）。
    per_ip_rpm: AtomicU64,
    /// 全局每分钟请求上限（0 = 不限）。
    global_rpm: AtomicU64,
    /// 每把访问密钥每分钟上限（0 = 不限）。
    key_rpm: AtomicU64,
    /// ★ 2026-10-28：选号策略阈值（0=默认，1-100=reserve/ceiling 百分比）。
    select_ceiling: AtomicU64,
    /// 改配置的时刻（面板显示「上次修改」用）。
    updated_at: AtomicU64,
    path: std::path::PathBuf,
}

/// 参数定义：名称、字段索引、默认值、最小、最大。
/// 集中一处，改边界只改这里——避免面板、API、加载三处各写一份。
pub const FIELDS: &[(&str, u64, u64, u64)] = &[
    // (name, default, min, max)
    ("max_inflight", 2, 1, 16),
    ("upstream_timeout_s", 300, 10, 3600),
    ("max_account_tries", 3, 1, 10),
    ("per_ip_rpm", 0, 0, 100000),
    ("global_rpm", 0, 0, 1000000),
    ("key_rpm", 0, 0, 100000),
    // ★ 2026-10-28（对齐 magpie reserve）：选号策略阈值。0=默认策略，1-100=reserve/ceiling
    ("select_ceiling", 0, 0, 100),
];

fn clamp_of(name: &str, v: u64) -> u64 {
    for (n, _, lo, hi) in FIELDS {
        if *n == name {
            return v.clamp(*lo, *hi);
        }
    }
    v
}

impl RuntimeCfg {
    pub fn load(data_dir: &std::path::Path) -> Arc<RuntimeCfg> {
        let path = data_dir.join("runtime.json");
        let saved: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        let get = |name: &str, def: u64| -> u64 {
            saved
                .get(name)
                .and_then(|x| x.as_u64())
                .map(|v| clamp_of(name, v))
                .unwrap_or(def)
        };
        let updated_at = saved.get("updated_at").and_then(|x| x.as_u64()).unwrap_or(0);
        Arc::new(RuntimeCfg {
            max_inflight: AtomicU64::new(get("max_inflight", 2)),
            upstream_timeout_s: AtomicU64::new(get("upstream_timeout_s", 300)),
            max_account_tries: AtomicU64::new(get("max_account_tries", 3)),
            per_ip_rpm: AtomicU64::new(get("per_ip_rpm", 0)),
            global_rpm: AtomicU64::new(get("global_rpm", 0)),
            key_rpm: AtomicU64::new(get("key_rpm", 0)),
            select_ceiling: AtomicU64::new(get("select_ceiling", 0)),
            updated_at: AtomicU64::new(updated_at),
            path,
        })
    }

    pub fn max_inflight(&self) -> u32 {
        self.max_inflight.load(Ordering::Relaxed).max(1) as u32
    }
    pub fn upstream_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.upstream_timeout_s.load(Ordering::Relaxed).max(10))
    }
    pub fn max_account_tries(&self) -> usize {
        self.max_account_tries.load(Ordering::Relaxed).max(1) as usize
    }
    pub fn per_ip_rpm(&self) -> u64 {
        self.per_ip_rpm.load(Ordering::Relaxed)
    }
    pub fn global_rpm(&self) -> u64 {
        self.global_rpm.load(Ordering::Relaxed)
    }
    /// ★ 2026-10-28：选号策略阈值（0=默认，1-100=reserve/ceiling）。
    pub fn select_ceiling(&self) -> u32 {
        self.select_ceiling.load(Ordering::Relaxed) as u32
    }

    pub fn key_rpm(&self) -> u64 {
        self.key_rpm.load(Ordering::Relaxed)
    }

    /// 当前全部值（面板 GET 用）。
    pub fn snapshot(&self) -> Value {
        json!({
            "max_inflight": self.max_inflight(),
            "upstream_timeout_s": self.upstream_timeout_s.load(Ordering::Relaxed),
            "max_account_tries": self.max_account_tries(),
            "per_ip_rpm": self.per_ip_rpm(),
            "global_rpm": self.global_rpm(),
            "key_rpm": self.key_rpm(),
            "select_ceiling": self.select_ceiling(),
            "updated_at": self.updated_at.load(Ordering::Relaxed),
            // 字段元信息随快照下发，前端不用硬编码边界
            "fields": FIELDS.iter().map(|(n, d, lo, hi)| json!({
                "name": n, "default": d, "min": lo, "max": hi
            })).collect::<Vec<_>>(),
        })
    }

    /// 批量更新（面板 POST 用）。返回 `(改动字段, 被夹到边界的字段, 落盘错误)`。
    ///
    /// 只接受 FIELDS 里声明的名字——未知键忽略，不做「存着以后用」的宽容，
    /// 否则拼错的键会静默堆积在 runtime.json 里永远不生效。
    ///
    /// ★ 第三个返回值是**落盘结果**。旧实现是 `fn apply(..) -> (Vec, Vec)`，`save()`
    ///   内部把 `fs::write` / `fs::rename` 的 `Err` 全部 `let _ =` 吞掉 —— 于是内存里
    ///   改了、面板回「已更新 N 项，立即生效」、用户也确实看到行为变了，
    ///   **但重启后全部回退**。用户只会记得「这个面板的设置有 bug，改两次才生效」，
    ///   而真正的原因是磁盘写不进去（目录只读 / 磁盘满 / 杀软锁文件）。
    ///   现在把错误交回调用方，面板能明确说「已生效但没存下来」。
    pub fn apply(&self, patch: &Value) -> (Vec<String>, Vec<String>, Option<String>) {
        let mut changed: Vec<String> = Vec::new();
        let mut clamped: Vec<String> = Vec::new();
        let Some(obj) = patch.as_object() else {
            return (changed, clamped, None);
        };
        for (name, _, _, _) in FIELDS {
            let Some(raw) = obj.get(*name).and_then(|x| x.as_u64()) else { continue };
            let v = clamp_of(name, raw);
            if v != raw {
                clamped.push((*name).to_string());
            }
            let slot = match *name {
                "max_inflight" => &self.max_inflight,
                "upstream_timeout_s" => &self.upstream_timeout_s,
                "max_account_tries" => &self.max_account_tries,
                "per_ip_rpm" => &self.per_ip_rpm,
                "global_rpm" => &self.global_rpm,
                "key_rpm" => &self.key_rpm,
                "select_ceiling" => &self.select_ceiling,
                _ => continue,
            };
            if slot.swap(v, Ordering::Relaxed) != v {
                changed.push((*name).to_string());
            }
        }
        if changed.is_empty() {
            return (changed, clamped, None);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.updated_at.store(now, Ordering::Relaxed);
        let err = self.save().err();
        (changed, clamped, err)
    }

    /// 落盘。返回 `Err(人话原因)` —— 调用方必须把失败告诉用户（见 `apply` 的注释）。
    fn save(&self) -> Result<(), String> {
        let v = json!({
            "max_inflight": self.max_inflight(),
            "upstream_timeout_s": self.upstream_timeout_s.load(Ordering::Relaxed),
            "max_account_tries": self.max_account_tries(),
            "per_ip_rpm": self.per_ip_rpm(),
            "global_rpm": self.global_rpm(),
            "key_rpm": self.key_rpm(),
            "select_ceiling": self.select_ceiling(),
            "updated_at": self.updated_at.load(Ordering::Relaxed),
        });
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("无法创建目录 {}：{e}", dir.display()))?;
        }
        // 原子落盘：先写 .tmp 再 rename，避免写一半崩溃留下坏文件。
        // ★ 不走 `aigw_store::write_atomic`：本文件刻意保持「aigw-server 内部小工具」
        //   不引 store 的落盘闸门，避免与 Store 的写序互相阻塞（它们的写入频率都极低）。
        let text = serde_json::to_string_pretty(&v)
            .map_err(|e| format!("序列化失败：{e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &text).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| {
            // rename 失败时 tmp 会留在原地；清掉它以免下次误判（失败本身已上报）
            let _ = std::fs::remove_file(&tmp);
            format!("重命名到 {} 失败：{e}", self.path.display())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aigw-livecfg-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn defaults_when_no_file() {
        let c = RuntimeCfg::load(&tmp_dir("def"));
        assert_eq!(c.max_inflight(), 2);
        assert_eq!(c.max_account_tries(), 3);
    }

    #[test]
    fn apply_and_persist() {
        let d = tmp_dir("apply");
        let c = RuntimeCfg::load(&d);
        let (changed, clamped, err) =
            c.apply(&json!({"max_inflight": 5, "upstream_timeout_s": 600}));
        assert!(err.is_none(), "临时目录可写，落盘不应失败：{err:?}");
        assert!(changed.contains(&"max_inflight".to_string()));
        assert!(clamped.is_empty());
        assert_eq!(c.max_inflight(), 5);
        // 重新加载应保留
        let c2 = RuntimeCfg::load(&d);
        assert_eq!(c2.max_inflight(), 5);
        assert_eq!(c2.upstream_timeout(), std::time::Duration::from_secs(600));
    }

    #[test]
    fn out_of_range_is_clamped() {
        let c = RuntimeCfg::load(&tmp_dir("clamp"));
        let (_, clamped, _) = c.apply(&json!({"max_inflight": 9999}));
        assert!(clamped.contains(&"max_inflight".to_string()));
        assert_eq!(c.max_inflight(), 16, "应夹到上界");
    }

    #[test]
    fn unknown_keys_ignored() {
        let c = RuntimeCfg::load(&tmp_dir("unknown"));
        let (changed, _, _) = c.apply(&json!({"not_a_field": 123}));
        assert!(changed.is_empty());
    }

    /// ★ 落盘失败必须**可见**（旧实现静默吞掉 Err）。
    ///
    /// 场景：数据目录只读 / 磁盘满 / 杀软锁文件。旧行为是内存改了、面板回「已生效」、
    /// 用户也确实看到行为变了，但重启后全部回退 —— 用户只会记得「这面板设置改两次
    /// 才生效」，而真正的原因是磁盘写不进去。
    #[test]
    fn save_failure_is_reported_not_swallowed() {
        let d = tmp_dir("savefail");
        let c = RuntimeCfg::load(&d);
        // 把 runtime.json 的父目录换成一个**文件**，让 create_dir_all 必然失败。
        // （直接设只读目录在 Windows 上不可靠：管理员/ACL 可能仍然可写。）
        let blocked = d.join("blocked");
        std::fs::write(&blocked, b"x").unwrap();
        // 用一个新实例，path 指向被文件占住的目录
        let c2 = RuntimeCfg::load(&blocked.join("nested"));
        let (changed, _, err) = c2.apply(&json!({"max_inflight": 7}));
        assert!(changed.contains(&"max_inflight".to_string()), "内存应已改");
        assert_eq!(c2.max_inflight(), 7, "内存值应已生效");
        let e = err.expect("落盘失败必须返回错误，不能静默");
        assert!(!e.is_empty(), "错误原因不能是空串");
        // 原实例仍然正常（不受另一个实例的失败影响）
        let (_, _, ok) = c.apply(&json!({"max_inflight": 3}));
        assert!(ok.is_none(), "可写目录不应报错：{ok:?}");
    }

    #[test]
    fn zero_rpm_means_unlimited() {
        let c = RuntimeCfg::load(&tmp_dir("rpm"));
        let _ = c.apply(&json!({"per_ip_rpm": 0}));
        assert_eq!(c.per_ip_rpm(), 0);
    }
}
