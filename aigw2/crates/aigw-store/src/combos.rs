//! 模型链（combo）：一个模型名 = 一条有序候选链。
//!
//! 为什么需要（移植自 our-free-model `src/combos.js`）：额度墙、地域拒绝、凭据过期、
//! 上游 5xx，四种故障对用户都是「这轮挂了」，但本机明明还有别的平台空着能答。
//! 链式回退把这次失败变成一次用户看不见的重试：按顺序走候选，第一个产出内容的赢。
//!
//! 与 aigw2 已有的「同平台换账号」是两件事：那个在**一个平台内部**换凭据，这里
//! **跨平台/跨模型**换，因此必须放在 `chat_core` 解析模型之后、`resolve_spec` 之前。
//!
//! 两种策略：
//! - `fallback`  —— 永远从链头开始。可预测，链是「质量阶梯」时选它。
//! - `round-robin` —— 每轮从下一个候选开始（连续 `stickyLimit` 轮粘住同一个），
//!   让一组等价模型分摊负载。游标只在内存里：重启丢了只是换个起点，没有代价。
//!
//! 容量取**交集**不是并集：链头支持视觉不代表回退项也支持，链头有 1M 窗口不代表
//! 回退项不会在 128K 处截断。诚实的数字是每个候选都能兑现的那个。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 组合模型 id 前缀。故意不是合法平台 id，避免与 `模型@平台` 语义混淆。
pub const COMBO_PREFIX: &str = "combo:";

/// 一条候选链最多几节。上限的理由：链越长，一次请求最坏耗时就越大，
/// 而超过这个长度基本是误把模型清单整段贴进来了。
pub const MAX_COMBO_MEMBERS: usize = 16;

/// 最多几条链。同上，防误操作把配置文件撑爆。
pub const MAX_COMBOS: usize = 64;

