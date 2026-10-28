//! 历史裁剪（省 token）：对齐 magpie-community/plugins packages/history-trim
//! （移植自 OmniRoute lite 引擎 open-sse/services/compression/lite.ts）。
//!
//! ## 四步（顺序固定，不可调换）
//! 1. **空白折叠**：连续 3 个以上换行 → 1 个空行；行尾空格/制表符去掉。不动字词。
//! 2. **重复 system 提示词**：trim 后文字完全相同的第二条 system 删除。
//! 3. **工具结果截断**：超过 `max_tool_len` 的**历史**工具结果截断，
//!    回退到最近空白（80 字符窗口）并追加 `\n...[truncated]`。
//! 4. **连续重复消息**：与上一条**逐字段完全相同**（含 tool_calls / name）的删除。
//!
//! ## 关键安全边界（照抄参考项目，这是它最值得学的地方）
//! **只动历史，不动当前轮**：`turn_start` = 最后一条 assistant 消息之后的位置。
//! 最后一条 assistant 之后的每一条消息都是「模型还没读过的本轮内容」——
//! 里面的补丁、Markdown 硬换行、行尾制表符都是有意义的，一律不碰。
//! 没有 assistant 消息的历史 = 没有过去，四步全不动。
//!
//! 四步里只有「工具结果截断」会真的丢字，而它**只作用于历史**；
//! 其余三步只删「本来就重复了两遍」的东西。

use serde_json::{json, Map, Value};

pub const DEFAULT_MAX_TOOL_LEN: usize = 2000;
pub const MIN_MAX_TOOL_LEN: usize = 256;
pub const MAX_MAX_TOOL_LEN: usize = 1_000_000;
const LOOKBACK: usize = 80;
const TRUNC_SUFFIX: &str = "\n...[truncated]";

/// 裁剪配置。
#[derive(Debug, Clone)]
pub struct TrimCfg {
    pub max_tool_len: usize,
    /// false = 完全不截断工具结果。
    pub compress_tool_results: bool,
    /// true = 四步都不动 system 消息。
    pub preserve_system: bool,
}

impl Default for TrimCfg {
    fn default() -> Self {
        TrimCfg {
            max_tool_len: DEFAULT_MAX_TOOL_LEN,
            compress_tool_results: true,
            preserve_system: false,
        }
    }
}

impl TrimCfg {
    pub fn from_json(o: &Value) -> Self {
        let raw = o
            .get("maxToolLength")
            .and_then(|x| x.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_TOOL_LEN);
        TrimCfg {
            max_tool_len: raw.clamp(MIN_MAX_TOOL_LEN, MAX_MAX_TOOL_LEN),
            compress_tool_results: o
                .get("compressToolResults")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            preserve_system: o
                .get("preserveSystemPrompt")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        }
    }
}

/// 当前轮起点：最后一条 assistant 之后。没有 assistant 则 0（=整段都是本轮）。
fn turn_start(msgs: &[Value]) -> usize {
    for i in (0..msgs.len()).rev() {
        if msgs[i].get("role").and_then(|r| r.as_str()) == Some("assistant") {
            return i + 1;
        }
    }
    0
}

/// 空白折叠：≥3 换行 → 2 换行；行尾空白去掉。
fn fold_text(text: &str) -> String {
    // 先折叠换行，再去每行尾空白（顺序与参考实现一致）
    let mut out = String::with_capacity(text.len());
    let mut nl_run = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            nl_run += 1;
            // 只有发生换行时才判定，连续第 1、2 个换行保留，第 3 个起丢弃
            if nl_run <= 2 {
                out.push('\n');
            }
        } else {
            nl_run = 0;
            out.push(c);
        }
    }
    // 行尾空白：按行处理
    let trimmed: Vec<String> = out
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']).to_string())
        .collect();
    trimmed.join("\n")
}

fn content_str(v: &Value) -> Option<&str> {
    v.get("content").and_then(|c| c.as_str())
}

fn with_content(v: &Value, s: String) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    m.insert("content".into(), Value::String(s));
    Value::Object(m)
}

fn role_of(v: &Value) -> &str {
    v.get("role").and_then(|r| r.as_str()).unwrap_or("")
}

