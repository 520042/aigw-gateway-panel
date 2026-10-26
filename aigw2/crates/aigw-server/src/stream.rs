//! OpenAI SSE → Anthropic / Responses 的流式事件转换。
//!
//! 两套状态机（`AnthState` / `RespState`）各自维护「当前开着哪个 content block /
//! output item」，把上游 OpenAI 的增量帧翻译成目标协议的事件序列。
//!
//! **为什么独立成模块**：它们是有状态的解析器，边界条件多且全是协议细节 ——
//! 块的开关必须配对、工具调用要跨帧累积、推理内容要先于正文、收尾必须发 stop。
//! 这些原本长在 `lib.rs`（一万六千行）里，任何一处改动都要重编译整个 crate，
//! 也没法脱离 HTTP 单独驱动。搬出来之后可以喂一串 SSE 事件直接断言产出。
//!
//! 口径对齐 `_refs/` 里的成熟实现：
//! - Anthropic：`thinking_delta` 独立成块且排在正文之前
//!   （autumnsentiment-Trae2api-cn/src/anthropic_api.rs 的 Python 原版）；
//! - Responses：推理是独立 item，正文 `output_index` 顺延；纯工具调用时**不造**
//!   空 message item（否则客户端把空 assistant 消息当最终答案，直接结束回合）。

use axum::body::Bytes;
use serde_json::{json, Value};

use crate::{anthropic_stop_reason, nanos_hex, nanos_hex_int, responses_output, responses_usage};

/// OpenAI SSE → Anthropic SSE 事件流：message_start → content_block_start →
/// content_block_delta(text_delta) → content_block_stop → message_delta → message_stop。
pub fn anthropic_stream(
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    model: String,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    use futures_util::StreamExt;
    let msg_id = format!("msg_{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let state = AnthState {
        inner,
        parser: aigw_relay::SseParser::new(),
        model,
        msg_id,
        started: false,
        blocks_opened: 0,
        text_index: 0,
        text_open: false,
        thinking_index: 0,
        thinking_open: false,
        tool_open: None,
        tool_meta: Vec::new(),
        closed: false,
        stop_reason: "end_turn".to_string(),
        usage_seen: false,
        delta_sent: false,
        out_text: String::new(),
        pending: std::collections::VecDeque::new(),
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(b) = s.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(b), s));
            }
            if s.closed {
                return None;
            }
            match s.inner.next().await {
                Some(Ok(bytes)) => {
                    let evs = s.parser.feed(&bytes);
                    s.consume(evs);
                }
                Some(Err(_e)) => {
                    s.finish();
                }
                None => {
                    s.finish();
                }
            }
        }
    }))
}

/// /v1/messages 流式转换状态机。
/// 文本块与工具块按到达顺序依次开启：tool_use 用 content_block_start +
/// input_json_delta 表达，文本用 text_delta，块之间必须有 content_block_stop。
struct AnthState {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    parser: aigw_relay::SseParser,
    model: String,
    msg_id: String,
    /// 是否已发 message_start
    started: bool,
    /// 已开启的内容块数量（= 下一个块的 index）
    blocks_opened: usize,
    /// 当前打开文本块的 index
    text_index: usize,
    text_open: bool,
    /// 当前打开的 **思考块**（Anthropic 的 `type: "thinking"`）的 index。
    ///
    /// 为什么需要：上游 OpenAI 方言把模型的推理过程放在 `delta.reasoning_content`
    /// 里（DeepSeek / GLM / Kimi / MiniMax 都是这个字段）。旧实现在 Anthropic 侧只取
    /// `delta.content`，reasoning 被**整段丢掉** —— Claude Code 这类客户端在 thinking
    /// 模式下看到的是「模型什么也没想就直接答了」。Trae2api 的做法是对
    /// `reasoning_content|reasoning|thinking` 起一个独立块发 `thinking_delta`，这里对齐。
    thinking_index: usize,
    thinking_open: bool,
    /// 当前打开的工具块：(block index, tool id)
    tool_open: Option<(usize, String)>,
    /// 上游 tool_calls[i] 的 (id, name) 记忆（id/name 只在首帧出现）
    tool_meta: Vec<(String, String)>,
    closed: bool,
    stop_reason: String,
    /// 是否已收到上游 usage 帧（让 message_delta 携带准确 output_tokens）
    usage_seen: bool,
    /// 是否已发 message_delta（幂等）
    delta_sent: bool,
    /// 已下发的文本累计（上游无 usage 帧时用于估算 output_tokens）
    out_text: String,
    pending: std::collections::VecDeque<Bytes>,
}

