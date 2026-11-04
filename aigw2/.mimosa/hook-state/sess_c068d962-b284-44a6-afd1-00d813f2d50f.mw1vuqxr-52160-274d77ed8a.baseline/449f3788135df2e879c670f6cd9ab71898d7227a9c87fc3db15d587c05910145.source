//! 思考标签处理（对齐 magpie-community/plugins packages/think-tags）。
//!
//! ## 为什么需要（省 token）
//! 有些模型把思考过程直接写进**正文**：`<think>…</think>好，答案是……`。
//! 智能体（Claude Code / Codex）会把整段正文塞进历史并在下一轮**原样发回**，
//! 于是每轮都在为同一段思考重复付费。把这个块剥掉，历史里只剩真答案。
//!
//! ## 核心规则（照抄参考实现，这几条边界是它的精华）
//! 1. **只剥开头**：`<think>` 必须在文本开头（允许前导空白）。
//!    正文中间出现的 `<think>`（模型在举例/写代码）一律不动。
//! 2. **跳过 Markdown 代码里的 `</think>`**：块内的行内代码 `` `</think>` ``
//!    或围栏代码块里的同名文字是**思考里的举例**，不是块的结束符。
//!    误判会把思考截断到一半、后半段漏进正文。
//! 3. 剥完后开头的空白再去一次。
//!
//! ## 两种模式
//! - `strip`（默认）：从正文里剥掉思考块
//! - `to_content`：把 Chat 回复的 `reasoning_content` 塞回正文成 `<think>` 块
//!   （给只显示正文的客户端看）。**仅 Chat**：Anthropic 的 thinking block 带签名，
//!   转成文本后下一轮会验签失败。
//!
//! ## 与「反向」的区别
//! 我们各 adapter 已把上游的 reasoning 增量映射成 `reasoning_content`（那是**上游给的结构化思考**）。
//! 本模块处理的是**模型自己写进正文的标签**，两者不冲突。

use serde_json::Value;

const OPEN: &str = "<think>";
const CLOSE: &str = "</think>";

/// 剥掉开头处的 `<think>…</think>` 块。没有则原样返回。
///
/// `find_close` 会跳过 Markdown 代码里的 `</think>`。
pub fn strip_leading(text: &str) -> String {
    let lead_len = text.len() - text.trim_start().len();
    let rest = &text[lead_len..];
    if !rest.starts_with(OPEN) {
        return text.to_string();
    }
    let body = &rest[OPEN.len()..];
    match find_close_outside_code(body) {
        Some(idx) => body[idx + CLOSE.len()..].trim_start().to_string(),
        None => text.to_string(), // 没闭合 → 不动（宁可保留也不误删）
    }
}

/// 在 `s` 中找第一个**不在 Markdown 代码里**的 `</think>`，返回其**字节**位置。
///
/// 追踪三件事（与参考实现一致）：行内代码的围栏长度、围栏代码块的围栏长度、
/// 当前行是否已过前导空格。
///
/// 注意：必须按**字符边界**推进 —— 中文/emoji 是多字节，按字节 `i += 1` 再
/// `&s[i..]` 会 panic（实测踩过：`start byte index 1 is not a char boundary`）。
fn find_close_outside_code(s: &str) -> Option<usize> {
    let mut span = 0usize;
    let mut fence = 0usize;
    let mut line_lead = true;
    // 遍历字符，同时用 char_indices 拿到字节位置
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if c == '\n' {
            line_lead = true;
            continue;
        }
        if line_lead {
            if c == ' ' || c == '\t' {
                continue;
            }
            line_lead = false;
            // 行首连续 backtick 且 ≥3 → 围栏开关
            if c == '`' {
                let n = 1 + count_run(&mut iter, '`');
                if n >= 3 {
                    if fence == 0 {
                        fence = n;
                    } else if fence == n {
                        fence = 0;
                    }
                    continue;
                }
            }
        }
        if c == '`' && fence == 0 {
            let n = 1 + count_run(&mut iter, '`');
            if span == 0 {
                span = n;
            } else if span == n {
                span = 0;
            }
            continue;
        }
        if span == 0 && fence == 0 && s[i..].starts_with(CLOSE) {
            return Some(i);
        }
    }
    None
}