/// 主入口：就地裁剪 body.messages。返回 true = 改动了。
pub fn apply(body: &mut Value, cfg: &TrimCfg) -> bool {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return false;
    };
    if msgs.is_empty() {
        return false;
    }
    let start = turn_start(msgs);
    let mut changed = false;

    // ① 空白折叠（只动历史，跳过 system 若 preserve）
    for (i, m) in msgs.iter_mut().enumerate() {
        if i >= start {
            break;
        }
        if cfg.preserve_system && role_of(m) == "system" {
            continue;
        }
        if let Some(s) = content_str(m) {
            let folded = fold_text(s);
            if folded != s {
                *m = with_content(m, folded);
                changed = true;
            }
        }
    }

    // ② 重复 system 删除（trim 后文字相同）
    if !cfg.preserve_system {
        let mut seen: std::collections::HashSet<String> = Default::default();
        let mut keep: Vec<bool> = vec![true; msgs.len()];
        for (i, m) in msgs.iter().enumerate() {
            if i >= start {
                break;
            }
            if role_of(m) != "system" {
                continue;
            }
            let Some(s) = content_str(m) else { continue };
            let key = s.trim().to_string();
            if seen.contains(&key) {
                keep[i] = false;
                changed = true;
            } else {
                seen.insert(key);
            }
        }
        if changed {
            let mut idx = 0;
            msgs.retain(|_| {
                let k = keep[idx];
                idx += 1;
                k
            });
        }
    }

    // ③ 工具结果截断（只动历史）
    if cfg.compress_tool_results {
        let start = turn_start(msgs); // ② 可能删了元素，重算
        for (i, m) in msgs.iter_mut().enumerate() {
            if i >= start {
                break;
            }
            if let Some(newm) = cut_tool_results(m, cfg.max_tool_len) {
                *m = newm;
                changed = true;
            }
        }
    }

    // ④ 连续重复消息删除（逐字段相同，含 tool_calls/name）
    {
        let start = turn_start(msgs);
        let mut keep: Vec<bool> = vec![true; msgs.len()];
        for i in 1..msgs.len() {
            if i >= start {
                break;
            }
            // preserve_system 时 system 消息一步都不动（参考项目 `preserveSystemPrompt`
            // 的语义是「四步全跳过 system」，不是只跳过第 2 步）
            if cfg.preserve_system
                && (role_of(&msgs[i]) == "system" || role_of(&msgs[i - 1]) == "system")
            {
                continue;
            }
            if msgs[i] == msgs[i - 1] {
                keep[i] = false;
                changed = true;
            }
        }
        if changed {
            let mut idx = 0;
            msgs.retain(|_| {
                let k = keep[idx];
                idx += 1;
                k
            });
        }
    }

    changed
}

/// 按字符边界安全截断（emoji/CJK 无空白可回退时退一个码位）。
fn cut_text(text: &str, max_len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        return text.to_string();
    }
    let mut cut = max_len;
    // 回退到最近空白（最多 80 字符）
    let on_boundary = !chars[cut - 1].is_whitespace() && !chars.get(cut).map(|c| c.is_whitespace()).unwrap_or(true);
    if on_boundary {
        let lo = cut.saturating_sub(LOOKBACK);
        let mut found = None;
        // 向前找
        for i in (lo..cut).rev() {
            if chars[i].is_whitespace() {
                found = Some(i);
                break;
            }
        }
        if found.is_none() {
            // 向后找
            let hi = (cut + LOOKBACK).min(chars.len());
            for i in cut..hi {
                if chars[i].is_whitespace() {
                    found = Some(i);
                    break;
                }
            }
        }
        if let Some(f) = found {
            cut = f;
        }
    }
    format!("{}{}", chars[..cut].iter().collect::<String>(), TRUNC_SUFFIX)
}

/// 截断一条消息里的 tool_result 内容（Anthropic 形态）。
fn cut_tool_results(msg: &Value, max_len: usize) -> Option<Value> {
    if role_of(msg) != "user" {
        return None;
    }
    let arr = msg.get("content")?.as_array()?;
    let mut changed = false;
    let mut out: Vec<Value> = Vec::with_capacity(arr.len());
    for block in arr {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            out.push(block.clone());
            continue;
        }
        let mut b = block.as_object().cloned().unwrap_or_default();
        let mut inner_changed = false;
        match b.get("content").cloned() {
            Some(Value::String(s)) => {
                if s.chars().count() > max_len {
                    b.insert("content".into(), Value::String(cut_text(&s, max_len)));
                    inner_changed = true;
                }
            }
            Some(Value::Array(parts)) => {
                let mut np: Vec<Value> = Vec::with_capacity(parts.len());
                for p in parts {
                    let is_text = p.get("type").and_then(|t| t.as_str()) == Some("text");
                    let s = p.get("text").and_then(|t| t.as_str());
                    match (is_text, s) {
                        (true, Some(s)) if s.chars().count() > max_len => {
                            let mut po = p.as_object().cloned().unwrap_or_default();
                            po.insert("text".into(), Value::String(cut_text(s, max_len)));
                            np.push(Value::Object(po));
                            inner_changed = true;
                        }
                        _ => np.push(p.clone()),
                    }
                }
                if inner_changed {
                    b.insert("content".into(), Value::Array(np));
                }
            }
            _ => {}
        }
        if inner_changed {
            changed = true;
            out.push(Value::Object(b));
        } else {
            out.push(block.clone());
        }
    }
    if !changed {
        return None;
    }
    let mut m = msg.as_object().cloned().unwrap_or_default();
    m.insert("content".into(), Value::Array(out));
    Some(Value::Object(m))
}