impl AnthState {
    fn event(&mut self, name: &str, data: &Value) {
        let s = format!("event: {name}\ndata: {data}\n\n");
        self.pending.push_back(Bytes::from(s));
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        self.event(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": self.parser.prompt_tokens, "output_tokens": 0}
                }
            }),
        );
    }

    fn close_text_block(&mut self) {
        if self.text_open {
            let idx = self.text_index;
            self.event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": idx}),
            );
            self.text_open = false;
        }
    }

    fn close_tool_block(&mut self) {
        if let Some((idx, _)) = self.tool_open.take() {
            self.event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": idx}),
            );
        }
    }

    fn close_blocks(&mut self) {
        self.close_thinking_block();
        self.close_text_block();
        self.close_tool_block();
    }

    /// 打开思考块。Anthropic 要求 thinking 块**排在正文之前**（它模拟的是
    /// 「先想再说」），所以这里不做任何块间顺序调整，只保证同一时刻只有一个块打开。
    fn open_thinking_block(&mut self) {
        if self.thinking_open {
            return;
        }
        self.close_text_block();
        self.close_tool_block();
        let idx = self.blocks_opened;
        self.blocks_opened += 1;
        self.thinking_index = idx;
        self.thinking_open = true;
        self.event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": {"type": "thinking", "thinking": ""}
            }),
        );
    }

    fn close_thinking_block(&mut self) {
        if self.thinking_open {
            let idx = self.thinking_index;
            self.event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": idx}),
            );
            self.thinking_open = false;
        }
    }

    fn open_text_block(&mut self) {
        if self.text_open {
            return;
        }
        self.close_thinking_block();
        self.close_tool_block();
        let idx = self.blocks_opened;
        self.blocks_opened += 1;
        self.text_index = idx;
        self.text_open = true;
        self.event(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": idx,
                "content_block": {"type": "text", "text": ""}
            }),
        );
    }

    /// 下发一段思考内容（上游 `delta.reasoning_content`）。
    fn thinking_delta(&mut self, text: &str) {
        self.ensure_started();
        self.open_thinking_block();
        let idx = self.thinking_index;
        self.event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": {"type": "thinking_delta", "thinking": text}
            }),
        );
    }

    fn delta(&mut self, text: &str) {
        self.ensure_started();
        self.open_text_block();
        self.out_text.push_str(text);
        let idx = self.text_index;
        self.event(
            "content_block_delta",
            &json!({
                "type": "content_block_delta",
                "index": idx,
                "delta": {"type": "text_delta", "text": text}
            }),
        );
    }

    /// 工具调用增量：第一个分片带 id/name（开块），后续分片只带 arguments。
    fn tool_delta(&mut self, id: &str, name: &str, args: &str) {
        self.ensure_started();
        let reuse = matches!(&self.tool_open, Some((_, cur)) if cur == id);
        if !reuse {
            self.close_text_block();
            self.close_tool_block();
            let idx = self.blocks_opened;
            self.blocks_opened += 1;
            self.tool_open = Some((idx, id.to_string()));
            self.event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": idx,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {}
                    }
                }),
            );
        }
        if !args.is_empty() {
            if let Some((idx, _)) = &self.tool_open {
                let idx = *idx;
                self.event(
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": idx,
                        "delta": {"type": "input_json_delta", "partial_json": args}
                    }),
                );
            }
        }
        self.stop_reason = "tool_use".to_string();
    }

    /// message_delta（幂等）：携带当前累计 output_tokens。
    fn send_delta(&mut self) {
        if self.delta_sent {
            return;
        }
        self.delta_sent = true;
        let sr: String = if self.stop_reason.is_empty() {
            "end_turn".to_string()
        } else {
            self.stop_reason.clone()
        };
        // 上游流式常不带 usage 帧（如 apk-doubao）：有真实计数用它，否则按字符估算，
        // 避免 output_tokens 恒为 0 让客户端计费/展示失真。
        let out_tokens = if self.parser.completion_tokens > 0 {
            self.parser.completion_tokens
        } else {
            (self.out_text.chars().count() as i64 / 2).max(1)
        };
        self.event(
            "message_delta",
            &json!({
                "type": "message_delta",
                "delta": {"stop_reason": sr},
                "usage": {"output_tokens": out_tokens}
            }),
        );
    }

    /// 收尾：所有块 content_block_stop → message_delta → message_stop。
    fn finish(&mut self) {
        if self.closed {
            return;
        }
        if !self.started {
            self.ensure_started();
        }
        self.close_blocks();
        self.send_delta();
        self.event("message_stop", &json!({"type": "message_stop"}));
        self.closed = true;
    }

    fn consume(&mut self, evs: Vec<aigw_relay::SseEvent>) {
        use aigw_relay::SseEvent;
        for ev in evs {
            match ev {
                SseEvent::Data(v) => {
                    if v.get("error").is_some() {
                        let msg = v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("上游返回流式错误");
                        self.event(
                            "error",
                            &json!({"type": "error", "error": {"type": "api_error", "message": msg}}),
                        );
                        self.finish();
                        continue;
                    }
                    if v.get("usage").is_some() {
                        self.usage_seen = true;
                    }
                    let first = v
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first());
                    if let Some(f) = first {
                        // ★ 思考内容必须单独成块：上游把推理放在 reasoning_content
                        //   （DeepSeek/GLM/Kimi/MiniMax 通用写法），旧实现只取 content，
                        //   于是 thinking 模式下的推理过程整段消失。
                        //   顺序按上游到达顺序走，正文来了自然关掉思考块。
                        let reasoning = f
                            .get("delta")
                            .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
                            .and_then(|c| c.as_str());
                        if let Some(r) = reasoning {
                            if !r.is_empty() {
                                self.thinking_delta(r);
                            }
                        }
                        if let Some(text) = f
                            .get("delta")
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !text.is_empty() {
                                self.delta(text);
                            }
                        }
                        if let Some(tcs) = f
                            .get("delta")
                            .and_then(|d| d.get("tool_calls"))
                            .and_then(|t| t.as_array())
                        {
                            for tc in tcs {
                                let up = tc
                                    .get("index")
                                    .and_then(|i| i.as_u64())
                                    .unwrap_or(0) as usize;
                                while self.tool_meta.len() <= up {
                                    self.tool_meta.push((String::new(), String::new()));
                                }
                                if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                                    if !id.is_empty() {
                                        self.tool_meta[up].0 = id.to_string();
                                    }
                                }
                                if let Some(n) = tc
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|x| x.as_str())
                                {
                                    if !n.is_empty() {
                                        self.tool_meta[up].1 = n.to_string();
                                    }
                                }
                                let args = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("");
                                let (id, name) = self.tool_meta[up].clone();
                                let id = if id.is_empty() {
                                    format!("toolu_{:016x}_{up}", nanos_hex_int())
                                } else {
                                    id
                                };
                                self.tool_delta(&id, &name, args);
                            }
                        }
                        if let Some(fr) = f.get("finish_reason").and_then(|x| x.as_str()) {
                            let has_tool = !self.tool_meta.is_empty();
                            self.stop_reason = anthropic_stop_reason(Some(fr), has_tool).to_string();
                        }
                    }
                }
                SseEvent::Done => self.finish(),
                SseEvent::Other => {}
            }
        }
        // finish_reason 与 usage 都到齐后提前发 message_delta（不必等流结束），
        // 这样 output_tokens 携带的是上游 usage 帧的真实值，而非 0。
        // 工具块打开期间不能提前收尾（否则 input_json_delta 会被截断）。
        if self.parser.finish_reason.is_some()
            && self.usage_seen
            && self.tool_open.is_none()
            && !self.closed
            && self.delta_sent == false
        {
            self.close_text_block();
            self.send_delta();
        }
    }
}


