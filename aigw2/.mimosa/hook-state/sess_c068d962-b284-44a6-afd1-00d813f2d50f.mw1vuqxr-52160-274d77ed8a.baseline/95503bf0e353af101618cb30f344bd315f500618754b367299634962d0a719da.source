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
///
/// ★ `11128` **不在这张表里**，见 [`rewrite_bare_11128`] 的注释：
///   它是 5 位阿拉伯数字，作为用户内容出现时（邮编 / 端口 / 行号 / 数学题）
///   替换它会直接损坏请求。只在上游确实按它拦截时才按需处理。
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
];

/// 裸数字 `11128` 的改写。
///
/// ## 为什么单独处理，而不是放进 [`REWRITES`]
///
/// `11128` 是普通 5 位数（邮编、端口、行号、ID、数学题里的数）。无条件全局替换
/// 会**损坏用户内容**，实测几种真实输入：
///
/// | 输入 | 被改成 | 后果 |
/// |---|---|---|
/// | `{"port": 11128}` | `{"port": 11-128}` | 变成非法 JSON |
/// | `SELECT * FROM t WHERE id=11128` | `... id=11-128` | 查询语义改变 |
/// | `11128 是质数吗` | `11-128 是质数吗` | 问题被改掉 |
///
/// 而这条替换的唯一理由是「上游看到裸 11128 会整单拦截」。那个拦截只发生在
/// **请求是由某个特定客户端发出**的场景里（它的错误码恰好是 11128），
/// 与用户内容里出现这个数字不是一回事。
///
/// ⇒ 策略：默认**不改**。只有在同一个请求里已经识别出客户端指纹
///   （`cc_` 键值 / billing-header / 客户端自证句）时才连带改写 —— 那种请求
///   本来就是要过风控的，改写它不会伤到用户表达；纯用户内容一律原样上行。
///
/// 若将来确认某个上游对「任何」请求里的 11128 都拦截，应改为按 provider 开关
/// 显式启用，而不是退回全局替换。
const BARE_11128: &str = "11128";
const BARE_11128_REWRITTEN: &str = "11-128";

/// 这个文本里是否出现了客户端指纹（决定要不要连带改写裸 11128）。
///
/// 与 [`has_fingerprint`] 的区别：这里**不含** `11128` 自身。否则
/// `「11128 是质数吗」` 会因为自身命中而触发改写，正好是要避免的情况。
///
/// ★ `cc_` 不能当裸前缀判：`int cc_count = 0;`、`SELECT cc_id FROM cost_center`、
///   `cc_type` 这类普通代码/文本到处都有 `cc_`，判成指纹会把它们连带卷进
///   「客户端请求」分支（进而改写 11128、剥离 cc_ 段）。真正的指纹是
///   **键值对**形态：`cc_entrypoint=cli;`、`cc_version=1.2.3;`。
///   所以这里要求 `cc_<名字>=` 三段齐备（见 [`has_cc_kv`]）。
fn has_client_fingerprint(text: &str) -> bool {
    const FEATURES: &[&str] = &[
        "x-anthropic-billing-header",
        "You are Claude Code",
        "Main branch (",
        "You are a coding agent running in the Codex CLI",
        "github.com/anthropics/",
    ];
    FEATURES.iter().any(|f| text.contains(f))
        || text.to_ascii_lowercase().contains("x-anthropic-billing-header")
        || has_cc_kv(text)
}