/// OpenAI Chat 形态的 tool 消息（role=tool，content 是字符串）也要截断——
/// 参考项目只处理 Anthropic 的 tool_result block，但 OpenAI 的 role=tool
/// 是同类数据，长期跑 agent 时它才是大头。这里作为扩展一并处理。
fn cut_openai_tool_msg(msg: &Value, max_len: usize) -> Option<Value> {
    if role_of(msg) != "tool" {
        return None;
    }
    let s = content_str(msg)?;
    if s.chars().count() <= max_len {
        return None;
    }
    Some(with_content(msg, cut_text(s, max_len)))
}

/// 公开的便利入口：Anthropic + OpenAI 两种工具结果形态一起裁。
pub fn apply_all(body: &mut Value, cfg: &TrimCfg) -> bool {
    let mut changed = apply(body, cfg);
    if !cfg.compress_tool_results {
        return changed;
    }
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return changed;
    };
    let start = turn_start(msgs);
    for (i, m) in msgs.iter_mut().enumerate() {
        if i >= start {
            break;
        }
        if let Some(nv) = cut_openai_tool_msg(m, cfg.max_tool_len) {
            *m = nv;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folds_whitespace_in_history_only() {
        let mut b = json!({"messages":[
            {"role":"user","content":"a\n\n\n\nb   "},
            {"role":"assistant","content":"ok"},
            {"role":"user","content":"current\n\n\n\nkeep"}
        ]});
        assert!(apply(&mut b, &TrimCfg::default()));
        // 历史里折叠
        assert_eq!(b["messages"][0]["content"], "a\n\nb");
        // 当前轮（assistant 之后）不动
        assert_eq!(b["messages"][2]["content"], "current\n\n\n\nkeep");
    }

    #[test]
    fn drops_duplicate_system_prompt() {
        let mut b = json!({"messages":[
            {"role":"system","content":"SAME"},
            {"role":"system","content":"SAME  "},
            {"role":"assistant","content":"hi"}
        ]});
        assert!(apply(&mut b, &TrimCfg::default()));
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "重复 system 应删掉一条：{b}");
    }

    #[test]
    fn preserve_system_keeps_duplicates() {
        let mut b = json!({"messages":[
            {"role":"system","content":"SAME"},
            {"role":"system","content":"SAME"},
            {"role":"assistant","content":"hi"}
        ]});
        let cfg = TrimCfg { preserve_system: true, ..Default::default() };
        apply(&mut b, &cfg);
        assert_eq!(b["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn cuts_long_tool_result_in_history() {
        let long = "x".repeat(3000);
        let mut b = json!({"messages":[
            {"role":"user","content":[{"type":"tool_result","content":long}]},
            {"role":"assistant","content":"ok"}
        ]});
        assert!(apply(&mut b, &TrimCfg::default()));
        let s = b["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert!(s.ends_with(TRUNC_SUFFIX), "应带截断标记");
        assert!(s.chars().count() < 3000);
    }

    #[test]
    fn does_not_cut_current_turn_tool_result() {
        let long = "x".repeat(3000);
        let mut b = json!({"messages":[
            {"role":"assistant","content":"ok"},
            {"role":"user","content":[{"type":"tool_result","content":long}]}
        ]});
        apply(&mut b, &TrimCfg::default());
        let s = b["messages"][1]["content"][0]["content"].as_str().unwrap();
        assert_eq!(s.chars().count(), 3000, "当前轮的工具结果不能截");
    }

    #[test]
    fn compress_off_keeps_tool_results() {
        let long = "x".repeat(3000);
        let mut b = json!({"messages":[
            {"role":"user","content":[{"type":"tool_result","content":long}]},
            {"role":"assistant","content":"ok"}
        ]});
        let cfg = TrimCfg { compress_tool_results: false, ..Default::default() };
        apply(&mut b, &cfg);
        let s = b["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert_eq!(s.chars().count(), 3000);
    }

    #[test]
    fn drops_repeated_message() {
        let mut b = json!({"messages":[
            {"role":"user","content":"same"},
            {"role":"user","content":"same"},
            {"role":"assistant","content":"ok"}
        ]});
        assert!(apply(&mut b, &TrimCfg::default()));
        assert_eq!(b["messages"].as_array().unwrap().len(), 2, "{b}");
    }

    #[test]
    fn no_assistant_means_whole_thing_is_current_turn() {
        let mut b = json!({"messages":[
            {"role":"user","content":"a\n\n\n\nb"},
            {"role":"user","content":"same"},
            {"role":"user","content":"same"}
        ]});
        // 没有 assistant → turn_start=0 → 四步全不动
        assert!(!apply(&mut b, &TrimCfg::default()));
        assert_eq!(b["messages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn openai_tool_role_cut() {
        let long = "y".repeat(4000);
        let mut b = json!({"messages":[
            {"role":"user","content":"q"},
            {"role":"assistant","content":"a"},
            {"role":"tool","content":long},
            {"role":"assistant","content":"done"}
        ]});
        assert!(apply_all(&mut b, &TrimCfg::default()));
        let s = b["messages"][2]["content"].as_str().unwrap();
        assert!(s.ends_with(TRUNC_SUFFIX));
    }

    #[test]
    fn max_tool_len_clamped() {
        let c = TrimCfg::from_json(&json!({"maxToolLength": 10}));
        assert_eq!(c.max_tool_len, MIN_MAX_TOOL_LEN);
        let c2 = TrimCfg::from_json(&json!({"maxToolLength": 99_999_999}));
        assert_eq!(c2.max_tool_len, MAX_MAX_TOOL_LEN);
    }

    #[test]
    fn cut_text_respects_char_boundary() {
        // 全角/emoji 场景：不 panic、不产生半个字符
        let s = "中".repeat(3000);
        let cut = cut_text(&s, 2000);
        assert!(cut.starts_with("中"));
        assert!(cut.ends_with(TRUNC_SUFFIX));
    }

    #[test]
    fn empty_messages_noop() {
        let mut b = json!({"messages":[]});
        assert!(!apply(&mut b, &TrimCfg::default()));
    }
}

// ─────────────────────────────────────────────────────────────
// 全局配置（照抄 sysprompt 的 load/save_and_apply 模式）
// ─────────────────────────────────────────────────────────────

static CFG: std::sync::Mutex<Option<TrimCfg>> = std::sync::Mutex::new(None);

/// 从 data 目录加载（不存在 → 默认开启，max_tool_len=2000）。
///
/// 默认**开启**的理由：这项的直接收益是省 token（真金白银），而它的安全边界
/// 已经由「只动历史、不动当前轮」保证；关掉它才需要用户显式配置。
pub fn load(data_dir: &std::path::Path) -> TrimCfg {
    let p = data_dir.join("history_trim.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => {
                let mut c = TrimCfg::from_json(&v);
                // 显式 enabled=false 才关
                if v.get("enabled").and_then(|x| x.as_bool()) == Some(false) {
                    c.compress_tool_results = false;
                }
                c
            }
            Err(e) => {
                tracing::warn!("history_trim.json 解析失败（{e}），按默认开启处理");
                TrimCfg::default()
            }
        },
        Err(_) => TrimCfg::default(),
    }
}

pub fn save_and_apply(data_dir: &std::path::Path, cfg: &TrimCfg, enabled: bool) -> std::io::Result<()> {
    let p = data_dir.join("history_trim.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = json!({
        "enabled": enabled,
        "maxToolLength": cfg.max_tool_len,
        "compressToolResults": cfg.compress_tool_results,
        "preserveSystemPrompt": cfg.preserve_system,
    });
    aigw_store::write_json_atomic(&p, &v)?;
    apply_cfg(cfg, enabled);
    Ok(())
}

pub fn apply_cfg(cfg: &TrimCfg, enabled: bool) {
    let mut g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(if enabled {
        cfg.clone()
    } else {
        TrimCfg {
            compress_tool_results: false,
            ..cfg.clone()
        }
    });
}

/// 当前配置。None = 未初始化 → 返回 None 表示「不裁剪」（保守）。
pub fn current() -> Option<TrimCfg> {
    CFG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 管线入口：用全局配置裁剪。返回 true = 改动了。
pub fn apply_global(body: &mut Value) -> bool {
    match current() {
        Some(cfg) => apply_all(body, &cfg),
        None => false,
    }
}
