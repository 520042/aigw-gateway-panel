//! 判断「这一轮是不是被腰斩了」（长度截断自动续写的判据）。
//!
//! 从参考项目 Agent2API 的 `core/upstream/continuation.rs` 移植（它又来自社区件
//! Trae2api-cn 的 `src/continuation.py`）。
//!
//! ── 它解决什么 ──────────────────────────────────────────
//! 上游的 turn 会**中途收尾**：输出预算用尽（`finish_reason: length` 一族）、连接被
//! 掐断、或模型把整轮都花在思考通道里、正文只吐了一两个词。客户端拿到的是**半截
//! 答案**，而它没法区分「模型就说这么多」与「被腰斩」—— 前端显示一段没有结尾标点
//! 的文字，用户以为模型坏了。
//!
//! ── 为什么判据要单独成模块且只有一份 ────────────────────
//! 非流式（读聚合好的 chat 体）与流式（从下发帧里攒出同一份三元组）两条链路都读
//! 这里的函数。写第二份「增量版」判据是头号风险：加一条判据漏改一处，就会出现
//! 「非流式续写、流式不续」这种查得出现象查不出根因的分歧。
//!
//! ── 用法纪律 ────────────────────────────────────────────
//! **工具轮永不续**：调用点的执行权在客户端，把它的 tool_calls 续写一遍等于伪造
//! 一次工具调用。这条判据在调用点做（本模块看不到「有没有 tool_calls」之外的
//! 上下文），调用前必须先排除工具轮。

/// `finish_reason` 表示「长度用尽」的各种写法。
///
/// 上游不统一：OpenAI 标准是 `length`，但兼容实现里 `max_tokens` /
/// `max_output_tokens` / `model_length` / `length_limit` 等都在真机出现过。
/// 只认 `length` 会漏掉一整类上游。
pub const LENGTH_FINISH_ALIASES: &[&str] = &[
    "length",
    "max_tokens",
    "max_output_tokens",
    "model_length",
    "length_limit",
    "max_length",
    "token_limit",
    "max_completion_tokens",
];

/// 一段正文「结构写到一半」的悬挂尾部（rstrip 后落在这些形态上）。
///
/// 都是「后面显然还有东西」的信号：逗号后面必接内容、竖线是表格中间、
/// 省略号与冒号都在引导下一句。
const DANGLING_TAILS: &[&str] = &[
    ",",
    "|",
    "...",
    "…",
    ":",
    "：",
    "，",
    "、",
    "；",
    "（",
    "(",
    "[",
    "{",
];

/// 正文以这些符号收尾算「说完了」。
const SENTENCE_ENDS: &[char] = &['。', '！', '？', '.', '!', '?', '”', '）', ')', '】', ']'];

/// 判据的输入：一轮结束时的三元组。
///
/// 非流式从聚合好的 chat 体里取，流式从下发帧里攒出**同样这三个** —— 两边口径
/// 必须一致（见模块头）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnOutcome<'a> {
    /// finish_reason（没有就传 None）。
    pub finish: Option<&'a str>,
    /// 正文长度（字符数，正文本身不需要带进来）。
    pub text_len: usize,
    /// 思考内容长度（字符数）。
    pub think_len: usize,
}

/// 这一轮像不像被腰斩。
///
/// 判据顺序照参考实现（先硬信号、再软信号）：
///   1. finish_reason 命中长度别名表；
///   2. 正文为空但**有思考**（整轮烧在思考通道里）；
///   3. 代码围栏 ``` 数量为奇数（未闭合的围栏是硬证据）；
///   4. 末 100 字符括号栈失衡（先给散文里的零星括号 +2 容忍，再严格配对）；
///   5. 正文 rstrip 后落在悬挂尾部集合里，或「长思考 + 极短正文且不以句末符号收尾」。
pub fn looks_truncated(o: TurnOutcome<'_>, text: &str) -> bool {
    // ① 长度别名（最硬的信号：上游自己说了是长度问题）
    if let Some(f) = o.finish {
        let low = f.to_ascii_lowercase();
        if LENGTH_FINISH_ALIASES.iter().any(|a| low == *a) {
            return true;
        }
    }
    // ② 空正文 + 有思考：该再问一次
    if o.text_len == 0 && o.think_len > 0 {
        return true;
    }
    // 正文与思考都空 → 归既有的空答复兜底链，本模块不管
    if o.text_len == 0 {
        return false;
    }
    let t: Vec<char> = text.chars().collect();
    // ③ 未闭合的代码围栏
    if count_fences(text) % 2 == 1 {
        return true;
    }
    // 只看末尾一段：正文中间有括号是常态，末尾失衡才是截断信号
    let tail_start = o.text_len.saturating_sub(100);
    let tail: String = t[tail_start.min(t.len())..].iter().collect();
    // ④ 括号栈失衡（+2 容忍散文里的零星括号）
    if bracket_imbalance(&tail) > 2 {
        return true;
    }
    // ⑤ 悬挂尾部 / 长思考 + 极短正文
    let trimmed = text.trim_end();
    if DANGLING_TAILS.iter().any(|d| trimmed.ends_with(d)) {
        return true;
    }
    if o.think_len > 200 {
        let short = o.text_len < 20;
        let no_end = !trimmed.ends_with(SENTENCE_ENDS);
        if short && no_end {
            return true;
        }
    }
    false
}


