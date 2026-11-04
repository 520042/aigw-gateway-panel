//! 模积分倍率表：静态离线表 + 在线刷新覆盖层（CodeBuddy / WorkBuddy 族）。
//!
//! ── 数据源与为什么离线 ─────────────────────────────────────
//! 离线表来自 CodeBuddy 国版客户端 APK 内嵌的模型目录
//! （assets/codebuddy-international-models.json，@tencent-ai/codebuddy-code@2.150.0，
//! 2026-09-13 提取）。在线接口要登录态才拿得到，离线表是未登录时唯一途径。
//!
//! ── 倍率是什么 ──────────────────────────────────────────────
//! x0.79 表示这一条请求按 0.79 的系数扣积分池额度。与号池的低余额优先 /
//! 快到期优先」加权（aigw-pool 的 pick）配合，面板能看懂「为什么这个号掉得快」。
//!
//! ── 在线刷新层 ──────────────────────────────────────────────
//! 静态表会过期（上游改倍率不通知任何人），所以运行时允许一层**在线缓存**覆盖：
//! 同名模型在线优先，整体来源随 rows 下发（rates_source），界面注明「离线(2026-09)
//! 还是「在线 MM-DD HH:MM」——**宁可不显示，不给人看错数；显示了就说清多新**。
//! 缓存同时落盘 data/model-rates.json，重启不丢。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;
use std::sync::RwLock;

/// (模型 id, 倍率)。id 与 /v1/models 里的裸模型名同一口径。
const RATES: &[(&str, &str)] = &[
    ("default-model", "0.79"),
    ("default-model-lite", "0.67"),
    ("fast-model", "0.34"),
    ("balanced-model", "0.59"),
    ("primary-model", "3.31"),
    ("deep-model", "3.33"),
    ("gpt-5.5", "3.31"),
    ("gpt-5.4", "1.65"),
    ("gpt-5.3-codex", "1.25"),
    ("gpt-5.1-codex", "0.90"),
    ("gpt-5.1-codex-mini", "0.18"),
    ("gemini-3.1-pro", "1.32"),
    ("gemini-3.0-flash", "0.33"),
    ("gemini-3.5-flash", "0.99"),
    ("gemini-2.5-flash", "0.22"),
    ("gemini-3.1-flash-lite", "0.17"),
    ("gemini-2.5-pro", "0.90"),
    ("deepseek-v3-2-volc", "0.29"),
    ("glm-5.0", "0.80"),
    ("kimi-k2.5", "0.45"),
    ("gemini-3.0-pro-image", "4.96"),
    ("gemini-3.1-flash-image", "1.78"),
    ("gemini-2.5-flash-image", "1.14"),
    ("hunyuan-image-v3.0", "5.00"),
    ("hunyuan-image-v2.0-general-edit", "5.00"),
    ("hunyuan-video-art", "10.00"),
    ("gpt-5.6-sol", "3.47"),
    ("gpt-5.6-terra", "1.39"),
    ("gpt-5.6-luna", "0.14"),
    ("glm-5.3", "0.79"),
    ("glm-5.2", "0.79"),
    ("hy3", "0.00"),
    ("kimi-k3", "1.62"),
    ("kimi-k2.6", "0.52"),
    ("minimax-m3", "0.25"),
];

/// 在线缓存：模型裸名 → 倍率；fetched_at 是 epoch 秒（0=从未成功）。
static ONLINE: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();
static ONLINE_AT: AtomicI64 = AtomicI64::new(0);

fn online() -> &'static RwLock<HashMap<String, String>> {
    ONLINE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// 用一批在线解析结果整体替换缓存（拿到才算新，失败时**不要**调这个）。
pub fn set_online(rates: HashMap<String, String>) {
    if rates.is_empty() {
        return;
    }
    let n = rates.len();
    *online().write().unwrap_or_else(|e| e.into_inner()) = rates;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ONLINE_AT.store(now, Ordering::Relaxed);
    tracing::info!("倍率在线刷新成功：{n} 条");
}

/// 来源标签（面板 tooltip / rates_source 字段用）。
pub fn source_label() -> String {
    let at = ONLINE_AT.load(Ordering::Relaxed);
    if at > 0 {
        // epoch → MM-DD HH:MM（本地时区，纯 std）
        let secs = at + 8 * 3600; // 面板语境固定北京时间即可（与 WB 服务同域）
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let (_y, m, d) = civil_from_days(days);
        format!(
            "在线 {:02}-{:02} {:02}:{:02}",
            m,
            d,
            rem / 3600,
            (rem % 3600) / 60
        )
    } else {
        "离线(2026-09)".to_string()
    }
}

/// Hinnant civil_from_days 逆算（无 chrono）。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn strip_bare(model: &str) -> &str {
    match model.rsplit_once('@') {
        Some((m, _)) => m,
        None => model,
    }
}