/// Responses 流式转换：OpenAI chunk → response.output_text.delta 等事件序列。
struct RespState {
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    parser: aigw_relay::SseParser,
    model: String,
    resp_id: String,
    item_id: String,
    created_at: u64,
    seq: u64,
    started: bool,
    item_open: bool,
    closed: bool,
    out_text: String,
    /// 上游流里累积的工具调用（Responses 要的是 function_call item）。
    /// 旧实现完全忽略 delta.tool_calls —— Codex CLI 流式跑工具时收到的只有空文本，
    /// agent 循环直接卡死（它永远等不到要执行的动作）。
    tool_calls: Vec<StreamToolCall>,
    /// 已累积的推理文本（上游 `delta.reasoning_content`）。
    ///
    /// Responses 协议里推理是独立 item：`{"type":"reasoning","summary":[...]}`，
    /// 与正文 message item 分开输出。旧实现直接忽略这段增量，客户端（Codex / DSH
    /// 这类会展示思考过程的）完全看不到推理。这里先攒起来，在 finish 时补成一个
    /// reasoning item —— 与 function_call item 的处理方式一致，避免增量事件
    /// 与 item 生命周期交错出错。
    reasoning: String,
    /// reasoning item 是否已发出（幂等：正文前冲一次，finish 兜底时不再重复）
    reasoning_flushed: bool,
    pending: std::collections::VecDeque<Bytes>,
}

