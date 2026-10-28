//! 模型名重定向（对齐 magpie-community/plugins packages/model-map）。
//!
//! 场景：客户端写 `claude-sonnet-4`，你想让它实际打到 `glm-5.3`；
//! 或者把一组 `claude-*` 统一改写成别家模型。改写后**回复里的模型名会还原成
//! 客户端原本请求的名字** —— 这点很重要：Claude Code 会读回模型名来推算上下文窗口，
//! 看到上游真实名会算错。
//!
//! ## 支持的映射形态（照抄参考项目）
//! ```json
//! { "mapping": { "fast": "deepseek-chat", "claude-*": "glm-5.3" } }
//! ```
//! - **精确名**：`"fast": "deepseek-chat"`
//! - **通配**：`"claude-*": "glm-x"` —— `*` 匹配任意后缀，目标里的 `*` 用捕获的后缀替换
//! - **正则**（受限）：`"/^claude-(.*)$/": "glm-$1"` —— 只支持 `^前缀(.*)$` 形态
//!   （不引 regex crate 的取舍：真实用到的模式几乎都是这种，复杂正则如实不匹配）
//! - **链式**：`a → b → c` 会一直映到不再变；**成环**时停在重复前（不会死循环）
//!
//! ## 响应侧
//! 非流式：`body.model` / `body.response.model` / `body.modelVersion` 还原成请求名。
//! 流式：`message_start.message.model`、事件的 `model` 字段还原。
//! `keep_upstream_name=true` 则完全不动响应（保留上游真实名）。

use serde_json::Value;

/// 应用映射到请求体（就地）。返回 `Some(原始名)` 表示改写发生了（供响应侧还原）。
pub fn apply_request(body: &mut Value, rules: &[(String, String)], keep_upstream: bool) -> Option<String> {
    let _ = keep_upstream;
    let asked = body.get("model").and_then(|x| x.as_str())?.to_string();
    let mapped = map_name(&asked, rules)?;
    body["model"] = Value::String(mapped);
    Some(asked)
}

/// 纯函数版映射（精确名 + 前缀通配 + `/^前缀(.*)$/`，含链式与防环）。
pub fn map_name(name: &str, rules: &[(String, String)]) -> Option<String> {
    let mut cur = name.to_string();
    let mut seen: Vec<String> = vec![cur.clone()];
    for _ in 0..16 {
        let mut next: Option<String> = None;
        for (k, v) in rules {
            // /^prefix(.*)$/ 形态
            if let Some(inner) = k.strip_prefix('/').and_then(|s| s.strip_suffix('/')) {
                if let Some(prefix) = inner.strip_prefix('^').and_then(|s| s.strip_suffix("(.*)$")) {
                    if let Some(cap) = cur.strip_prefix(prefix) {
                        next = Some(v.replace("$1", cap));
                        break;
                    }
                }
                continue;
            }
            // 前缀通配 prefix*
            if let Some(prefix) = k.strip_suffix('*') {
                if let Some(suffix) = cur.strip_prefix(prefix) {
                    next = Some(v.replace('*', suffix));
                    break;
                }
                continue;
            }
            // 精确名
            if k == &cur {
                next = Some(v.clone());
                break;
            }
        }
        let Some(n) = next else { break };
        if n == cur || seen.contains(&n) {
            break;
        }
        seen.push(n.clone());
        cur = n;
    }
    if cur == name { None } else { Some(cur) }
}