/// 从迭代器里连续消费 `want` 字符，返回消费个数。
fn count_run<I: Iterator<Item = (usize, char)>>(iter: &mut std::iter::Peekable<I>, want: char) -> usize {
    let mut n = 0;
    while let Some((_, c)) = iter.peek() {
        if *c != want {
            break;
        }
        iter.next();
        n += 1;
    }
    n
}

/// 非流式响应处理。
///
/// 覆盖三处正文位置（与参考项目一致）：
/// Chat `choices[].message.content`、Anthropic `content[].text`、
/// Responses `output[].content[].text`。
pub fn strip_response(v: &mut Value) -> bool {
    let mut changed = false;
    // Chat
    if let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for c in choices {
            if let Some(m) = c.get_mut("message") {
                if let Some(s) = m.get_mut("content").and_then(|x| x.as_str().map(String::from)) {
                    let t = strip_leading(&s);
                    if t != s {
                        m["content"] = Value::String(t);
                        changed = true;
                    }
                }
            }
        }
    }
    // Anthropic：只动第一个 text block（参考实现同）
    if let Some(arr) = v.get_mut("content").and_then(|c| c.as_array_mut()) {
        if let Some(first) = arr.iter_mut().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")) {
            if let Some(s) = first.get("text").and_then(|x| x.as_str().map(String::from)) {
                let t = strip_leading(&s);
                if t != s {
                    first["text"] = Value::String(t);
                    changed = true;
                }
            }
        }
    }
    // Responses
    if let Some(items) = v.get_mut("output").and_then(|o| o.as_array_mut()) {
        for it in items {
            if it.get("type").and_then(|t| t.as_str()) != Some("message") {
                continue;
            }
            if let Some(parts) = it.get_mut("content").and_then(|c| c.as_array_mut()) {
                for p in parts {
                    if p.get("type").and_then(|t| t.as_str()) != Some("output_text") {
                        continue;
                    }
                    if let Some(s) = p.get("text").and_then(|x| x.as_str().map(String::from)) {
                        let t = strip_leading(&s);
                        if t != s {
                            p["text"] = Value::String(t);
                            changed = true;
                        }
                    }
                }
            }
        }
    }
    changed
}

/// `to_content` 模式：把 Chat 回复的 reasoning_content 塞进正文成 `<think>` 块。
///
/// 仅 Chat —— Anthropic 的 thinking block 带签名，转文本后下一轮会验签失败。
pub fn reasoning_to_text(v: &mut Value) -> bool {
    let Some(choices) = v.get_mut("choices").and_then(|c| c.as_array_mut()) else {
        return false;
    };
    let mut changed = false;
    for c in choices {
        let Some(m) = c.get_mut("message").and_then(|x| x.as_object_mut()) else { continue };
        let reasoning = m
            .get("reasoning_content")
            .and_then(|x| x.as_str())
            .or_else(|| m.get("reasoning").and_then(|x| x.as_str()))
            .map(String::from);
        let Some(r) = reasoning else { continue };
        if r.is_empty() {
            continue;
        }
        let content = m.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
        m.insert(
            "content".into(),
            Value::String(format!("{OPEN}\n{r}\n{CLOSE}\n\n{content}")),
        );
        m.remove("reasoning_content");
        m.remove("reasoning");
        changed = true;
    }
    changed
}

// ─────────────────────────────────────────────────────────────
// 全局配置
// ─────────────────────────────────────────────────────────────