/// 从 OpenAI chat 响应体取判据需要的三元组（非流式路径用）。
///
/// 返回 None 表示响应形状不认识（没有 choices）—— 那就不判，宁可少标一次。
pub fn outcome_from_chat(v: &serde_json::Value) -> Option<(TurnOutcome<'_>, &str)> {
    let first = v.get("choices")?.as_array()?.first()?;
    let msg = first.get("message");
    let text = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let think = msg
        .and_then(|m| m.get("reasoning_content").or_else(|| m.get("reasoning")))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let finish = first.get("finish_reason").and_then(|f| f.as_str());
    Some((
        TurnOutcome {
            finish,
            text_len: text.chars().count(),
            think_len: think.chars().count(),
        },
        text,
    ))
}

/// 数代码围栏 ``` 的个数（整篇数，不是只看末尾）。
///
/// 只数 ```（三个反引号）：~~~ 与单个 ` 在散文里太常见，会误判。
fn count_fences(text: &str) -> usize {
    let mut n = 0usize;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '`' {
            let mut run = 0usize;
            while i < chars.len() && chars[i] == '`' {
                run += 1;
                i += 1;
            }
            if run == 3 {
                n += 1;
            }
        } else {
            i += 1;
        }
    }
    n
}

/// 括号栈失衡程度：正数=左括号没配上，负数=右括号多出来。
///
/// 只数三种常见括号；字符串里的括号无法区分（那需要词法分析），散文场景够用。
fn bracket_imbalance(tail: &str) -> i32 {
    let mut stack: Vec<char> = Vec::new();
    for c in tail.chars() {
        match c {
            '(' | '[' | '{' => stack.push(c),
            ')' => {
                if stack.last() == Some(&'(') {
                    stack.pop();
                }
            }
            ']' => {
                if stack.last() == Some(&'[') {
                    stack.pop();
                }
            }
            '}' => {
                if stack.last() == Some(&'{') {
                    stack.pop();
                }
            }
            _ => {}
        }
    }
    stack.len() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(text: &str, think: usize) -> TurnOutcome<'static> {
        TurnOutcome {
            finish: None,
            text_len: text.chars().count(),
            think_len: think,
        }
    }

    #[test]
    fn length_finish_reasons_are_truncated() {
        // 上游自己说了是长度问题 —— 最硬的信号，且各种写法都要认
        for f in LENGTH_FINISH_ALIASES {
            let o = TurnOutcome {
                finish: Some(f),
                text_len: 10,
                think_len: 0,
            };
            assert!(looks_truncated(o, "随便一段话"), "{f} 应判为截断");
        }
        // 正常结束不算
        let done = TurnOutcome {
            finish: Some("stop"),
            text_len: 5,
            think_len: 0,
        };
        assert!(!looks_truncated(done, "说完了。"));
    }

    #[test]
    fn empty_text_with_thinking_is_truncated() {
        // 整轮烧在思考通道里，正文一个字没出来
        let o = TurnOutcome {
            finish: None,
            text_len: 0,
            think_len: 500,
        };
        assert!(looks_truncated(o, ""));
        // 正文与思考都空 → 归空答复兜底链，本模块不管（避免与那条链重复处理）
        let empty = TurnOutcome {
            finish: None,
            text_len: 0,
            think_len: 0,
        };
        assert!(!looks_truncated(empty, ""));
    }

    #[test]
    fn unclosed_code_fence_is_truncated() {
        // 奇数个围栏 = 有开没闭，是硬证据
        let s = "看这段代码：\n```rust\nfn main() {\n";
        assert!(looks_truncated(outcome(s, 0), s));
        // 成对闭合就不算
        let ok = "```rust\nfn main() {}\n```";
        assert!(!looks_truncated(outcome(ok, 0), ok), "闭合的围栏不该算");
    }

    #[test]
    fn dangling_tail_is_truncated_but_complete_sentence_is_not() {
        assert!(looks_truncated(outcome("首先，", 0), "首先，"));
        assert!(looks_truncated(outcome("步骤一：", 0), "步骤一："));
        // 以句末符号收尾 = 说完了
        assert!(!looks_truncated(
            outcome("这就是答案。", 0),
            "这就是答案。"
        ));
    }

    #[test]
    fn long_thinking_with_a_stub_of_text_is_truncated() {
        // 思考写了一大篇、正文只吐两个词且没结尾 —— 明显被腰斩
        let o = TurnOutcome {
            finish: None,
            text_len: 3,
            think_len: 800,
        };
        assert!(looks_truncated(o, "所以是"));
        // 同样短的正文但思考很少：就是「模型答得短」，不该续
        let o2 = TurnOutcome {
            finish: None,
            text_len: 3,
            think_len: 5,
        };
        assert!(!looks_truncated(o2, "好的。"), "短答案不该被当成截断");
    }

    #[test]
    fn prose_with_a_few_brackets_is_not_truncated() {
        // 散文里零星出现的括号（+2 容忍）不该被当成失衡
        let s = "这里有个括号（见上文），还有一个 [引用]。";
        assert!(!looks_truncated(outcome(s, 0), s), "{s}");
        // 末尾一大串没配上的左括号才是信号
        let bad = "答案是 fn(a, b, c, d, e, f, g, h(";
        assert!(looks_truncated(outcome(bad, 0), bad));
    }
}
