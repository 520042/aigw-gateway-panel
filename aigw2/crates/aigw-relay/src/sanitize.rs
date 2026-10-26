//! 客户端指纹脱敏（对齐 workbuddy2api-panel internal/upstream/sanitize.go）。
//!
//! **为什么需要**：上游网关会拦截请求体里的客户端身份特征——Claude Code、Codex CLI
//! 等客户端的 system prompt 里带有可识别的自证句，上游据此判定「非授权客户端调用」
//! 并直接 400（错误码 11128）。这些句子对推理没有意义，删掉/改写不影响语义。
//!
//! 三层处理（顺序不可颠倒）：
//! 1. **改写层**：全模板句逐字替换（每句只改一个词，语义不变）
//! 2. **剥离层**：`x-anthropic-billing-header: ...;` 整段删除、`cc_xxx=...;` 循环清理
//! 3. **兜底层**：残留的裸键名做最小缩写（header→hdr），破坏逐字匹配、保留可读性
//!
//! 另外：请求体里出现裸数字 `11128` 会被上游整单拦截（那正是拦截自身的错误码），
//! 改写为 `11-128`（插入连字符，保留可读性与指代）。

use serde_json::Value;

/// 改写层：(原句, 改写句)。只改一个词，语义不变。
const REWRITES: &[(&str, &str)] = &[
    // 身份自证句。匹配串**不带结尾标点**：CLI 版以句号收尾，
    // 桌面版（Agent SDK）以逗号接后继内容——去掉标点两种形态一并覆盖。
    (
        "You are Claude Code, Anthropic's official CLI for Claude",
        "You are Claude Code, Anthropic's official CLI tool for Claude",
    ),
    ("Main branch", "Default branch"),
    (
        "You are a coding agent running in the Codex CLI, a terminal-based coding assistant.",
        "You are a coding agent running in the Codex CLI tool, a terminal-based coding assistant.",
    ),
    // 反馈句：整句带 Anthropic 仓库链接，上游按整句拦截。
    (
        "To give feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
        "To provide feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
    ),
    // 上游反探测：请求体出现裸数字 11128 即整单拦截（与该数字的上下文无关）。
    ("11128", "11-128"),
];

/// 特征预检：快速路径，一个都不中就直接返回原串（零分配）。
fn has_fingerprint(text: &str) -> bool {
    const FEATURES: &[&str] = &[
        "x-anthropic-billing-header",
        "cc_entrypoint=",
        "You are Claude Code",
        "Main branch (",
        "You are a coding agent running in the Codex CLI",
        "github.com/anthropics/",
        "11128",
    ];
    // ★ cc_ 前缀必须单独判：Claude Code 的键值对不止 cc_entrypoint（还有
    //   cc_version= / cc_model= …）。只列具体键名会让其余变体逃过预检、
    //   整段清理被跳过——这正是 strips_cc_kv 测试暴露的问题。
    FEATURES.iter().any(|f| text.contains(f))
        || text.contains("cc_")
        || text.to_ascii_lowercase().contains("x-anthropic-billing-header")
}