/// 流式工具调用累积器。
#[derive(Default, Clone)]
struct StreamToolCall {
    call_id: String,
    name: String,
    args: String,
}

impl RespState {
    fn event(&mut self, name: &str, mut data: Value) {
        if let Some(o) = data.as_object_mut() {
            o.insert("type".to_string(), json!(name));
            o.insert("sequence_number".to_string(), json!(self.seq));
        }
        self.seq += 1;
        let s = format!("event: {name}\ndata: {data}\n\n");
        self.pending.push_back(Bytes::from(s));
    }

    fn ensure_started(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let resp = json!({
            "id": self.resp_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "in_progress",
            "model": self.model,
            "output": [],
        });
        self.event("response.created", json!({"response": resp}));
    }

    fn ensure_item(&mut self) {
        if self.item_open {
            return;
        }
        self.ensure_started();
        self.item_open = true;
        let item = json!({
            "id": self.item_id,
            "type": "message",
            "status": "in_progress",
            "role": "assistant",
            "content": [],
        });
        self.event(
            "response.output_item.added",
            json!({"output_index": 0, "item": item}),
        );
        self.event(
            "response.content_part.added",
            json!({
                "item_id": self.item_id,
                "output_index": 0,
                "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []},
            }),
        );
    }

    /// 把已攒下的推理补成一个 reasoning item（幂等，只出一次）。
    ///
    /// 为什么不在 finish 里统一补：正文 item 一旦以 output_index 0 开出去，再往前面
    /// 插 reasoning 就会让客户端的 index 顺序错乱。所以必须在**第一条正文增量之前**
    /// 冲出去；纯推理无正文的情况由 finish 兜底（同样走这个幂等方法）。
    fn flush_reasoning(&mut self) {
        if self.reasoning.trim().is_empty() || self.reasoning_flushed {
            return;
        }
        self.ensure_started();
        self.reasoning_flushed = true;
        let rid = format!("rs_{}", nanos_hex());
        self.event(
            "response.output_item.added",
            json!({"output_index": 0, "item": {
                "id": rid,
                "type": "reasoning",
                "status": "in_progress",
                "summary": [],
            }}),
        );
        self.event(
            "response.output_item.done",
            json!({"output_index": 0, "item": {
                "id": rid,
                "type": "reasoning",
                "status": "completed",
                "summary": [{"type": "summary_text", "text": self.reasoning.clone()}],
            }}),
        );
    }

    fn delta(&mut self, text: &str) {
        self.flush_reasoning();
        self.ensure_item();
        self.out_text.push_str(text);
        self.event(
            "response.output_text.delta",
            json!({
                "item_id": self.item_id,
                "output_index": 0,
                "content_index": 0,
                "delta": text,
            }),
        );
    }

