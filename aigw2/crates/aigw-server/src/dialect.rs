//! 三方协议的互相转换：OpenAI chat.completions ⇄ Anthropic Messages ⇄ Responses。
//!
//! **为什么独立成模块**：这些函数是纯数据变换（`Value` 进 `Value` 出），不碰网络、
//! 不碰状态，却是整个网关最容易出错的地方 —— 客户端一换协议，任何一处字段漏映射都
//! 会表现为「上游 200 但客户端解析失败」，比 4xx 难查得多。它们原本长在 `lib.rs`
//! 那一万六千行里，改一次就得把整个 crate 重编译重测；搬到这里以后可以对着
//! `json!` 字面量直接写用例（见文件末尾 tests）。
//!
//! 口径对齐 `_refs/` 里的成熟实现（Claude Code / Codex / LiteLLM 的行为）：
//! - **tool_use / tool_calls 必须成对出现**：Anthropic 的 `tool_use` 块与 OpenAI 的
//!   `tool_calls[]` 一一对应，`arguments` 是 **JSON 字符串**（不是对象）；
//! - **tool_result → role=tool 消息**：OpenAI 要求它紧跟带 `tool_calls` 的 assistant
//!   消息，所以按块顺序先发，不能攒到最后；
//! - **image 走 data URI**：Anthropic 的 `source.base64` 拼成 OpenAI 的
//!   `image_url.url = data:<media>;base64,<data>`；
//! - **stop_reason 三态**：`max_tokens` / `tool_use` / `end_turn`，没有工具调用时
//!   绝不能报 `tool_use`（客户端会等一个永远不会来的工具结果）。

use serde_json::{json, Value};

// ── 转换过程中反复用到的小工具 ──────────────────────────────
/// Anthropic 内容块数组里的文本（字符串或 [{type:text,text}]）
pub fn anthropic_blocks_text(c: Option<&Value>) -> String {
    let Some(c) = c else { return String::new() };
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = c.as_array() {
        for b in arr {
            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            } else if let Some(s) = b.as_str() {
                out.push_str(s);
            }
        }
    }
    out
}

/// OpenAI 的 tool_calls → Anthropic 的 tool_use 内容块（arguments 是 JSON 字符串）。
pub fn openai_tool_use_blocks(msg: Option<&Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let Some(msg) = msg else { return out };
    if let Some(arr) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for (i, tc) in arr.iter().enumerate() {
            let f = tc.get("function");
            let name = f.and_then(|f| f.get("name")).and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let id = tc
                .get("id")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("toolu_{:016x}_{i}", nanos_hex_int()));
            let args = f
                .and_then(|f| f.get("arguments"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let input = if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"__raw": args}))
            };
            out.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
        }
    }
    // 老式 function_call 单个工具调用
    if let Some(fc) = msg.get("function_call") {
        let name = fc.get("name").and_then(|x| x.as_str()).unwrap_or("");
        if !name.is_empty() {
            let args = fc.get("arguments").and_then(|x| x.as_str()).unwrap_or("");
            let input = if args.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({"__raw": args}))
            };
            out.push(json!({
                "type": "tool_use",
                "id": format!("toolu_{:016x}_fc", nanos_hex_int()),
                "name": name,
                "input": input
            }));
        }
    }
    out
}