/// 响应还原：把上游模型名改回客户端请求的名字。
///
/// 覆盖 4 处（与参考项目一致）：`model` / `response.model` / `modelVersion`、
/// 以及流式 `message_start.message.model`。
pub fn restore_model_name(v: &mut Value, asked: &str) -> bool {
    let mut hit = false;
    if let Some(o) = v.as_object_mut() {
        if o.contains_key("model") {
            o.insert("model".into(), Value::String(asked.to_string()));
            hit = true;
        }
        if let Some(inner) = o.get_mut("response").and_then(|r| r.as_object_mut()) {
            if inner.contains_key("model") {
                inner.insert("model".into(), Value::String(asked.to_string()));
                hit = true;
            }
        }
        if o.contains_key("modelVersion") {
            o.insert("modelVersion".into(), Value::String(asked.to_string()));
            hit = true;
        }
        // 流式 message_start 形态
        if let Some(ms) = o.get_mut("message").and_then(|m| m.as_object_mut()) {
            if ms.contains_key("model") {
                ms.insert("model".into(), Value::String(asked.to_string()));
                hit = true;
            }
        }
    }
    hit
}

// ─────────────────────────────────────────────────────────────
// 全局配置（照抄 sysprompt / history_trim 的模式）
// ─────────────────────────────────────────────────────────────

static RULES: std::sync::Mutex<Option<Vec<(String, String)>>> = std::sync::Mutex::new(None);
static KEEP_UPSTREAM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn load(data_dir: &std::path::Path) -> (Vec<(String, String)>, bool) {
    let p = data_dir.join("model_map.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => parse_cfg(&v),
            Err(e) => {
                tracing::warn!("model_map.json 解析失败（{e}），不启用模型重定向");
                (Vec::new(), false)
            }
        },
        Err(_) => (Vec::new(), false),
    }
}

pub fn parse_cfg(v: &Value) -> (Vec<(String, String)>, bool) {
    let keep = v
        .get("keep_upstream_name")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let m = v.get("mapping").filter(|x| x.is_object()).unwrap_or(v);
    let mut out = Vec::new();
    if let Some(obj) = m.as_object() {
        for (k, val) in obj {
            let Some(t) = val.as_str() else { continue };
            if t.is_empty() || k == "keep_upstream_name" || k == "mapping" {
                continue;
            }
            out.push((k.clone(), t.to_string()));
        }
    }
    (out, keep)
}