static MODE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn load(data_dir: &std::path::Path) -> Option<String> {
    let p = data_dir.join("think_tags.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => {
                let enabled = v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false);
                if !enabled {
                    return None;
                }
                Some(
                    v.get("mode")
                        .and_then(|x| x.as_str())
                        .unwrap_or("strip")
                        .to_string(),
                )
            }
            Err(e) => {
                tracing::warn!("think_tags.json 解析失败（{e}），不启用");
                None
            }
        },
        Err(_) => None,
    }
}

pub fn save_and_apply(data_dir: &std::path::Path, mode: Option<&str>) -> std::io::Result<()> {
    let p = data_dir.join("think_tags.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = match mode {
        Some(m) => serde_json::json!({"enabled": true, "mode": m}),
        None => serde_json::json!({"enabled": false}),
    };
    aigw_store::write_json_atomic(&p, &v)?;
    apply_cfg(mode);
    Ok(())
}

pub fn apply_cfg(mode: Option<&str>) {
    let mut g = MODE.lock().unwrap_or_else(|e| e.into_inner());
    *g = mode.map(|m| m.to_string());
}

pub fn current_mode() -> Option<String> {
    MODE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 管线入口：按当前模式处理响应体。返回 true = 改动了。
pub fn apply_global(v: &mut Value) -> bool {
    match current_mode().as_deref() {
        Some("to_content") => reasoning_to_text(v),
        Some("strip") => strip_response(v),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_leading_block() {
        assert_eq!(strip_leading("<think>想一下</think>答案"), "答案");
        assert_eq!(strip_leading("  <think>\n思考\n</think>\n\n正文"), "正文");
    }

    #[test]
    fn keeps_mid_text_block() {
        // 不在开头 → 不动（模型可能在举例）
        let s = "答案是 42，比如 <think>x</think> 这种写法";
        assert_eq!(strip_leading(s), s);
    }

    #[test]
    fn skips_close_inside_inline_code() {
        // 行内代码里的 </think> 是思考的举例，不是结束
        let s = "<think>思考里举例 `</think>` 这个词</think>真答案";
        assert_eq!(strip_leading(s), "真答案");
    }

    #[test]
    fn skips_close_inside_fence() {
        let s = "<think>思考\n```\n</think>\n```\n还有思考</think>答案";
        assert_eq!(strip_leading(s), "答案");
    }

    #[test]
    fn unclosed_block_is_left_alone() {
        // 没有闭合 → 宁可保留也不误删
        let s = "<think>只有开头没结尾";
        assert_eq!(strip_leading(s), s);
    }

    #[test]
    fn strip_chat_response() {
        let mut v = json!({"choices":[{"message":{"content":"<think>想了</think>好"}}]});
        assert!(strip_response(&mut v));
        assert_eq!(v["choices"][0]["message"]["content"], "好");
    }

    #[test]
    fn strip_anthropic_and_responses() {
        let mut a = json!({"content":[{"type":"text","text":"<think>x</think>答"}]});
        assert!(strip_response(&mut a));
        assert_eq!(a["content"][0]["text"], "答");

        let mut r = json!({"output":[{"type":"message","content":[{"type":"output_text","text":"<think>y</think>复"}]}]});
        assert!(strip_response(&mut r));
        assert_eq!(r["output"][0]["content"][0]["text"], "复");
    }

    #[test]
    fn to_content_moves_reasoning_into_text() {
        let mut v = json!({"choices":[{"message":{"content":"答案","reasoning_content":"想了很多"}}]});
        assert!(reasoning_to_text(&mut v));
        let c = v["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(c.contains("<think>"));
        assert!(c.contains("想了很多"));
        assert!(c.ends_with("答案"));
        assert!(v["choices"][0]["message"].get("reasoning_content").is_none(), "应删掉原字段");
    }

    #[test]
    fn clean_text_untouched() {
        let mut v = json!({"choices":[{"message":{"content":"普通回答"}}]});
        assert!(!strip_response(&mut v));
        assert_eq!(v["choices"][0]["message"]["content"], "普通回答");
    }
}