pub fn nanos_hex() -> String {
    format!(
        "{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

/// Responses 的 content 可以是字符串，也可以是 [{"type":"input_text","text":…}] 块数组。
pub fn responses_text(c: Option<&Value>) -> String {
    let Some(c) = c else { return String::new() };
    if let Some(s) = c.as_str() {
        return s.to_string();
    }
    let mut out = String::new();
    if let Some(arr) = c.as_array() {
        for b in arr {
            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                out.push_str(t);
            } else if let Some(s) = b.as_str() {
                out.push_str(s);
            }
        }
    }
    out
}

pub fn responses_usage(tin: i64, tout: i64) -> Value {
    json!({
        "input_tokens": tin,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": tout,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": tin + tout,
    })
}

/// 协议转换面（/v1/messages、/v1/responses 的非流式响应）用的 usage 兜底：
/// 上游没给 completion_tokens 时按正文/工具参数字符估算，别让客户端看到恒 0 的 output_tokens。
pub fn extract_usage(v: &Value) -> (i64, i64) {
    let u = v.get("usage");
    (
        u.and_then(|u| u.get("prompt_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        u.and_then(|u| u.get("completion_tokens"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
    )
}

pub fn conv_usage(v: &Value) -> (i64, i64) {
    let (tin, tout) = extract_usage(v);
    if tout > 0 {
        return (tin, tout);
    }
    let text_chars = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.chars().count())
        .unwrap_or(0);
    let args_chars: usize = v
        .pointer("/choices/0/message/tool_calls")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|t| t.pointer("/function/arguments").and_then(|x| x.as_str()))
                .map(|s| s.chars().count())
                .sum()
        })
        .unwrap_or(0);
    let chars = text_chars + args_chars;
    if chars > 0 {
        (tin, (chars / 2).max(1) as i64)
    } else {
        (tin, 0)
    }
}

/// Anthropic Messages 请求 → OpenAI chat.completions 请求形状。
/// - system（字符串或块数组）→ 前缀 system 消息
/// - content 字符串或块数组：text 拼接、image → OpenAI image_url data URI
/// - assistant 的 tool_use 块 → OpenAI tool_calls；user 的 tool_result 块 → role=tool 消息
///   （OpenAI 要求 tool 消息紧跟带 tool_calls 的 assistant 消息，故按块顺序先发）
/// - tools[{name,description,input_schema}] → tools[{type:function,…}]；tool_choice 三态映射
pub fn anthropic_to_openai(v: &Value) -> Result<Value, String> {
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = v.get("system") {
        let text = anthropic_blocks_text(Some(sys));
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) else {
        return Err("messages 字段缺失或不是数组".to_string());
    };
    for m in msgs {
        let role = m
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("user")
            .to_string();
        match m.get("content") {
            Some(Value::String(s)) => messages.push(json!({"role": role, "content": s})),
            Some(Value::Array(blocks)) => {
                if role == "assistant" {
                    let mut text = String::new();
                    let mut tool_calls: Vec<Value> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(x) = b.get("text").and_then(|x| x.as_str()) {
                                    text.push_str(x);
                                }
                            }
                            Some("tool_use") => {
                                let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
                                let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
                                if name.is_empty() {
                                    continue;
                                }
                                let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                                tool_calls.push(json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {"name": name, "arguments": input.to_string()}
                                }));
                            }
                            _ => {}
                        }
                    }
                    if tool_calls.is_empty() {
                        messages.push(json!({"role": "assistant", "content": text}));
                    } else {
                        let content = if text.is_empty() { Value::Null } else { json!(text) };
                        messages.push(json!({
                            "role": "assistant",
                            "content": content,
                            "tool_calls": tool_calls
                        }));
                    }
                } else {
                    // user（或罕见的 system 块数组）：先发 tool 结果，再发剩余文本/图片
                    let mut parts: Vec<Value> = Vec::new();
                    let mut tool_msgs: Vec<Value> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("tool_result") => {
                                let id = b
                                    .get("tool_use_id")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("");
                                let mut body = anthropic_blocks_text(b.get("content"));
                                if body.is_empty() {
                                    body = "(空结果)".to_string();
                                }
                                tool_msgs.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": body
                                }));
                            }
                            Some("text") => {
                                if let Some(x) = b.get("text").and_then(|x| x.as_str()) {
                                    parts.push(json!({"type": "text", "text": x}));
                                }
                            }
                            Some("image") => {
                                let src = b.get("source");
                                let media = src
                                    .and_then(|s| s.get("media_type"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("image/png");
                                let data = src
                                    .and_then(|s| s.get("data"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");
                                parts.push(json!({
                                    "type": "image_url",
                                    "image_url": {"url": format!("data:{media};base64,{data}")}
                                }));
                            }
                            _ => {}
                        }
                    }
                    for tm in tool_msgs {
                        messages.push(tm);
                    }
                    if !parts.is_empty() {
                        let all_text = parts
                            .iter()
                            .all(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"));
                        let content = if all_text {
                            json!(parts
                                .iter()
                                .filter_map(|p| p.get("text"))
                                .filter_map(|t| t.as_str())
                                .collect::<Vec<_>>()
                                .join("\n"))
                        } else {
                            json!(parts)
                        };
                        messages.push(json!({"role": role, "content": content}));
                    }
                }
            }
            _ => messages.push(json!({"role": role, "content": ""})),
        }
    }
    let mut out = json!({"model": model, "messages": messages});
    for k in ["max_tokens", "temperature", "top_p", "stream"] {
        if let Some(x) = v.get(k) {
            out[k] = x.clone();
        }
    }
    if let Some(stops) = v.get("stop_sequences").and_then(|s| s.as_array()) {
        out["stop"] = json!(stops);
    }
    // 工具定义：Anthropic 的 input_schema 就是 JSON Schema，直接搬到 parameters。
    // 服务端工具（web_search_20250305 等只有 type/name，没有 input_schema）OpenAI 形状
    // 表达不了，跳过——否则上游会因为缺 parameters 直接 400。
    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        let mut conv: Vec<Value> = Vec::new();
        for t in tools {
            let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() || t.get("input_schema").is_none() {
                continue;
            }
            let mut f = json!({
                "name": name,
                "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({})),
            });
            if let Some(d) = t.get("description").and_then(|x| x.as_str()) {
                f["description"] = json!(d);
            }
            conv.push(json!({"type": "function", "function": f}));
        }
        if !conv.is_empty() {
            out["tools"] = json!(conv);
        }
    }
    if let Some(tc) = v.get("tool_choice") {
        let kind = tc.get("type").and_then(|x| x.as_str()).unwrap_or("auto");
        out["tool_choice"] = match kind {
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => {
                let name = tc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                json!({"type": "function", "function": {"name": name}})
            }
            _ => json!("auto"),
        };
    }
    Ok(out)
}