pub fn save_and_apply(
    data_dir: &std::path::Path,
    rules: &[(String, String)],
    keep_upstream: bool,
) -> std::io::Result<()> {
    let p = data_dir.join("model_map.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let map: serde_json::Map<String, Value> = rules
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    let v = serde_json::json!({
        "mapping": Value::Object(map),
        "keep_upstream_name": keep_upstream,
    });
    aigw_store::write_json_atomic(&p, &v)?;
    apply_cfg(rules, keep_upstream);
    Ok(())
}

pub fn apply_cfg(rules: &[(String, String)], keep_upstream: bool) {
    let mut g = RULES.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(rules.to_vec());
    KEEP_UPSTREAM.store(keep_upstream, std::sync::atomic::Ordering::Relaxed);
}

pub fn current() -> (Vec<(String, String)>, bool) {
    let rules = RULES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    (
        rules,
        KEEP_UPSTREAM.load(std::sync::atomic::Ordering::Relaxed),
    )
}

/// 管线入口：改写请求体里的 model。返回 `Some(原名)` 表示发生了改写。
pub fn apply_global(body: &mut Value) -> Option<String> {
    let (rules, keep) = current();
    if rules.is_empty() {
        return None;
    }
    apply_request(body, &rules, keep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn exact_mapping() {
        let r = rules(&[("fast", "deepseek-chat")]);
        assert_eq!(map_name("fast", &r), Some("deepseek-chat".into()));
        assert_eq!(map_name("other", &r), None);
    }

    #[test]
    fn wildcard_mapping() {
        let r = rules(&[("claude-*", "glm-*")]);
        assert_eq!(map_name("claude-sonnet", &r), Some("glm-sonnet".into()));
        assert_eq!(map_name("gpt-5", &r), None);
    }

    #[test]
    fn regex_like_mapping() {
        let r = rules(&[("/^claude-(.*)$/", "glm-$1")]);
        assert_eq!(map_name("claude-opus-4", &r), Some("glm-opus-4".into()));
    }

    #[test]
    fn chains_until_stable() {
        let r = rules(&[("a", "b"), ("b", "c")]);
        assert_eq!(map_name("a", &r), Some("c".into()), "应链式映到 c");
    }

    #[test]
    fn cycle_stops_safely() {
        let r = rules(&[("a", "b"), ("b", "a")]);
        // 成环：停在重复前，不死循环
        let got = map_name("a", &r);
        assert!(got.is_some(), "应产出某个值而不是死循环");
        assert!(got == Some("b".into()) || got == Some("a".into()));
    }

    #[test]
    fn apply_request_sets_body_model() {
        let mut b = json!({"model": "claude-sonnet", "messages": []});
        let r = rules(&[("claude-*", "glm-*")]);
        let asked = apply_request(&mut b, &r, false);
        assert_eq!(asked, Some("claude-sonnet".into()));
        assert_eq!(b["model"], "glm-sonnet");
    }

    #[test]
    fn restore_covers_all_four_places() {
        let mut body = json!({"model": "glm-sonnet"});
        assert!(restore_model_name(&mut body, "claude-sonnet"));
        assert_eq!(body["model"], "claude-sonnet");

        let mut b2 = json!({"response": {"model": "glm"}});
        assert!(restore_model_name(&mut b2, "asked"));
        assert_eq!(b2["response"]["model"], "asked");

        let mut b3 = json!({"modelVersion": "gemini-x"});
        assert!(restore_model_name(&mut b3, "asked"));
        assert_eq!(b3["modelVersion"], "asked");

        let mut b4 = json!({"message": {"model": "glm"}});
        assert!(restore_model_name(&mut b4, "asked"));
        assert_eq!(b4["message"]["model"], "asked");
    }

    #[test]
    fn parse_cfg_both_forms() {
        // mapping 包裹形态
        let (r, keep) = parse_cfg(&json!({"mapping":{"fast":"x"},"keep_upstream_name":true}));
        assert_eq!(r.len(), 1);
        assert!(keep);
        // New API 扁平形态
        let (r2, _) = parse_cfg(&json!({"fast":"x","other":"y"}));
        assert_eq!(r2.len(), 2);
    }
}

// ─────────────────────────────────────────────────────────────
// 本次请求的「原名」记录（供响应侧还原）
// ─────────────────────────────────────────────────────────────
//
// 为什么用 thread-local 而不是改函数签名：chat_core 有十来个返回点
// （成功/失败/换号重试/流式/非流式），把 asked 一路传下去要改十几个签名，
// 而它是**同一请求同一线程**内的短生命周期状态。线程本地正好匹配这个语义。
// 每次请求开始前 clear，结束时不依赖清理（下个请求会覆盖）。

thread_local! {
    static ASKED: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// 记下本次请求客户端原本要的模型名（改写发生时由请求侧调用）。
pub fn remember_asked(name: &str) {
    ASKED.with(|a| *a.borrow_mut() = Some(name.to_string()));
}

/// 取本次请求的原始模型名（响应侧还原用）。
pub fn asked_now() -> Option<String> {
    ASKED.with(|a| a.borrow().clone())
}

/// 清空（每个请求入口调一次，避免串到下个请求）。
pub fn clear_asked() {
    ASKED.with(|a| *a.borrow_mut() = None);
}

/// 响应侧还原：若有记录则改回原名，返回 true 表示改了。
pub fn restore_global(v: &mut Value) -> bool {
    if current().1 {
        // keep_upstream_name=true：保留上游真实名，不还原
        return false;
    }
    match asked_now() {
        Some(asked) => restore_model_name(v, &asked),
        None => false,
    }
}

/// 用全局规则映射单个模型名（不改 body，供 @平台 拆分后的裸名映射用）。
pub fn map_name_global(name: &str) -> Option<String> {
    let (rules, _) = current();
    if rules.is_empty() {
        return None;
    }
    map_name(name, &rules)
}