/// 链名/id 长度上限。
pub const MAX_COMBO_NAME_LEN: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Combo {
    /// 完整 id，带 `combo:` 前缀。
    pub id: String,
    pub name: String,
    /// 有序候选（真实模型 id，可带 `@平台` 后缀）。
    pub models: Vec<String>,
    /// `fallback` | `round-robin`。
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// round-robin 游标连续粘住几轮。
    #[serde(default = "default_sticky")]
    pub sticky_limit: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_strategy() -> String {
    "fallback".to_string()
}
fn default_sticky() -> u32 {
    1
}
fn default_true() -> bool {
    true
}

/// `My Fast Chain` -> `combo:my-fast-chain`。
///
/// 非拉丁名（中文）也要有稳定且 URL 安全的身份：slug 为空或含非 ASCII 时，
/// 拼一段 sha256 前缀 —— 否则两条中文名会撞成同一个 `combo:u`。
pub fn combo_id(name: &str) -> String {
    let normalized = name.trim().to_lowercase();
    if normalized.is_empty() {
        return String::new();
    }
    let slug: String = normalized
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(48)
        .collect();
    let identity = if !slug.is_empty() && normalized.is_ascii() {
        slug
    } else {
        let h = sha256_hex(normalized.as_bytes());
        format!("{}-{}", if slug.is_empty() { "u" } else { &slug }, &h[..16])
    };
    format!("{COMBO_PREFIX}{identity}")
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 把落盘的原始值清洗成合法链列表。
///
/// 落盘的是用户输入，经过一个 JSON 文件存活过，所以每个字段都按不可信处理：
/// 非数组的 models 当空（随后整条链被丢掉，因为没有链节能答的模型不该出现在清单里）、
/// 非字符串 id 丢掉、未知 strategy 退回 `fallback` 而不是照抄一个拼错的值。
pub fn normalize(raw: &[Combo]) -> Vec<Combo> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in raw {
        if out.len() >= MAX_COMBOS {
            break;
        }
        let id = if row.id.starts_with(COMBO_PREFIX) {
            row.id.clone()
        } else {
            combo_id(if row.name.is_empty() { &row.id } else { &row.name })
        };
        if id == COMBO_PREFIX || !seen.insert(id.clone()) {
            continue;
        }
        let mut models: Vec<String> = Vec::new();
        for m in &row.models {
            let t = m.trim();
            if t.is_empty() || t == id || models.iter().any(|x| x == t) {
                continue;
            }
            if models.len() >= MAX_COMBO_MEMBERS {
                break;
            }
            models.push(t.to_string());
        }
        if models.is_empty() {
            continue;
        }
        let strategy = if row.strategy == "round-robin" {
            "round-robin".to_string()
        } else {
            "fallback".to_string()
        };
        out.push(Combo {
            id: id.clone(),
            name: if row.name.trim().is_empty() {
                id[COMBO_PREFIX.len()..].to_string()
            } else {
                row.name.trim().chars().take(MAX_COMBO_NAME_LEN).collect()
            },
            models,
            strategy,
            sticky_limit: row.sticky_limit.max(1),
            enabled: row.enabled,
        });
    }
    out
}

/// 链的旋转游标：`combo id -> (当前起点, 已连续粘住轮数)`。
///
/// 只在内存里，理由见模块注释。
type Rotation = std::collections::HashMap<String, (usize, u32)>;

pub struct ComboStore {
    inner: Mutex<Vec<Combo>>,
    rotation: Mutex<Rotation>,
    path: Option<PathBuf>,
}

impl ComboStore {
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join("combos.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<Vec<Combo>>(&s) {
                Ok(v) => normalize(&v),
                Err(e) => {
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let _ = std::fs::rename(
                        &path,
                        data_dir.join(format!("combos.json.bad-{ts}")),
                    );
                    tracing::warn!("combos.json 解析失败（{e}），已留档到 combos.json.bad-{ts}，从空表启动");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            inner: Mutex::new(raw),
            rotation: Mutex::new(Rotation::new()),
            path: Some(path),
        }
    }

    pub fn empty() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            rotation: Mutex::new(Rotation::new()),
            path: None,
        }
    }

    /// 在写闸门内取快照并落盘（各写方法的统一出口）。
    ///
    /// ★ 闸门必须同时盖住「取快照」与「写盘」：只盖写盘时，A 取旧快照后被抢占、
    ///   B 取新快照先写完，A 再把旧快照写回去 —— 磁盘留下旧数据。
    fn persist_now(&self) -> bool {
        crate::atomic::with_save_gate(|| {
            let snapshot = self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
            self.write_snapshot(&snapshot)
        })
    }

    /// 纯写入（调用方负责串行）。
    fn write_snapshot(&self, rows: &[Combo]) -> bool {
        let Some(p) = &self.path else {
            return false;
        };
        match serde_json::to_vec_pretty(rows) {
            Ok(bytes) => match crate::atomic::write_atomic(p, &bytes) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("combos.json 写入失败：{e}");
                    false
                }
            },
            Err(e) => {
                tracing::warn!("combos.json 序列化失败：{e}");
                false
            }
        }
    }

    pub fn rows(&self) -> Vec<Combo> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn get(&self, id: &str) -> Option<Combo> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|c| c.id == id)
            .cloned()
    }

    pub fn add(&self, name: &str, models: Vec<String>, strategy: &str, sticky: u32) -> Result<Combo, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("链名不能为空（写个能认出来的名字，比如「快速回退链」）".into());
        }
        if name.chars().count() > MAX_COMBO_NAME_LEN {
            return Err(format!(
                "链名过长（{} 个字符，上限 {MAX_COMBO_NAME_LEN}）",
                name.chars().count()
            ));
        }
        let cleaned: Vec<String> = models
            .into_iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        if cleaned.is_empty() {
            return Err("链里至少要有一个模型（空的链是一段永远答不上来的回退）".into());
        }
        if cleaned.len() > MAX_COMBO_MEMBERS {
            return Err(format!(
                "一条链最多 {MAX_COMBO_MEMBERS} 个模型（现在 {} 个）",
                cleaned.len()
            ));
        }
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if rows.len() >= MAX_COMBOS {
            return Err(format!("最多 {MAX_COMBOS} 条链，先删掉几条不用的"));
        }
        let id = combo_id(name);
        if id == COMBO_PREFIX {
            return Err("链名没法生成合法 id（全是符号？换个名字）".into());
        }
        if rows.iter().any(|c| c.id == id) {
            return Err(format!("已经有一条叫「{}」的链了（id {id}），换个名字", name));
        }
        let c = Combo {
            id,
            name: name.to_string(),
            models: cleaned,
            strategy: if strategy == "round-robin" { "round-robin".into() } else { "fallback".into() },
            sticky_limit: sticky.max(1),
            enabled: true,
        };
        rows.push(c.clone());
        drop(rows);
        let _ = self.persist_now();
        Ok(c)
    }

    pub fn update(
        &self,
        id: &str,
        name: Option<&str>,
        models: Option<Vec<String>>,
        strategy: Option<&str>,
        sticky: Option<u32>,
        enabled: Option<bool>,
    ) -> Result<Option<Combo>, String> {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(idx) = rows.iter().position(|c| c.id == id) else {
            return Ok(None);
        };
        if let Some(n) = name {
            let n = n.trim();
            if n.is_empty() {
                return Err("链名不能改成空（不想改名就别传 name 字段）".into());
            }
            if n.chars().count() > MAX_COMBO_NAME_LEN {
                return Err(format!("链名过长（{} 个字符，上限 {MAX_COMBO_NAME_LEN}）", n.chars().count()));
            }
            rows[idx].name = n.to_string();
        }
        if let Some(ms) = models {
            let cleaned: Vec<String> = ms.into_iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
            if cleaned.is_empty() {
                return Err("链里至少要有一个模型".into());
            }
            if cleaned.len() > MAX_COMBO_MEMBERS {
                return Err(format!("一条链最多 {MAX_COMBO_MEMBERS} 个模型（现在 {} 个）", cleaned.len()));
            }
            rows[idx].models = cleaned;
        }
        if let Some(s) = strategy {
            rows[idx].strategy = if s == "round-robin" { "round-robin".into() } else { "fallback".into() };
        }
        if let Some(k) = sticky {
            rows[idx].sticky_limit = k.max(1);
        }
        if let Some(e) = enabled {
            rows[idx].enabled = e;
        }
        let out = rows[idx].clone();
        drop(rows);
        let _ = self.persist_now();
        Ok(Some(out))
    }

    pub fn remove(&self, id: &str) -> bool {
        let mut rows = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let before = rows.len();
        rows.retain(|c| c.id != id);
        let changed = rows.len() != before;
        drop(rows);
        if changed {
            let _ = self.persist_now();
        }
        changed
    }

    /// 这一轮该按什么顺序试。
    ///
    /// `fallback` 原样返回；`round-robin` 轮转起点并保持 `sticky_limit` 轮 ——
    /// 严格每轮换会打死链上每个 provider 的 prompt cache，而严格粘死又让其它候选
    /// 变成摆设。
    pub fn candidates(&self, c: &Combo) -> Vec<String> {
        if c.models.is_empty() {
            return Vec::new();
        }
        if c.strategy != "round-robin" || c.models.len() == 1 {
            return c.models.clone();
        }
        let mut rot = self.rotation.lock().unwrap_or_else(|e| e.into_inner());
        let entry = rot.entry(c.id.clone()).or_insert((0, 0));
        let (start, used) = *entry;
        // 粘住够轮数才前进
        let next = if used + 1 >= c.sticky_limit {
            ((start + 1) % c.models.len(), 0)
        } else {
            (start, used + 1)
        };
        *entry = next;
        let start = start % c.models.len();
        let mut out = Vec::with_capacity(c.models.len());
        for i in 0..c.models.len() {
            out.push(c.models[(start + i) % c.models.len()].clone());
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_id_is_stable_and_url_safe() {
        assert_eq!(combo_id("My Fast Chain"), "combo:my-fast-chain");
        assert_eq!(combo_id("  Fast  "), "combo:fast");
        // 中文名也要有稳定身份，且不能全部撞成同一个
        let a = combo_id("快速链");
        let b = combo_id("慢速链");
        assert!(a.starts_with("combo:"), "{a}");
        assert_ne!(a, b, "两条中文名不能生成同一个 id");
        assert_eq!(a, combo_id("快速链"), "同名的 id 必须稳定");
    }

    #[test]
    fn normalize_drops_empty_chains_and_unknown_strategy() {
        let raw = vec![
            Combo { id: "combo:a".into(), name: "A".into(), models: vec!["m1".into()], strategy: "bogus".into(), sticky_limit: 0, enabled: true },
            Combo { id: "combo:empty".into(), name: "E".into(), models: vec![], strategy: "fallback".into(), sticky_limit: 1, enabled: true },
        ];
        let got = normalize(&raw);
        assert_eq!(got.len(), 1, "没有链节的链必须被丢掉");
        assert_eq!(got[0].strategy, "fallback", "未知策略退回 fallback");
        assert_eq!(got[0].sticky_limit, 1, "sticky_limit 0 必须被抬到 1");
    }

    #[test]
    fn fallback_always_starts_at_the_head() {
        let s = ComboStore::empty();
        let c = s.add("chain", vec!["a".into(), "b".into(), "c".into()], "fallback", 1).unwrap();
        for _ in 0..3 {
            assert_eq!(s.candidates(&c), vec!["a", "b", "c"]);
        }
    }

    #[test]
    fn round_robin_rotates_and_respects_sticky_limit() {
        let s = ComboStore::empty();
        let c = s.add("chain", vec!["a".into(), "b".into(), "c".into()], "round-robin", 1).unwrap();
        assert_eq!(s.candidates(&c)[0], "a");
        assert_eq!(s.candidates(&c)[0], "b");
        assert_eq!(s.candidates(&c)[0], "c");
        assert_eq!(s.candidates(&c)[0], "a", "必须回到链头");
        // sticky_limit=2：同一起点连续两轮
        let c2 = s.add("chain2", vec!["a".into(), "b".into()], "round-robin", 2).unwrap();
        assert_eq!(s.candidates(&c2)[0], "a");
        assert_eq!(s.candidates(&c2)[0], "a", "第二轮的起点必须还粘着");
    }

    #[test]
    fn add_rejects_blank_names_and_empty_chains() {
        let s = ComboStore::empty();
        assert!(s.add("   ", vec!["a".into()], "fallback", 1).is_err());
        assert!(s.add("ok", vec![], "fallback", 1).is_err());
        assert!(s.add("ok", vec!["  ".into()], "fallback", 1).is_err());
        assert!(s.add("ok", vec!["a".into()], "fallback", 1).is_ok());
        // 同名必须拒绝，否则第二条会静默覆盖第一条
        assert!(s.add("ok", vec!["b".into()], "fallback", 1).is_err());
    }

    #[test]
    fn chains_are_capped() {
        let s = ComboStore::empty();
        let many: Vec<String> = (0..40).map(|i| format!("m{i}")).collect();
        assert!(s.add("too-long", many, "fallback", 1).is_err());
        let ok: Vec<String> = (0..MAX_COMBO_MEMBERS).map(|i| format!("m{i}")).collect();
        assert!(s.add("just-right", ok, "fallback", 1).is_ok());
    }

    #[test]
    fn remove_and_update_work() {
        let s = ComboStore::empty();
        let c = s.add("chain", vec!["a".into()], "fallback", 1).unwrap();
        let u = s.update(&c.id, None, Some(vec!["x".into(), "y".into()]), None, None, Some(false)).unwrap().unwrap();
        assert_eq!(u.models, vec!["x", "y"]);
        assert!(!u.enabled);
        assert!(s.update(&c.id, Some("  "), None, None, None, None).is_err(), "不能改成空名字");
        assert!(s.remove(&c.id));
        assert!(s.get(&c.id).is_none());
    }
}