pub fn nanos_hex_int() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub fn anthropic_stop_reason(finish: Option<&str>, has_tool: bool) -> &'static str {
    match finish {
        Some("length") | Some("max_tokens") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("stop") => "end_turn",
        _ => {
            if has_tool {
                "tool_use"
            } else {
                "end_turn"
            }
        }
    }
}

/// OpenAI chat.completions 响应 → Anthropic Messages 响应形状。
pub fn openai_to_anthropic(v: &Value, model: &str) -> Value {
    let first = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let msg = first.and_then(|c| c.get("message"));
    let text = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let finish = first
        .and_then(|c| c.get("finish_reason"))
        .and_then(|f| f.as_str());
    let tool_blocks = openai_tool_use_blocks(msg);
    let has_tool = !tool_blocks.is_empty();
    let mut content: Vec<Value> = Vec::new();
    // 推理块必须是 content 的第一项：Anthropic 的语义是「先 thinking 再 text」，
    // 顺序反了客户端的渲染会错乱（也会让 thinking 显示在答案下面）。
    if let Some(r) = msg
        .and_then(|m| m.get("reasoning_content").or_else(|| m.get("reasoning")))
        .and_then(|c| c.as_str())
    {
        if !r.trim().is_empty() {
            content.push(json!({"type": "thinking", "thinking": r}));
        }
    }
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    content.extend(tool_blocks);
    let stop_reason = anthropic_stop_reason(finish, has_tool);
    let (input_tokens, output_tokens) = conv_usage(v);
    json!({
        "id": format!("msg_{}", v.get("id").and_then(|i| i.as_str()).unwrap_or("unknown")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens},
    })
}