/// 查一个模型的积分倍率（在线优先，回退离线表）。输入可能是裸名、
/// 模型@平台 全名、或 vendor 前缀（x-able/glm-5.3）：都剥成裸名再查。
/// 只查**离线静态表**（不看在线覆盖层）。
///
/// 用途：在线刷新成功后，统计「有多少条是静态表里没有的新模型」——
/// 这个数字告诉用户「新模型的倍率到底拿到了没有」。
pub fn offline_rate_for(model: &str) -> Option<&'static str> {
    let bare = strip_bare(model);
    if let Some(hit) = RATES.iter().find(|(k, _)| *k == bare) {
        return Some(hit.1);
    }
    // vendor 前缀兜底（与 rate_for 同口径，否则统计会偏多）
    bare.rsplit('/')
        .next()
        .and_then(|tail| RATES.iter().find(|(k, _)| *k == tail))
        .map(|(_, v)| *v)
}
pub fn rate_for(model: &str) -> Option<String> {
    let bare = strip_bare(model);
    if let Some(g) = online().read().ok() {
        if let Some(v) = g.get(bare) {
            return Some(v.clone());
        }
        // vendor 前缀兜底
        if let Some(tail) = bare.rsplit('/').next() {
            if let Some(v) = g.get(tail) {
                return Some(v.clone());
            }
        }
    }
    if let Some(hit) = RATES.iter().find(|(id, _)| *id == bare) {
        return Some(hit.1.to_string());
    }
    if let Some(tail) = bare.rsplit('/').next() {
        if let Some(hit) = RATES.iter().find(|(id, _)| *id == tail) {
            return Some(hit.1.to_string());
        }
    }
    None
}

/// 缓存落盘（data/model-rates.json）。刷新成功后由任务调用。
pub fn save_cache(dir: &std::path::Path) {
    let at = ONLINE_AT.load(Ordering::Relaxed);
    if at <= 0 {
        return;
    }
    let snap: serde_json::Map<String, serde_json::Value> = online()
        .read()
        .map(|g| g.iter().map(|(k, v)| (k.clone(), serde_json::json!(v))).collect())
        .unwrap_or_default();
    let v = serde_json::json!({ "fetched_at": at, "rates": snap });
    let path = dir.join("model-rates.json");
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// 启动时回灌缓存（文件缺/坏静默跳过 = 用离线表）。
pub fn load_cache(dir: &std::path::Path) {
    let path = dir.join("model-rates.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(at) = v.get("fetched_at").and_then(|x| x.as_i64()) else { return };
    let Some(map) = v.get("rates").and_then(|x| x.as_object()) else { return };
    let mut rates = HashMap::new();
    for (k, val) in map {
        if let Some(s) = val.as_str() {
            rates.insert(k.clone(), s.to_string());
        }
    }
    if rates.is_empty() {
        return;
    }
    *online().write().unwrap_or_else(|e| e.into_inner()) = rates;
    ONLINE_AT.store(at, Ordering::Relaxed);
    tracing::info!("倍率在线缓存已回灌（{n} 条）", n = map.len());
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 纯静态检查（不查表值，避免与 ONLINE 全局态的测试互相污染）。
    #[test]
    fn table_keys_are_bare_names() {
        assert!(RATES.iter().all(|(id, _)| !id.contains('/') && !id.contains('@')));
        assert!(RATES.iter().all(|(_, r)| r.parse::<f64>().is_ok()), "倍率值必须可解析为数字");
    }

    /// 查表与在线覆盖的完整时序（一个测试内跑完：ONLINE 是全局态，
    /// 拆成两个并行测试会互相污染——上一轮就是这么假绿过一次的）。
    #[test]
    fn lookup_and_online_layer_in_order() {
        // ① 离线表阶段：三种形态 + 查不到返回 None
        assert_eq!(rate_for("balanced-model").as_deref(), Some("0.59"));
        assert_eq!(rate_for("kimi-k3@wb-gateway").as_deref(), Some("1.62"));
        assert_eq!(rate_for("z-ai/glm-5.3").as_deref(), Some("0.79"));
        assert_eq!(rate_for("some-unlisted-model"), None);
        assert!(source_label().starts_with("离线"));
        // ② 在线覆盖必须赢过离线
        let mut m = HashMap::new();
        m.insert("glm-5.3".to_string(), "0.42".to_string());
        set_online(m);
        assert_eq!(rate_for("glm-5.3").as_deref(), Some("0.42"), "在线值必须赢过离线");
        assert_eq!(rate_for("z-ai/glm-5.3").as_deref(), Some("0.42"), "vendor 前缀也要吃到在线值");
        assert!(source_label().starts_with("在线"));
        // ③ 空集替换必须被忽略（否则一次失败的刷新会清空好数据）
        set_online(HashMap::new());
        assert_eq!(rate_for("glm-5.3").as_deref(), Some("0.42"));
        // ④ 在线表没有的模型回退离线表
        assert_eq!(rate_for("balanced-model").as_deref(), Some("0.59"));
        // ⑤ offline_rate_for 只认静态表，不看在线层（在线刷新后的「新增 N 条」统计靠它）
        //    ★ 必须放在这个测试里：ONLINE 是全局态，拆成并行测试会互相污染
        //    （本轮就因此红过一次：新测试中途 set_online 把这里的状态冲掉了）。
        assert_eq!(offline_rate_for("glm-5.3"), Some("0.79"));
        assert_eq!(offline_rate_for("zhipu/glm-5.3"), Some("0.79"), "vendor 前缀同口径");
        assert_eq!(offline_rate_for("hy4-preview"), None);
        let mut m2 = HashMap::new();
        m2.insert("hy4-preview".to_string(), "0.55".to_string());
        set_online(m2);
        assert_eq!(rate_for("hy4-preview").as_deref(), Some("0.55"), "rate_for 看在线层");
        assert_eq!(
            offline_rate_for("hy4-preview"),
            None,
            "offline_rate_for 不该看在线层"
        );
        // 收尾复位，避免把在线态留给后续测试
        set_online(HashMap::new());
    }

}