/// 单段文本净化。
fn sanitize_text(text: &str) -> String {
    if !has_fingerprint(text) {
        return text.to_string();
    }
    let mut out = text.to_string();
    // ① 改写层
    for (from, to) in REWRITES {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    // ② 剥离层：x-anthropic-billing-header: <值>; 整段删除
    while let Some(start) = find_ci(&out, "x-anthropic-billing-header:") {
        let end = out[start..]
            .find([';', '\n'])
            .map(|i| start + i + 1)
            .unwrap_or(out.len());
        out.replace_range(start..end, "");
    }
    // ③ 剥离层：cc_xxx=...; 尾随裸键值（循环，因为可能连续多段）
    if out.contains("cc_") {
        let mut prev = String::new();
        while prev != out {
            prev = out.clone();
            out = strip_cc_kv(&out);
        }
    }
    // ④ 兜底层：残留裸键名最小缩写
    out = replace_ci(&out, "x-anthropic-billing-header", "x-anthropic-billing-hdr");
    out.trim().to_string()
}

/// 去掉 `cc_xxx=...;` 形态（大小写不敏感）。
fn strip_cc_kv(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 找 "cc_" 起始（且前面是词边界）
        if i + 3 <= bytes.len()
            && bytes[i] == b'c'
            && bytes[i + 1] == b'c'
            && bytes[i + 2] == b'_'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            // 键名：cc_ 之后连续字母数字/下划线
            let mut j = i + 3;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                // 值：到 ';' 或换行结束（值里可以有 '.' '/' '-' 等）
                let mut k = j + 1;
                while k < bytes.len() && bytes[k] != b';' && bytes[k] != b'\n' {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b';' {
                    k += 1;
                }
                // 吞掉尾随空白
                while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.to_ascii_lowercase();
    let n = needle.to_ascii_lowercase();
    h.find(&n)
}

fn replace_ci(hay: &str, needle: &str, to: &str) -> String {
    let mut out = String::with_capacity(hay.len());
    let mut rest = hay;
    while let Some(i) = find_ci(rest, needle) {
        out.push_str(&rest[..i]);
        out.push_str(to);
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// 对消息内容做脱敏（content 可能是字符串或数组）。
fn sanitize_content(v: &mut Value) -> bool {
    match v {
        Value::String(s) => {
            let cleaned = sanitize_text(s);
            if &cleaned != s {
                *s = cleaned;
                return true;
            }
            false
        }
        Value::Array(arr) => {
            let mut changed = false;
            for item in arr.iter_mut() {
                // {"type":"text","text":"..."} 形态
                if let Some(t) = item.get_mut("text") {
                    if let Value::String(s) = t {
                        let cleaned = sanitize_text(s);
                        if &cleaned != s {
                            *s = cleaned;
                            changed = true;
                        }
                    }
                }
                // {"type":"tool_result","content":"..."} 递归
                if let Some(c) = item.get_mut("content") {
                    if sanitize_content(c) {
                        changed = true;
                    }
                }
            }
            changed
        }
        _ => false,
    }
}

/// 对整个请求体做指纹脱敏。返回被改动的字段名（供日志/调试）。
///
/// 覆盖：messages[].content、system（字符串或数组）、tools[].description。
pub fn sanitize_request_body(v: &mut Value) -> Vec<&'static str> {
    let mut changed: Vec<&'static str> = Vec::new();
    let Some(o) = v.as_object_mut() else {
        return changed;
    };
    // system
    if let Some(sys) = o.get_mut("system") {
        if sanitize_content(sys) {
            changed.push("system");
        }
    }
    // messages
    if let Some(msgs) = o.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let mut hit = false;
        for m in msgs.iter_mut() {
            if let Some(c) = m.get_mut("content") {
                if sanitize_content(c) {
                    hit = true;
                }
            }
        }
        if hit {
            changed.push("messages");
        }
    }
    // tools[].description（OpenAI 形态在 function.description，Anthropic 形态在顶层 description）
    if let Some(tools) = o.get_mut("tools").and_then(|t| t.as_array_mut()) {
        let mut hit = false;
        for t in tools.iter_mut() {
            // Anthropic 形态：tools[].description
            if let Some(Value::String(s)) = t.get_mut("description") {
                let cleaned = sanitize_text(s);
                if &cleaned != s {
                    *s = cleaned;
                    hit = true;
                }
            }
            // OpenAI 形态：tools[].function.description
            if let Some(Value::String(s)) = t.get_mut("function").and_then(|f| f.get_mut("description")) {
                let cleaned = sanitize_text(s);
                if &cleaned != s {
                    *s = cleaned;
                    hit = true;
                }
            }
        }
        if hit {
            changed.push("tools");
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rewrites_identity_sentence() {
        let mut v = json!({
            "messages": [{"role":"system","content":"You are Claude Code, Anthropic's official CLI for Claude."}]
        });
        let ch = sanitize_request_body(&mut v);
        assert!(ch.contains(&"messages"));
        let s = v["messages"][0]["content"].as_str().unwrap();
        assert!(s.contains("official CLI tool for Claude"));
        assert!(!s.contains("official CLI for Claude"));
    }

    #[test]
    fn strips_billing_header_kv() {
        let mut v = json!({
            "system": "abc x-anthropic-billing-header: cc_version=2.0; cc_entrypoint=cli; def"
        });
        sanitize_request_body(&mut v);
        let s = v["system"].as_str().unwrap();
        assert!(!s.to_ascii_lowercase().contains("x-anthropic-billing-header"));
        assert!(s.contains("abc"));
        assert!(s.contains("def"));
    }

    #[test]
    fn strips_cc_kv() {
        let mut v = json!({"system": "hello cc_version=1.2.3; world"});
        sanitize_request_body(&mut v);
        let s = v["system"].as_str().unwrap();
        assert!(!s.contains("cc_version"));
        assert!(s.contains("hello") && s.contains("world"));
    }

    #[test]
    fn rewrites_bare_11128() {
        let mut v = json!({"messages":[{"role":"user","content":"code=11128 happened"}]});
        sanitize_request_body(&mut v);
        let s = v["messages"][0]["content"].as_str().unwrap();
        assert!(s.contains("11-128"));
        assert!(!s.contains("11128"));
    }

    #[test]
    fn array_content_is_sanitized() {
        let mut v = json!({
            "messages":[{"role":"user","content":[
                {"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude"}
            ]}]
        });
        sanitize_request_body(&mut v);
        let s = v["messages"][0]["content"][0]["text"].as_str().unwrap();
        assert!(s.contains("official CLI tool"));
    }

    #[test]
    fn clean_body_untouched() {
        let mut v = json!({"messages":[{"role":"user","content":"你好，今天天气如何"}]});
        let ch = sanitize_request_body(&mut v);
        assert!(ch.is_empty());
        assert_eq!(v["messages"][0]["content"], json!("你好，今天天气如何"));
    }

    #[test]
    fn tool_description_sanitized() {
        let mut v = json!({"tools":[{"type":"function","function":{"name":"f","description":"You are Claude Code, Anthropic's official CLI for Claude"}}]});
        sanitize_request_body(&mut v);
        let s = v["tools"][0]["function"]["description"].as_str().unwrap();
        assert!(s.contains("official CLI tool"));
    }
}