/// Responses 请求 → OpenAI chat 请求（input 支持字符串、消息数组或裸字符串数组）。
pub fn responses_to_openai(v: &Value) -> Result<Value, String> {
    let mut msgs: Vec<Value> = Vec::new();
    if let Some(ins) = v.get("instructions").and_then(|x| x.as_str()) {
        if !ins.trim().is_empty() {
            msgs.push(json!({"role": "system", "content": ins}));
        }
    }
    match v.get("input") {
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Err("input 不能为空".to_string());
            }
            msgs.push(json!({"role": "user", "content": s}));
        }
        Some(Value::Array(items)) => {
            for it in items {
                if let Some(s) = it.as_str() {
                    if !s.trim().is_empty() {
                        msgs.push(json!({"role": "user", "content": s}));
                    }
                    continue;
                }
                // ★ 工具调用链必须转换，否则 Codex CLI 的多轮工具对话是**断的**：
                //   Responses 把工具往返放在 input 数组里（function_call /
                //   function_call_output 两种 item），旧实现把非 user/assistant/system
                //   的 item 一律 continue 丢弃 —— 上游看到的是一串「用户说了句话」
                //   却没有任何工具调用与结果，模型只能重新猜一遍或直接答错。
                match it.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "function_call" => {
                        let call_id = it
                            .get("call_id")
                            .or_else(|| it.get("id"))
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = it
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if name.is_empty() {
                            continue;
                        }
                        // arguments 在 Responses 里是**字符串**；个别客户端给对象，
                        // 统一成 OpenAI 要求的字符串形态。
                        let args = match it.get("arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v @ Value::Object(_)) => v.to_string(),
                            _ => "{}".to_string(),
                        };
                        msgs.push(json!({
                            "role": "assistant",
                            "content": Value::Null,
                            "tool_calls": [{
                                "id": call_id,
                                "type": "function",
                                "function": {"name": name, "arguments": args},
                            }],
                        }));
                        continue;
                    }
                    "function_call_output" => {
                        let call_id = it
                            .get("call_id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        if call_id.is_empty() {
                            continue;
                        }
                        let text = match it.get("output") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        msgs.push(json!({
                            "role": "tool",
                            "tool_call_id": call_id,
                            "content": text,
                        }));
                        continue;
                    }
                    // reasoning / item_reference 等纯客户端状态，丢弃无害
                    "reasoning" | "item_reference" => continue,
                    _ => {}
                }
                let raw_role = it.get("role").and_then(|x| x.as_str()).unwrap_or("user");
                let role = if raw_role == "developer" { "system" } else { raw_role };
                if role != "user" && role != "assistant" && role != "system" {
                    continue;
                }
                let text = responses_text(it.get("content"));
                if text.trim().is_empty() {
                    continue;
                }
                msgs.push(json!({"role": role, "content": text}));
            }
        }
        _ => return Err("缺少 input 字段（字符串或消息数组）".to_string()),
    }
    if msgs.is_empty() || !msgs.iter().any(|m| m.get("role").and_then(|r| r.as_str()) != Some("system")) {
        return Err("input 里没有可发送的用户/助手消息".to_string());
    }
    let mut oa = json!({
        "model": v.get("model").cloned().unwrap_or_else(|| json!("unknown")),
        "messages": msgs,
    });
    let val = oa.as_object_mut().expect("object");
    for (from, to) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
    ] {
        if let Some(x) = v.get(from) {
            val.insert(to.to_string(), x.clone());
        }
    }
    if let Some(r) = v.get("reasoning").and_then(|x| x.get("effort")) {
        let _ = r; // 上游多为 OpenAI 兼容端点，effort 无对应字段，忽略
    }
    // ★ 工具定义也要换形状：Responses 是**扁平**的
    //   {type:"function", name, description, parameters, strict}
    //   OpenAI chat 是**嵌套**的
    //   {type:"function", function:{name, description, parameters}}
    //   旧实现完全不转 tools，于是 Codex CLI 发过来的工具在上游眼里不存在 ——
    //   模型只能凭文字瞎猜，多轮工具流程根本跑不起来。
    if let Some(Value::Array(tools)) = v.get("tools") {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let ty = t.get("type").and_then(|x| x.as_str()).unwrap_or("function");
                // 非 function 类（web_search / file_search / mcp 等托管工具）没有
                // OpenAI chat 等价物，直接跳过而不是原样透传（透传会让上游 400）。
                if ty != "function" {
                    return None;
                }
                // 已经是嵌套形状的（个别客户端混着发）原样保留
                if t.get("function").is_some() {
                    return Some(t.clone());
                }
                let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("");
                if name.is_empty() {
                    return None;
                }
                let mut f = serde_json::Map::new();
                f.insert("name".into(), json!(name));
                if let Some(d) = t.get("description") {
                    f.insert("description".into(), d.clone());
                }
                f.insert(
                    "parameters".into(),
                    t.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                );
                Some(json!({"type": "function", "function": Value::Object(f)}))
            })
            .collect();
        if !converted.is_empty() {
            val.insert("tools".into(), Value::Array(converted));
            // tool_choice：Responses 的 {"type":"function","name":"x"} → chat 的
            // {"type":"function","function":{"name":"x"}}；"auto"/"none"/"required" 直传。
            match v.get("tool_choice") {
                Some(Value::String(s)) => {
                    val.insert("tool_choice".into(), json!(s));
                }
                Some(tc @ Value::Object(_)) => {
                    let name = tc.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    if !name.is_empty() {
                        val.insert(
                            "tool_choice".into(),
                            json!({"type": "function", "function": {"name": name}}),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok(oa)
}

pub fn responses_output(text: &str, msg_id: &str) -> Value {
    json!({
        "id": msg_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": []}],
    })
}

/// OpenAI chat.completion → Responses 对象。
pub fn openai_to_responses(v: &Value, model: &str) -> Value {
    let first = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first());
    let text = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let reasoning = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let (tin, tout) = conv_usage(v);
    let msg_id = format!("msg_{}", nanos_hex());
    let mut output: Vec<Value> = Vec::new();
    if !reasoning.trim().is_empty() {
        output.push(json!({
            "id": format!("rs_{}", nanos_hex()),
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": reasoning}],
        }));
    }
    // ★ 反向：上游返回的 tool_calls 必须变成 Responses 的 function_call item，
    //   否则 Codex CLI 收到的响应里只有一段空文本，工具调用被静默吞掉 ——
    //   客户端永远等不到要执行的动作，整个 agent 循环卡死。
    let tool_calls = first
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array());
    if text.trim().is_empty() && tool_calls.map(|t| !t.is_empty()).unwrap_or(false) {
        // 纯工具调用：不产生空 message item（客户端会把空 assistant 消息当答案）
    } else {
        output.push(responses_output(text, &msg_id));
    }
    if let Some(calls) = tool_calls {
        for c in calls {
            let call_id = c
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{}", nanos_hex()));
            let f = c.get("function");
            let name = f
                .and_then(|x| x.get("name"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let args = match f.and_then(|x| x.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => "{}".to_string(),
            };
            output.push(json!({
                "id": format!("fc_{}", nanos_hex()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": name,
                "arguments": args,
            }));
        }
    }
    json!({
        "id": format!("resp_{}", nanos_hex()),
        "object": "response",
        "created_at": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "status": "completed",
        "error": Value::Null,
        "incomplete_details": Value::Null,
        "instructions": Value::Null,
        "model": model,
        "output": output,
        "output_text": text,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
        "usage": responses_usage(tin, tout),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_request_becomes_openai_request() {
        let src = json!({
            "model": "claude-x",
            "system": "你是助手",
            "max_tokens": 256,
            "messages": [
                {"role": "user", "content": "你好"}
            ],
            "tools": [{"name": "get_weather", "description": "查天气",
                       "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}}],
            "tool_choice": {"type": "auto"}
        });
        let out = anthropic_to_openai(&src).expect("应能转换");
        // system 提成前缀消息，且必须排在最前
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "你是助手");
        assert_eq!(out["messages"][1]["role"], "user");
        // 工具定义换成 OpenAI 形状：input_schema → function.parameters
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
        assert!(out["tools"][0]["function"]["parameters"].is_object());
        // max_tokens 在两个协议里同名同义，不能丢
        assert_eq!(out["max_tokens"], 256);
    }

    #[test]
    fn anthropic_tool_use_and_tool_result_round_trip() {
        let src = json!({
            "model": "claude-x",
            "messages": [
                {"role": "user", "content": "北京天气"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "我查一下"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                     "input": {"city": "北京"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "晴 25 度"}
                ]}
            ]
        });
        let out = anthropic_to_openai(&src).expect("应能转换");
        let msgs = out["messages"].as_array().expect("messages 必须是数组");
        // assistant 消息带 tool_calls，arguments 必须是 JSON **字符串**
        let assistant = msgs.iter().find(|m| m["role"] == "assistant").expect("缺少 assistant 消息");
        let calls = assistant["tool_calls"].as_array().expect("缺少 tool_calls");
        assert_eq!(calls[0]["id"], "toolu_1");
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        let args = calls[0]["function"]["arguments"].as_str().expect("arguments 必须是字符串");
        let parsed: Value = serde_json::from_str(args).expect("arguments 必须是合法 JSON");
        assert_eq!(parsed["city"], "北京");
        // tool_result 变成 role=tool 且带 tool_call_id
        let tool_msg = msgs.iter().find(|m| m["role"] == "tool").expect("缺少 role=tool 消息");
        assert_eq!(tool_msg["tool_call_id"], "toolu_1");
        assert_eq!(tool_msg["content"], "晴 25 度");
    }

    #[test]
    fn openai_response_becomes_anthropic_message() {
        let src = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "你好呀"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let out = openai_to_anthropic(&src, "claude-x");
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "你好呀");
        // 没有工具调用时绝不能报 tool_use——客户端会等一个永远不来的工具结果
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["usage"]["input_tokens"], 10);
        assert_eq!(out["usage"]["output_tokens"], 5);
    }

    #[test]
    fn openai_tool_calls_become_anthropic_tool_use_blocks() {
        let src = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"北京\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let out = openai_to_anthropic(&src, "claude-x");
        let blocks = out["content"].as_array().expect("content 必须是数组");
        let tu = blocks.iter().find(|b| b["type"] == "tool_use").expect("缺少 tool_use 块");
        assert_eq!(tu["id"], "call_1");
        assert_eq!(tu["name"], "get_weather");
        // Anthropic 的 input 是**对象**，不是 JSON 字符串
        assert_eq!(tu["input"]["city"], "北京");
        assert_eq!(out["stop_reason"], "tool_use");
    }

    #[test]
    fn nonstream_reasoning_becomes_thinking_block_and_reasoning_item() {
        // 非流式路径与流式必须同口径：上游 message.reasoning_content 不能被丢。
        let src = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "答案",
                    "reasoning_content": "先推理"
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}
        });
        // Anthropic：thinking 必须是 content 的第一项
        let anth = openai_to_anthropic(&src, "claude-x");
        let blocks = anth["content"].as_array().expect("content 必须是数组");
        assert_eq!(blocks[0]["type"], "thinking", "thinking 必须排第一：{anth}");
        assert_eq!(blocks[0]["thinking"], "先推理");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "答案");

        // Responses：reasoning item 必须在 message item 之前
        let resp = openai_to_responses(&src, "gpt-x");
        let items = resp["output"].as_array().expect("output 必须是数组");
        assert_eq!(items[0]["type"], "reasoning", "reasoning 必须排第一：{resp}");
        assert_eq!(items[0]["summary"][0]["text"], "先推理");
        assert_eq!(items[1]["type"], "message");
    }

    #[test]
    fn nonstream_without_reasoning_has_no_thinking_block() {
        // 没有 reasoning_content 时不能凭空造 thinking / reasoning
        let src = json!({"choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}]});
        let anth = openai_to_anthropic(&src, "m");
        let blocks = anth["content"].as_array().unwrap();
        assert!(blocks.iter().all(|b| b["type"] != "thinking"), "{anth}");
        let resp = openai_to_responses(&src, "m");
        let items = resp["output"].as_array().unwrap();
        assert!(items.iter().all(|i| i["type"] != "reasoning"), "{resp}");
    }

    #[test]
    fn anthropic_stop_reason_maps_all_three_states() {
        assert_eq!(anthropic_stop_reason(Some("length"), false), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("max_tokens"), false), "max_tokens");
        assert_eq!(anthropic_stop_reason(Some("tool_calls"), true), "tool_use");
        assert_eq!(anthropic_stop_reason(Some("stop"), true), "end_turn");
        // finish_reason 缺失时有工具就 tool_use，没有就 end_turn
        assert_eq!(anthropic_stop_reason(None, true), "tool_use");
        assert_eq!(anthropic_stop_reason(None, false), "end_turn");
    }

    #[test]
    fn responses_request_becomes_openai_request() {
        let src = json!({
            "model": "gpt-x",
            "instructions": "你是助手",
            "input": [
                {"type": "message", "role": "user",
                 "content": [{"type": "input_text", "text": "你好"}]}
            ],
            "tools": [{"type": "function", "name": "get_weather",
                       "parameters": {"type": "object"}, "description": "查天气"}]
        });
        let out = responses_to_openai(&src).expect("应能转换");
        // instructions 提成 system
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][0]["content"], "你是助手");
        assert_eq!(out["messages"][1]["role"], "user");
        assert_eq!(out["messages"][1]["content"], "你好");
        // Responses 是扁平 function 定义，OpenAI 要包一层 function
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn openai_response_becomes_responses_object() {
        let src = json!({
            "id": "chatcmpl-1",
            "choices": [{
                "message": {"role": "assistant", "content": "你好呀"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let out = openai_to_responses(&src, "gpt-x");
        assert_eq!(out["object"], "response");
        let output = out["output"].as_array().expect("output 必须是数组");
        assert_eq!(output[0]["type"], "message");
        assert_eq!(output[0]["content"][0]["type"], "output_text");
        assert_eq!(output[0]["content"][0]["text"], "你好呀");
        assert_eq!(out["usage"]["input_tokens"], 10);
        assert_eq!(out["usage"]["output_tokens"], 5);
        assert_eq!(out["usage"]["total_tokens"], 15);
    }

    #[test]
    fn openai_tool_calls_become_function_call_items() {
        let src = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"北京\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let out = openai_to_responses(&src, "gpt-x");
        let items = out["output"].as_array().expect("output 必须是数组");
        let fc = items.iter().find(|i| i["type"] == "function_call").expect("缺少 function_call");
        assert_eq!(fc["call_id"], "call_1");
        assert_eq!(fc["name"], "get_weather");
        assert_eq!(fc["arguments"], "{\"city\":\"北京\"}");
    }

    #[test]
    fn conversion_never_panics_on_minimal_input() {
        // 上游可能返回一个几乎空的壳子；这里只要求不 panic、返回可用的默认结构
        let empty = json!({});
        let r = anthropic_to_openai(&empty);
        // 缺 model/messages 时应当返回 Err 而不是 panic
        assert!(r.is_err() || r.unwrap()["messages"].is_array());
        let _ = responses_to_openai(&empty);
        let _ = openai_to_anthropic(&empty, "m");
        let _ = openai_to_responses(&empty, "m");
    }
}