    /// 累积上游 delta.tool_calls：按 index 归并，id/name 只在首帧出现，
    /// arguments 逐帧拼接（OpenAI 流式工具调用的标准形状）。
    fn push_tool_calls(&mut self, calls: &[Value]) {
        for c in calls {
            let idx = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            while self.tool_calls.len() <= idx {
                self.tool_calls.push(StreamToolCall::default());
            }
            let slot = &mut self.tool_calls[idx];
            if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                if !id.is_empty() {
                    slot.call_id = id.to_string();
                }
            }
            if let Some(f) = c.get("function") {
                if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                    if !n.is_empty() {
                        slot.name = n.to_string();
                    }
                }
                if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                    slot.args.push_str(a);
                }
            }
        }
    }

    fn finish(&mut self) {
        if self.closed {
            return;
        }
        let has_tools = self.tool_calls.iter().any(|c| !c.name.is_empty());
        // ★ 推理先出：Responses 里推理是独立 item（type=reasoning），排在正文之前。
        //   上游正文没来时也要先 ensure_started，否则响应流里第一个事件是 output_item
        //   而没有 response.created，客户端会当作协议错误。
        self.flush_reasoning();
        // 纯工具调用（没有正文）时不能凭空造一个空 message item：
        // 客户端会把空 assistant 消息当成最终答案，直接结束回合。
        if !has_tools || !self.out_text.is_empty() {
            self.ensure_item();
        }
        let text = self.out_text.clone();
        if self.item_open {
            let base_idx = usize::from(!self.reasoning.trim().is_empty());
            self.event(
                "response.output_text.done",
                json!({
                    "item_id": self.item_id,
                    "output_index": base_idx,
                    "content_index": 0,
                    "text": text,
                }),
            );
            self.event(
                "response.content_part.done",
                json!({
                    "item_id": self.item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": text, "annotations": []},
                }),
            );
            self.event(
                "response.output_item.done",
                json!({"output_index": 0, "item": responses_output(&text, &self.item_id)}),
            );
        }
        // 工具调用补成 function_call item（output_index 顺延到正文之后）
        let mut output: Vec<Value> = Vec::new();
        if self.item_open {
            output.push(responses_output(&text, &self.item_id));
        }
        for c in self.tool_calls.clone() {
            if c.name.is_empty() {
                continue;
            }
            let idx = output.len();
            let call_id = if c.call_id.is_empty() {
                format!("call_{}", nanos_hex())
            } else {
                c.call_id.clone()
            };
            let item = json!({
                "id": format!("fc_{}", nanos_hex()),
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "name": c.name,
                "arguments": if c.args.trim().is_empty() { "{}".to_string() } else { c.args.clone() },
            });
            self.event(
                "response.output_item.added",
                json!({"output_index": idx, "item": {
                    "id": item["id"],
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": item["call_id"],
                    "name": item["name"],
                    "arguments": "",
                }}),
            );
            self.event(
                "response.output_item.done",
                json!({"output_index": idx, "item": item}),
            );
            output.push(item);
        }
        let tin = self.parser.prompt_tokens;
        let tout = if self.parser.completion_tokens > 0 {
            self.parser.completion_tokens
        } else {
            (self.out_text.chars().count() as i64 / 2).max(1)
        };
        let resp = json!({
            "id": self.resp_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "completed",
            "model": self.model,
            "output": output,
            "output_text": text,
            "usage": responses_usage(tin, tout),
        });
        self.event("response.completed", json!({"response": resp}));
        self.closed = true;
    }

    fn consume(&mut self, evs: Vec<aigw_relay::SseEvent>) {
        use aigw_relay::SseEvent;
        for ev in evs {
            match ev {
                SseEvent::Data(v) => {
                    if let Some(err) = v.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("上游返回流式错误");
                        self.ensure_started();
                        self.event(
                            "error",
                            json!({"message": msg, "code": "upstream_error"}),
                        );
                        self.finish();
                        continue;
                    }
                    let first = v
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first());
                    if let Some(f) = first {
                        let delta = f.get("delta");
                        // 推理增量先攒着，finish 时补成独立的 reasoning item（见字段注释）
                        if let Some(r) = delta
                            .and_then(|d| {
                                d.get("reasoning_content").or_else(|| d.get("reasoning"))
                            })
                            .and_then(|c| c.as_str())
                        {
                            if !r.is_empty() {
                                self.reasoning.push_str(r);
                            }
                        }
                        if let Some(text) = delta
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str())
                        {
                            if !text.is_empty() {
                                self.delta(text);
                            }
                        }
                        if let Some(calls) =
                            delta.and_then(|d| d.get("tool_calls")).and_then(|t| t.as_array())
                        {
                            if !calls.is_empty() {
                                // 先发 response.created，客户端才知道响应已开始
                                self.ensure_started();
                                self.push_tool_calls(calls);
                            }
                        }
                    }
                }
                SseEvent::Done => self.finish(),
                SseEvent::Other => {}
            }
        }
    }
}

pub fn responses_stream(
    inner: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    model: String,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    use futures_util::StreamExt;
    let state = RespState {
        inner,
        parser: aigw_relay::SseParser::new(),
        model,
        resp_id: format!("resp_{}", nanos_hex()),
        item_id: format!("msg_{}", nanos_hex()),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        seq: 0,
        started: false,
        item_open: false,
        closed: false,
        out_text: String::new(),
        tool_calls: Vec::new(),
        reasoning: String::new(),
        reasoning_flushed: false,
        pending: std::collections::VecDeque::new(),
    };
    Box::pin(futures_util::stream::unfold(state, |mut s| async move {
        loop {
            if let Some(b) = s.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(b), s));
            }
            if s.closed {
                return None;
            }
            match s.inner.next().await {
                Some(Ok(bytes)) => {
                    let evs = s.parser.feed(&bytes);
                    s.consume(evs);
                }
                Some(Err(_e)) => s.finish(),
                None => s.finish(),
            }
        }
    }))
}