/// 文本里是否存在 `cc_<名字>=` 这种键值对（客户端指纹的真形态）。
///
/// 只看前缀 + 名字 + 等号，不要求后面有值（值可能为空，仍是键值对）。
fn has_cc_kv(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i + 3 <= bytes.len() {
        // 找 "cc_"（前面必须是词边界，避免 xcc_foo 之类）
        if bytes[i] == b'c'
            && bytes[i + 1] == b'c'
            && bytes[i + 2] == b'_'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            // 键名：cc_ 之后至少一个字母数字/下划线
            let mut j = i + 3;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 3 && j < bytes.len() && bytes[j] == b'=' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// 特征预检：快速路径，一个都不中就直接返回原串（零分配）。
fn has_fingerprint(text: &str) -> bool {
    // ★ `11128` 仍算「值得进处理流程」的信号 —— 它可能是客户端请求里的风控码，
    //   也可能是用户内容里的普通数字。进了 `sanitize_text` 之后由
    //   `has_client_fingerprint` 决定到底改不改（见 BARE_11128 的注释）。
    has_client_fingerprint(text) || text.contains(BARE_11128)
}

/// 单段文本净化。
fn sanitize_text(text: &str) -> String {
    if !has_fingerprint(text) {
        return text.to_string();
    }
    // ★ 先判「这段文本到底是不是客户端指纹」。不是的话（典型：用户自己写的
    //   `{"port": 11128}`、`SELECT ... WHERE id=11128`、`11128 是质数吗`）
    //   原样返回 —— 绝不能因为出现了 5 位数字就改用户内容。
    let is_client_text = has_client_fingerprint(text);
    if !is_client_text {
        // 走到这里只可能因为命中了 BARE_11128 这一条；没有别的可做。
        return text.to_string();
    }
    let mut out = text.to_string();
    // ① 改写层
    for (from, to) in REWRITES {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    // ①b 裸 11128：只在确认是客户端文本时连带改写（见 BARE_11128 注释）
    if out.contains(BARE_11128) {
        out = out.replace(BARE_11128, BARE_11128_REWRITTEN);
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
        // ★ 上限是**防御性**的，不是性能优化。
        //   这个循环的安全性依赖 `strip_cc_kv` 幂等（第二遍不再变化）。一旦哪次
        //   改动让它非幂等（历史事故：`bytes[i] as char` 把中文拆成乱码，每轮都产出
        //   不同字符串 → `prev != out` 恒真 → 无限 clone 直到 OOM abort），
        //   这里就会变成死循环。加个硬上限，把「进程崩溃」降级成「多剥几遍」。
        //   正常文本一轮就稳定，所以上限取 16 完全够。
        let mut prev = String::new();
        let mut rounds = 0;
        while prev != out {
            if rounds >= 16 {
                tracing::warn!("cc_ 键值剥离 16 轮仍未收敛，停止续剥（输入可能触发了非幂等形态）");
                break;
            }
            rounds += 1;
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
        // ★ 按**字符**边界前进，不能 `bytes[i] as char`。
        //
        //   旧实现把 UTF-8 的**字节**当**码点**压进去：一个中文字符的 3 个字节
        //   （如 `你` = E4 BD A0）会变成 `ä½ ` 三个互不相关的字符。后果有两层：
        //     1. 文本被毁（用户看到的是乱码）；
        //     2. **更严重**：`sanitize_text` 的剥离层是 `while prev != out` 的循环，
        //        而乱码后的文本下一轮还会被再次改写（码点→UTF-8 字节→再拆），
        //        每轮都产生新字符串、永不相等 → 死循环 `clone()` → OOM。
        //        实测这段代码能让测试进程分配 8.8 GB 后 abort。
        //   正确做法：取出该位置的真实字符（按 UTF-8 长度跳），原样压入。
        let ch_len = utf8_char_len(bytes[i]);
        let end = (i + ch_len).min(bytes.len());
        // `from_utf8_lossy` 兜底：输入 str 本身合法，这里只是防御（切片不会越界）。
        out.push_str(&s[i..end]);
        i = end;
    }
    out
}

/// 该字节开头的 UTF-8 字符占几个字节。
///
/// 输入是合法 `&str` 的字节，所以这里只需看首字节的高位模式；非法形态按 1 处理，
/// 保证 `i` 一定前进（**绝不允许返回 0**，否则就是死循环）。
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        // 续字节（10xxxxxx）或非法首字节：按 1 前进，宁可少合并也不卡死
        1
    }
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


/// 最小中性系统提示词：内容被上游审核拦下时的**重发替身**。
///
/// 一句话、无客户端特征、不带任何可被逐字匹配的产品名。用它替掉客户端那几千字的
/// system 模板后，同一条请求通常就能过审 —— 因为拦它的多半是模板里的指纹句，
/// 而不是用户真正问的内容。
pub const NEUTRAL_SYSTEM: &str = "You are a helpful assistant. Answer the user's request directly.";

/// 这次 400 是不是「上游审核拦下了提示词」。
///
/// ★ 为什么需要这道判定：上游按**逐字**匹配审核，撞上就是 HTTP 400，而撞上的常常是
///   客户端（Claude Code / Codex CLI 等）system 模板里的指纹句 —— 那不是用户的错，
///   也不是账号的错。脱敏表必然滞后于客户端模板更新，所以除了拦截前脱敏，
///   还要能在被拦下之后**认出这件事**并补救一次。
///
/// 判定刻意保守：只认明确的审核/内容类措辞与已知错误码，认不出来就返回 false,
///   让请求按普通 400 处理（宁可少救一次，也不要把「参数真写错了」当成审核去重试）。
pub fn looks_like_content_block(status: u16, body: &str) -> bool {
    if status != 400 {
        return false;
    }
    // 已知的风控/内容错误码（脱敏层也会把裸 11128 改写掉，这里认改写前后两种形态）
    if body.contains("11128") || body.contains("11-128") {
        return true;
    }
    let low = body.to_ascii_lowercase();
    const HINTS: &[&str] = &[
        // 中文
        "内容安全",
        "内容审核",
        "敏感词",
        "违规",
        "违反",
        "审核",
        "不符合",
        "风险提示",
        // 英文
        "content policy",
        "content filter",
        "safety",
        "blocked",
        "moderation",
        "prompt blocked",
    ];
    HINTS.iter().any(|h| low.contains(h))
}

/// 把请求体换成「中性提示词版本」：替掉 system、清掉工具描述。
///
/// 返回是否真的改了东西（没 system 也没 tools 时返回 false，调用方据此跳过重发，
/// 否则一次纯属浪费的重试只会把失败延迟一拍）。
pub fn neutralize_request_body(v: &mut Value) -> bool {
    let Some(o) = v.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if o.contains_key("system") {
        o["system"] = Value::String(NEUTRAL_SYSTEM.to_string());
        changed = true;
    }
    // tools 的 description 里同样可能带客户端特征句，一并清成空描述
    if let Some(tools) = o.get_mut("tools").and_then(|x| x.as_array_mut()) {
        for tl in tools.iter_mut() {
            if let Some(Value::String(s)) = tl.get_mut("description") {
                *s = String::new();
                changed = true;
            }
            if let Some(Value::String(s)) = tl
                .get_mut("function")
                .and_then(|f| f.get_mut("description"))
            {
                *s = String::new();
                changed = true;
            }
        }
    }
    changed
}


// ---- 内容拦截降级状态机 ----
//
// 照搬 Agent2API 的 core::degrade（它又来自 workbuddy2api 的 internal/server/degrade.go）。
// 配合上面的重发补救：补救解决「这一次」，降级解决「接下来一段时间」——
// 否则每个请求都要先撞一次 400 再救回来，白花一轮往返。
//
// ★ 三个语义细节，改之前先看懂：
//   ① 降级期到**次日 00:00（UTC+8）**结束，不是固定时长也不是无限期：上游风控
//      按天统计，零点是一次天然重置；无限期降级会让用户在问题消失后仍拿不到
//      自己的 system 提示词。
//   ② **触发不续期**：已在降级期内再触发，保持最早那个零点。否则一个持续报错
//      的客户端会把截止时刻无限推后，等于永久降级。
//   ③ **内存态、重启清零**：这是运行期状态不是配置；落盘还要引入「什么时候该清」
//      的第二套判据，得不偿失。

/// UTC+8 固定偏移（秒）。中国标准时间没有夏令时，不需要时区库。
const CST_OFFSET_S: i64 = 8 * 3600;
/// 一天的秒数
const DAY_S: i64 = 86_400;

/// 降级截止时刻（unix 秒）；0 = 未触发。
///
/// 用一个整数而不是 Mutex<Option<..>>：状态就是个数，读写都是「取/存一次」，
/// 并发触发最坏是两个线程算出同一个零点，无害。
static DEGRADE_UNTIL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 当前是否处于降级期。
pub fn degrade_active() -> bool {
    degrade_until() > 0
}

/// 降级截止时刻（unix 秒）；0 = 不在降级期。
///
/// 过期值在这里就被当成「没有」，于是调用方不必自己比时间，也不会出现
/// 「一处说降级中、另一处说已结束」的分叉。
pub fn degrade_until() -> i64 {
    let until = DEGRADE_UNTIL.load(std::sync::atomic::Ordering::SeqCst);
    if until > 0 && now_unix() < until {
        until
    } else {
        0
    }
}

/// 触发降级，返回生效的截止时刻。已在降级期内**不续期**。
pub fn degrade_trigger() -> i64 {
    let now = now_unix();
    let cur = DEGRADE_UNTIL.load(std::sync::atomic::Ordering::SeqCst);
    if cur > now {
        return cur;
    }
    let next = next_midnight_cst(now);
    DEGRADE_UNTIL.store(next, std::sync::atomic::Ordering::SeqCst);
    next
}

/// 解除降级（用户当场改好了提示词时不该等到零点）。
pub fn degrade_clear() {
    DEGRADE_UNTIL.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// `now` 之后最近的 UTC+8 零点（纯函数，便于单测）。
fn next_midnight_cst(now: i64) -> i64 {
    let local = now + CST_OFFSET_S;
    let into_day = local.rem_euclid(DAY_S);
    // ★ 必须在 local 时间轴上算零点，再**减回偏移**转回 UTC 秒：直接把 local
    //   零点当 unix 秒返回，等于把截止时刻往后推了 8 小时（降级会一直持续到
    //   当天 16:00 而不是 00:00 结束）。这条被单测 `next_midnight_cst_lands_on_
    //   local_zero` 抓出来过。
    local - into_day + DAY_S - CST_OFFSET_S
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {

    #[test]
    fn next_midnight_cst_lands_on_local_zero() {
        // 纯函数：验证「次日 UTC+8 零点」的计算，不依赖当前时钟。
        // 基准 = 2026-01-01 00:00 CST = 2025-12-31 16:00 UTC。
        // 校验方式：base + 8h 必须正好落在 UTC 日界上（否则基准本身就是错的）。
        let cst_midnight: i64 = 1_767_196_800;
        assert_eq!(
            (cst_midnight + CST_OFFSET_S) % DAY_S,
            0,
            "基准必须是某个 UTC+8 零点"
        );
        // 早上 06:00 CST：距次日 00:00 还有 18 小时
        let morning = cst_midnight + 6 * 3600;
        let next = next_midnight_cst(morning);
        assert_eq!(next - morning, 18 * 3600, "06:00 距次日 00:00 应为 18 小时");
        // 算出来的时刻本身也必须是一个 UTC+8 零点
        assert_eq!((next + CST_OFFSET_S) % DAY_S, 0);
        // 恰好零点：应指向下一个零点（24 小时后），而不是原地不动
        assert_eq!(
            next_midnight_cst(cst_midnight) - cst_midnight,
            DAY_S,
            "零点触发也要跨到下一天"
        );
        // 23:59:59：距零点只剩 1 秒
        let almost = cst_midnight + DAY_S - 1;
        assert_eq!(next_midnight_cst(almost) - almost, 1);
        // 晚上 23:00：距零点 1 小时
        let evening = cst_midnight + 23 * 3600;
        assert_eq!(next_midnight_cst(evening) - evening, 3600);
    }

    #[test]
    fn degrade_state_machine_lifecycle() {
        // ★ 全局静态量：本测试内部完成「触发 → 不续期 → 解除」全流程，
        //   收尾必定 clear，避免影响其它用例（Rust 测试默认并发执行）。
        degrade_clear();
        assert!(!degrade_active(), "初始不应处于降级期");
        assert_eq!(degrade_until(), 0);

        let first = degrade_trigger();
        assert!(first > 0, "触发后应有截止时刻");
        assert!(degrade_active(), "触发后应处于降级期");
        // 截止时刻必须落在未来（否则降级形同虚设）
        assert!(first > now_unix());

        // ★ 不续期：降级期内再触发，截止时刻必须保持不变。
        //   否则一个持续报错的客户端会把截止无限推后 = 永久降级。
        assert_eq!(degrade_trigger(), first, "降级期内触发不得续期");

        degrade_clear();
        assert!(!degrade_active(), "解除后应退出降级期");
        assert_eq!(degrade_until(), 0);
    }

    use super::*;
    #[test]
    fn content_block_is_only_recognized_on_400() {
        // 只有 400 才可能是审核拦截；401/429/500 各有各的处理办法，
        // 把它们误认成内容问题会掩盖真正的凭据/限流/上游故障。
        assert!(looks_like_content_block(400, "content policy violation"));
        assert!(!looks_like_content_block(401, "content policy violation"));
        assert!(!looks_like_content_block(429, "blocked"));
        assert!(!looks_like_content_block(500, "safety"));
    }
    #[test]
    fn content_block_recognizes_both_languages_and_error_codes() {
        assert!(looks_like_content_block(400, "请求命中内容安全策略"));
        assert!(looks_like_content_block(400, "Content filter blocked"));
        // 已知风控码（脱敏后写作 11-128，两种形态都要认）
        assert!(looks_like_content_block(400, "err 11128"));
        assert!(looks_like_content_block(400, "err 11-128"));
        // 真正的参数错误不该被认成审核：认错了就是白重试一次
        assert!(!looks_like_content_block(400, "model not found"));
        assert!(!looks_like_content_block(400, "missing required field: model"));
    }
    #[test]
    fn neutralize_replaces_system_and_clears_tool_descriptions() {
        let mut v = json!({
            "model": "m",
            "system": "You are Claude Code, Anthropic's official CLI for Claude.",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "t", "description": "Claude Code built-in tool" }]
        });
        assert!(neutralize_request_body(&mut v));
        assert_eq!(v["system"], NEUTRAL_SYSTEM);
        assert_eq!(v["tools"][0]["description"], "");
        // 用户的提问必须原样保留：中性化只动 system 与工具描述
        assert_eq!(v["messages"][0]["content"], "hi");
    }
    #[test]
    fn neutralize_reports_false_when_there_is_nothing_to_swap() {
        // 没有 system 也没有 tools：重发也不会变好，调用方应跳过
        let mut v = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        assert!(!neutralize_request_body(&mut v));
    }
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

    /// 剥离 `cc_xxx=...;` 时，**非 ASCII 字符不能被打成乱码**。
    ///
    /// 旧实现用 `out.push(bytes[i] as char)` 逐字节重建字符串 —— 那是把 UTF-8
    /// **字节**当成 **Unicode 码点**压进去：一个中文字符的 3 个字节会变成 3 个
    /// 毫无关系的字符。只要这段文本里恰好有一个 `cc_xxx=...;`（于是走剥离层），
    /// 整段中文就全毁。
    ///
    /// 真实场景：Claude Code 的 system prompt 里带 `cc_version=…;`，而用户/团队
    /// 在同一段里追加了中文说明（「你是内部助手」「按以下规范回答」）——
    /// 这些中文会被替换成乱码后发给模型，模型看到的是不可读内容。
    #[test]
    fn strip_cc_kv_preserves_non_ascii_text() {
        let original = "你是内部助手，请按规范回答。cc_version=1.2.3; 以上是全部说明。";
        let mut v = json!({"system": original});
        sanitize_request_body(&mut v);
        let s = v["system"].as_str().unwrap();
        assert!(
            s.contains("你是内部助手"),
            "中文被毁：{s}"
        );
        assert!(
            s.contains("以上是全部说明"),
            "cc_ 之后的中文也被毁：{s}"
        );
        assert!(!s.contains("cc_version"), "cc_ 键值应被剥离：{s}");
        // 不应出现任何替换字符或乱码特征（UTF-8 被按字节拆开会落进拉丁扩展区）
        assert!(
            !s.chars().any(|c| ('\u{80}'..='\u{24F}').contains(&c) && !c.is_ascii()),
            "出现了疑似 UTF-8 拆字节产生的乱码：{s}"
        );
    }

    /// emoji / 三字节以上字符同样不能被拆坏。
    #[test]
    fn strip_cc_kv_preserves_emoji_and_wide_chars() {
        let original = "🚀 发射 cc_entrypoint=cli; ✅ 完成";
        let mut v = json!({"system": original});
        sanitize_request_body(&mut v);
        let s = v["system"].as_str().unwrap();
        assert!(s.contains("🚀"), "emoji 被毁：{s}");
        assert!(s.contains("✅"), "emoji 被毁：{s}");
        assert!(s.contains("发射") && s.contains("完成"), "中文被毁：{s}");
        assert!(!s.contains("cc_entrypoint"));
    }

    /// `cc_` 只有当它是**键值对**时才算客户端指纹。
    ///
    /// 普通代码里 `cc_count` / `cc_id` / `cc_type` 到处都是。把它们判成指纹会
    /// 连带把这段文本推进「客户端请求」分支（改写 11128、剥离 cc_ 段），
    /// 等于无缘无故损坏用户代码。这条测试钉住「不误伤」。
    #[test]
    fn bare_cc_prefix_is_not_a_client_fingerprint() {
        for code in [
            "int cc_count = 0;  // 计数器",
            "for (cc_iter = 0; cc_iter < n; cc_iter++)",
            "SELECT cc_id, cc_name FROM cost_center",
            "变量名 cc_type 表示信用卡类型",
            "cc_",              // 只有前缀
            "xcc_foo",          // 前面是字母，不是词边界
            "cc_name",          // 有名字但没等号
        ] {
            assert!(
                !has_client_fingerprint(code),
                "不该判成客户端指纹：{code}"
            );
        }
        // 真指纹（键值对）必须仍然认得
        for real in [
            "cc_entrypoint=cli;",
            "cc_version=1.2.3;",
            "cc_model=gpt-5;",
            "cc_x=",          // 值为空也算键值对
        ] {
            assert!(has_client_fingerprint(real), "应判成客户端指纹：{real}");
        }
    }

    /// 普通代码文本即使含 `cc_` 前缀也不该被改写。
    #[test]
    fn code_with_cc_prefix_is_untouched() {
        let mut v = json!({"messages":[{"role":"user","content":"int cc_count = 11128; // 计数器"}]});
        let changed = sanitize_request_body(&mut v);
        let got = v["messages"][0]["content"].as_str().unwrap();
        assert_eq!(got, "int cc_count = 11128; // 计数器", "代码被改写了");
        assert!(changed.is_empty(), "不该报告改动：{changed:?}");
    }

    /// 指纹判定是**逐段**的：system 里是客户端指纹，不代表用户消息也是。
    ///
    /// 这条隔离性防止「为了过风控而误伤用户内容」：
    /// system 里出现 `cc_version=1.2.3;` 时，用户消息里的 `cc_count=11128` 必须原样保留。
    /// 两者分属不同字段，`sanitize_text` 分别处理，所以判据不会跨字段串味。
    #[test]
    fn fingerprint_detection_does_not_leak_across_fields() {
        let mut v = json!({
            "system": "You are Claude Code, Anthropic's official CLI for Claude\ncc_version=1.2.3;",
            "messages": [
                {"role": "user", "content": "int cc_count = 11128; // 我的计数器"}
            ]
        });
        sanitize_request_body(&mut v);
        // system 被处理了（这是对的：它本来就是客户端模板）
        let sys = v["system"].as_str().unwrap();
        assert!(sys.contains("official CLI tool"), "system 应被改写：{sys}");
        assert!(!sys.contains("cc_version"), "system 的 cc_ 键值应被剥离：{sys}");
        // 用户消息一个字都不许动
        let msg = v["messages"][0]["content"].as_str().unwrap();
        assert_eq!(
            msg, "int cc_count = 11128; // 我的计数器",
            "用户消息被 system 的指纹判定串味了"
        );
    }

    /// 客户端指纹 + 裸 11128 同现时，11128 仍要改写（那种请求本来就要过风控）。
    #[test]
    fn rewrites_bare_11128_alongside_client_fingerprint() {
        let mut v = json!({
            "messages":[{"role":"user","content":"cc_version=1.2.3; code=11128 happened"}]
        });
        sanitize_request_body(&mut v);
        let s = v["messages"][0]["content"].as_str().unwrap();
        assert!(s.contains("11-128"), "应连带改写 11128：{s}");
        assert!(!s.contains("11128"), "11128 应已被改写：{s}");
        assert!(!s.contains("cc_version"), "cc_ 键值应被剥离：{s}");
    }

    /// ★ 用户内容里的 `11128` **绝不能**被改写。
    ///
    /// 旧实现把 `("11128","11-128")` 放进无条件改写表，于是这些真实输入全被
    /// 静默损坏（`{"port": 11-128}` 直接是非法 JSON）。这条测试把「不改」钉死。
    #[test]
    fn user_content_with_11128_is_left_alone() {
        let cases = [
            r#"{"port": 11128}"#,
            "SELECT * FROM t WHERE id=11128",
            "11128 是质数吗",
            "我的邮编是 11128",
            "line 11128 has a typo",
        ];
        for case in cases {
            let mut v = json!({"messages":[{"role":"user","content":case}]});
            let changed = sanitize_request_body(&mut v);
            let got = v["messages"][0]["content"].as_str().unwrap();
            assert_eq!(got, case, "用户内容被改写了（改动字段 {changed:?}）");
            assert!(
                changed.is_empty(),
                "不该报告改动，实际 {changed:?}；内容={case}"
            );
        }
    }

    /// 判据本身：`11128` 不构成「这是客户端文本」。
    #[test]
    fn bare_11128_is_not_a_client_fingerprint() {
        assert!(!has_client_fingerprint("11128"));
        assert!(!has_client_fingerprint(r#"{"port": 11128}"#));
        assert!(!has_client_fingerprint("11128 是质数吗"));
        // 但真正的指纹仍然认
        assert!(has_client_fingerprint("cc_version=1.2.3"));
        assert!(has_client_fingerprint("x-anthropic-billing-header: abc;"));
        assert!(has_client_fingerprint("You are Claude Code"));
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
