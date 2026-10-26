//! 小米 MiMo（aistudio.xiaomimimo.com）adapter —— 纯 HTTP，网页登录态（三个 Cookie）。
//!
//! 一手来源：xiaoY233/Chat2API 的 src/main/proxy/adapters/mimo.ts。
//! 该平台没有 OpenAI 兼容端点，一轮对话是私有协议：
//! 1. POST /open-apis/chat/conversation/save?xiaomichatbot_ph=… {conversationId,title,type} -> code=0；
//! 2. POST /open-apis/bot/chat?xiaomichatbot_ph=… {msgId,conversationId,query,modelConfig,multiMedias} -> SSE；
//! 3. SSE 用 event: 行标注类型：message/text 带增量 content，usage 带 token 数，dialogId 带对话 id。
//!
//! 凭据是浏览器的三个 Cookie：serviceToken / userId / xiaomichatbot_ph。

use crate::{RelayError, StreamStat};
use aigw_core::ErrorClass;
use aigw_registry::ProviderSpec;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub const MIMO_BASE: &str = "https://aistudio.xiaomimimo.com";
const MIMO_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36";

fn er(status: u16, body: impl Into<String>, class: ErrorClass) -> RelayError {
    RelayError { status, body: body.into(), class }
}

fn secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn uuid_like() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut b = [0u8; 16];
    rng.fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: Vec<String> = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn base_of(spec: &ProviderSpec) -> String {
    if spec.base_url.is_empty() {
        MIMO_BASE.to_string()
    } else {
        spec.base_url.trim_end_matches('/').to_string()
    }
}

/// 凭据：serviceToken / userId / xiaomichatbot_ph。
#[derive(Debug, Clone)]
pub struct MimoCred {
    pub service_token: String,
    pub user_id: String,
    pub ph_token: String,
}

/// 接受三种形态：整段 Cookie、纯 JSON、只给 xiaomichatbot_ph（极少见，其余会缺失而报错）。
pub fn parse_cred(secret: &str) -> Result<MimoCred, String> {
    let s = secret.trim();
    if s.is_empty() {
        return Err("凭据为空：需要 serviceToken / userId / xiaomichatbot_ph 三个 Cookie".into());
    }
    if s.starts_with('{') {
        let v: Value = serde_json::from_str(s).map_err(|e| format!("JSON 凭据解析失败：{e}"))?;
        let pick = |keys: &[&str]| -> Option<String> {
            keys.iter()
                .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
        };
        let st = pick(&["service_token", "serviceToken"]);
        let uid = pick(&["user_id", "userId"]);
        let ph = pick(&["ph_token", "xiaomichatbot_ph"]);
        return match (st, uid, ph) {
            (Some(a), Some(b), Some(c)) => Ok(MimoCred { service_token: a, user_id: b, ph_token: c }),
            _ => Err("JSON 凭据缺字段：需要 service_token / user_id / ph_token".into()),
        };
    }
    let mut st: Option<String> = None;
    let mut uid: Option<String> = None;
    let mut ph: Option<String> = None;
    for part in s.split(';') {
        let Some((k, v)) = part.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').to_string();
        match k.trim() {
            "serviceToken" | "service_token" => st = Some(v),
            "userId" | "user_id" => uid = Some(v),
            "xiaomichatbot_ph" | "ph_token" => ph = Some(v),
            _ => {}
        }
    }
    match (st, uid, ph) {
        (Some(a), Some(b), Some(c)) if !a.is_empty() && !b.is_empty() && !c.is_empty() => {
            Ok(MimoCred { service_token: a, user_id: b, ph_token: c })
        }
        _ => Err("Cookie 里缺 serviceToken / userId / xiaomichatbot_ph 之一：请从 aistudio.xiaomimimo.com 的请求头复制整段 Cookie".into()),
    }
}

fn headers(spec: &ProviderSpec, c: &MimoCred) -> Vec<(String, String)> {
    let mut hs: Vec<(String, String)> = vec![
        ("Content-Type".into(), "application/json".into()),
        ("Accept".into(), "*/*".into()),
        ("Accept-Language".into(), "zh-CN,zh;q=0.9,en;q=0.8".into()),
        ("Origin".into(), MIMO_BASE.into()),
        ("Referer".into(), format!("{MIMO_BASE}/")),
        ("X-Timezone".into(), "Asia/Shanghai".into()),
        ("User-Agent".into(), MIMO_UA.into()),
        (
            "Cookie".into(),
            format!(
                "serviceToken={}; userId={}; xiaomichatbot_ph={}",
                c.service_token, c.user_id, c.ph_token
            ),
        ),
    ];
    for (k, v) in &spec.headers {
        if !hs.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
            hs.push((k.clone(), v.clone()));
        }
    }
    hs
}

fn apply(rb: reqwest::RequestBuilder, hs: &[(String, String)]) -> reqwest::RequestBuilder {
    let mut rb = rb;
    for (k, v) in hs {
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb
}

fn flatten_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// 把 OpenAI messages 折成 MiMo 的单条 query（参考实现 buildMimoQuery）。
fn build_query(messages: &Value) -> String {
    let mut entries: Vec<(String, String)> = Vec::new();
    if let Some(arr) = messages.as_array() {
        for m in arr {
            let role = m.get("role").and_then(|x| x.as_str()).unwrap_or("user");
            let content = flatten_content(m.get("content").unwrap_or(&Value::Null));
            if content.trim().is_empty() {
                continue;
            }
            let r = match role {
                "system" => "System",
                "assistant" => "Assistant",
                _ => "User",
            };
            if let Some(last) = entries.last_mut() {
                if last.0 == r {
                    last.1.push_str("\n\n");
                    last.1.push_str(&content);
                    continue;
                }
            }
            entries.push((r.to_string(), content));
        }
    }
    if entries.len() == 1 && entries[0].0 == "User" {
        return entries[0].1.clone();
    }
    entries
        .iter()
        .map(|(r, c)| format!("{r}: {c}"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn read_request(v: &Value) -> (String, String, f64, f64, bool) {
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("MiMo-V2.5")
        .to_string();
    let query = build_query(v.get("messages").unwrap_or(&Value::Null));
    let temperature = v.get("temperature").and_then(|x| x.as_f64()).unwrap_or(0.8);
    let top_p = v.get("top_p").and_then(|x| x.as_f64()).unwrap_or(0.95);
    let lower = model.to_lowercase();
    let think = lower.contains("think") || lower.contains("r1");
    (model, query, temperature, top_p, think)
}

/// 去掉正文里的引用标记：`citation:N`、内联 `(citation:N)`、并列多篇 `(citation:1, citation:2)`
/// 及其后的「中：」。形态取自参考实现 Chat2API 的 `mimo.ts`（CITATION_PATTERN）。
fn strip_citations(s: &str) -> String {
    const MARK: &str = "citation:";
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == 'c' {
            let head: String = chars[i..].iter().take(MARK.len()).collect();
            if head == MARK {
                let mut j = i + MARK.len();
                loop {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    // 同一处并列多篇引用时上游写成 (citation:1, citation:2)
                    if j < chars.len() && (chars[j] == ',' || chars[j] == '\u{ff0c}') {
                        let mut k = j + 1;
                        while k < chars.len() && chars[k] == ' ' {
                            k += 1;
                        }
                        let ahead: String = chars[k..].iter().take(MARK.len()).collect();
                        if ahead == MARK {
                            j = k + MARK.len();
                            continue;
                        }
                    }
                    break;
                }
                let mut paren = false;
                if j < chars.len() {
                    if out.ends_with('(') && chars[j] == ')' {
                        out.pop();
                        paren = true;
                    } else if out.ends_with('\u{ff08}') && chars[j] == '\u{ff09}' {
                        out.pop();
                        paren = true;
                    }
                }
                if paren {
                    j += 1;
                }
                // 松散形态常跟着冒号（citation:1： / citation:1:），冒号与其后空白一并吃掉
                let mut e = j;
                while e < chars.len() && (chars[e] == ' ' || chars[e] == '\u{3000}') {
                    e += 1;
                }
                if e < chars.len() && (chars[e] == ':' || chars[e] == '\u{ff1a}') {
                    j = e + 1;
                    while j < chars.len() && (chars[j] == ' ' || chars[j] == '\u{3000}') {
                        j += 1;
                    }
                }
                let mut has_zhong = false;
                if j + 1 < chars.len()
                    && chars[j] == '\u{4e2d}'
                    && (chars[j + 1] == '\u{ff1a}' || chars[j + 1] == ':')
                {
                    j += 2;
                    has_zhong = true;
                    while j < chars.len() && (chars[j] == ' ' || chars[j] == '\u{3000}') {
                        j += 1;
                    }
                }
                // 「从(citation:1)中：」整段是引用引导语，连「从」一起收掉
                if has_zhong && paren && out.ends_with('\u{4ece}') {
                    out.pop();
                }
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
/// 流式思考过滤：MiMo 把思考内容以 <think…>…</think 形式混在正文里，
/// 首包边界可能切在标签中间，所以未闭合时保留尾部若干字节等下一块。
#[derive(Default)]
pub struct ThinkFilter {
    buf: String,
    in_think: bool,
}

impl ThinkFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        self.buf.push_str(s);
        let mut out = String::new();
        loop {
            if self.in_think {
                match self.buf.find("</think") {
                    Some(idx) => match self.buf[idx..].find('>') {
                        Some(p) => {
                            let cut = idx + p + 1;
                            self.buf.drain(..cut);
                            self.in_think = false;
                        }
                        None => {
                            let keep = self.buf.split_off(idx);
                            self.buf = keep;
                            break;
                        }
                    },
                    None => {
                        let keep = if self.buf.len() > 8 { self.buf.len() - 8 } else { 0 };
                        let mut k = keep;
                        while k > 0 && !self.buf.is_char_boundary(k) {
                            k -= 1;
                        }
                        if k > 0 {
                            let rest = self.buf.split_off(k);
                            self.buf = rest;
                        }
                        break;
                    }
                }
            } else {
                match self.buf.find("<think") {
                    Some(idx) => {
                        let head: String = self.buf[..idx].to_string();
                        out.push_str(&head);
                        let tail = self.buf.split_off(idx);
                        self.buf = tail;
                        match self.buf.find('>') {
                            Some(p) => {
                                let cut = p + 1;
                                self.buf.drain(..cut);
                                self.in_think = true;
                            }
                            None => break,
                        }
                    }
                    None => {
                        let mut k = if self.buf.len() > 6 { self.buf.len() - 6 } else { 0 };
                        while k > 0 && !self.buf.is_char_boundary(k) {
                            k -= 1;
                        }
                        if k > 0 {
                            let head: String = self.buf[..k].to_string();
                            out.push_str(&head);
                            let rest = self.buf.split_off(k);
                            self.buf = rest;
                        }
                        break;
                    }
                }
            }
        }
        out
    }

    pub fn finish(&mut self) -> String {
        if self.in_think {
            self.buf.clear();
            return String::new();
        }
        std::mem::take(&mut self.buf)
    }
}

/// 引用标记过滤：MiMo 正文里的 `(citation:N)` / `citation:N` 不能出现在用户可见文本里。
/// 标记最约 20 字节；为了不把跨块标记切坏，flush 时保留「最后一个 ( 或 citation 起头」的尾部，
/// 保证起始在保留区内的标记一定被完整保留，结束/整段收尾时再统一剥掉。
pub struct CitationFilter {
    buf: String,
}

impl CitationFilter {
    pub fn new() -> Self {
        Self { buf: String::new() }
    }

    /// 返回可以安全输出的头部（已完成引用标记剥离）；尾部可能与标记有关的部分留在内部。
    pub fn feed(&mut self, s: &str) -> String {
        if s.is_empty() {
            return String::new();
        }
        self.buf.push_str(s);
        let k = self.safe_split();
        if k == 0 {
            return String::new();
        }
        let head: String = self.buf[..k].to_string();
        let rest = self.buf.split_off(k);
        self.buf = rest;
        strip_citations(&head)
    }

    /// 收尾：把剩下的一律剥掉（此处标记一定完整，剥离绝对准确）。
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.buf);
        strip_citations(&rest)
    }

    /// 找出「可以安全输出的头部长度」：尾部任何可能属于引用标记/引用组的字节都要留下。
    fn safe_split(&self) -> usize {
        let n = self.buf.len();
        let mut cut = n;
        // 1) 尾部可能只写了 citation 的前缀（cita / cit …）：整段留到下一块再判。
        let mark = "citation";
        for k in (1..=mark.len()).rev() {
            if self.buf.ends_with(&mark[..k]) {
                cut = cut.min(n - k);
                break;
            }
        }
        // 2) 尾部落在一个还没写完的引用组里（citation:12 / citation:1, citation:2 …）：
        //    从组头留，组头紧邻的括号也一起留，否则括号会先吐出去、后面剥不掉。
        if let Some(p) = self.citation_tail_start() {
            let mut start = p;
            if self.buf[..start].ends_with('(') {
                start -= 1;
            } else if self.buf[..start].ends_with('\u{ff08}') {
                start -= 3; // 全角括号占 3 字节
            }
            cut = cut.min(start);
        }
        // 3) 尾部有未闭合的括号：可能是引用组开头（也可能是正文），等括号闭合或下一块。
        for ch in ['(', '\u{ff08}'] {
            if let Some(p) = self.buf.rfind(ch) {
                let after = &self.buf[p..];
                if !after.contains(')') && !after.contains('\u{ff09}') {
                    cut = cut.min(p);
                }
            }
        }
        // 4) 尾部是「…）中」/「…）中：」这类引用引导语的前半段：等下一块一起剥。
        let mut t: &str = &self.buf;
        if let Some(x) = t.strip_suffix('\u{ff1a}') {
            t = x;
        }
        if let Some(x) = t.strip_suffix('\u{4e2d}') {
            t = x;
        }
        // 尾部若停在引用组的右括号上（「…(citation:6)」），后面的「中：」还没到，
        // 也必须把整组留住——否则本块会把「从(citation:6)」剥成「从」先吐出去。
        if (t.len() < self.buf.len() || t.ends_with(')') || t.ends_with('\u{ff09}'))
            && (t.ends_with(')') || t.ends_with('\u{ff09}'))
        {
            let p = t.rfind('(').max(t.rfind('\u{ff08}'));
            if let Some(p) = p {
                cut = cut.min(p);
            }
        }
        // 5) 尾部是「从」「从(」「从(cita」：后面很可能紧跟 (citation:N)中： 引用引导语，
        //    strip 会把「从…中：」整段收掉，所以先把这几个字节留住。
        if let Some(p) = self.buf.rfind('\u{4ece}') {
            let tail = &self.buf[p + 3..];
            let after_paren = tail
                .strip_prefix('(')
                .or_else(|| tail.strip_prefix('\u{ff08}'))
                .unwrap_or(tail);
            let ok = if after_paren.as_ptr() == tail.as_ptr() {
                tail.chars().all(|c| c == ' ')
            } else {
                "citation".starts_with(after_paren)
            };
            if ok {
                cut = cut.min(p);
            }
        }
        // 6) 切点若正好落在「从」或左括号之后，把它们一起留回缓冲：
        //    否则「从」会在本块当正文吐出，后面的 (citation:N)中： 被剥掉时就没有「从」可收，
        //    左括号同理会留下孤立的「()」。
        if cut > 0 && cut < n {
            if self.buf[..cut].ends_with('\u{4ece}') {
                cut -= 3;
            } else if self.buf[..cut].ends_with('(') {
                cut -= 1;
            } else if self.buf[..cut].ends_with('\u{ff08}') {
                cut -= 3;
            }
        }
        while cut > 0 && !self.buf.is_char_boundary(cut) {
            cut -= 1;
        }
        cut
    }

    /// 末尾是否落在一个「可能还没写完」的引用组里；是则返回组头（"citation" 的字节位置）。
    fn citation_tail_start(&self) -> Option<usize> {
        let p = self.buf.rfind("citation")?;
        if Self::tail_is_partial_group(&self.buf[p..]) {
            Some(p)
        } else {
            None
        }
    }

    /// `citation:12`、`citation:1, cit`、`citation:1, citation:23` 这类「还没写完」的组返回 true。
    fn tail_is_partial_group(rest: &str) -> bool {
        let Some(mut s) = rest.strip_prefix("citation:") else {
            return false;
        };
        loop {
            s = s.trim_start_matches(|c: char| c.is_ascii_digit());
            if s.is_empty() {
                return true;
            }
            // 引用标记后面常跟冒号（citation:4：），剥离时冒号与冒号后的空白要一起吃掉，
            // 所以「冒号后还没有正文」时必须留到下一块，否则前半段会先漏出去。
            if let Some(tail) = s.strip_prefix(':').or_else(|| s.strip_prefix('\u{ff1a}')) {
                if tail.trim().is_empty() {
                    return true;
                }
            }
            let Some(after) = s.strip_prefix(',').or_else(|| s.strip_prefix('\u{ff0c}')) else {
                return false;
            };
            s = after.trim_start();
            if s.is_empty() {
                return true;
            }
            match s.strip_prefix("citation:") {
                Some(next) => s = next,
                None => return !s.is_empty() && "citation".starts_with(s),
            }
        }
    }
}

/// 统一发一帧正文增量：首帧补 role，累计字符数，序列化成 data: 行。
fn emit_delta(
    out: &mut String,
    sent_role: &mut bool,
    out_chars: &mut usize,
    mk: &dyn Fn(Value, Option<&str>) -> String,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    if !*sent_role {
        out.push_str(&mk(json!({"role": "assistant", "content": ""}), None));
        *sent_role = true;
    }
    *out_chars += text.chars().count();
    out.push_str(&mk(json!({"content": text}), None));
}

fn block_event_data(block: &str) -> (String, String) {
    let mut ev = String::new();
    let mut data = String::new();
    for line in block.lines() {
        let t = line.trim();
        if let Some(x) = t.strip_prefix("event:") {
            ev = x.trim().to_string();
        } else if let Some(x) = t.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(x.trim());
        }
    }
    (ev, data)
}

fn frame_type(ev: &str, fv: &Value) -> String {
    if !ev.is_empty() {
        return ev.to_string();
    }
    fv.get("type").and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn usage_tokens(u: &Value) -> (i64, i64) {
    let pick = |k: &str| {
        u.get(k)
            .and_then(|x| x.as_i64())
            .or_else(|| u.get(k).and_then(|x| x.as_f64()).map(|f| f as i64))
            .unwrap_or(0)
    };
    (pick("promptTokens"), pick("completionTokens"))
}

async fn conv_save(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    hs: &[(String, String)],
    c: &MimoCred,
    conv: &str,
) -> Result<(), RelayError> {
    let url = format!(
        "{}/open-apis/chat/conversation/save?xiaomichatbot_ph={}",
        base_of(spec),
        urlencode(&c.ph_token)
    );
    let resp = apply(http.post(&url), hs)
        .json(&json!({"conversationId": conv, "title": "新对话", "type": "chat"}))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| er(502, format!("MiMo 建会话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let txt = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
    if status != 200 || v.get("code").and_then(|x| x.as_i64()) != Some(0) {
        let msg = v
            .get("msg")
            .or_else(|| v.get("message"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return Err(er(
            401,
            format!("MiMo 建会话失败（serviceToken/userId 可能失效）：{}", if msg.is_empty() { txt } else { msg }),
            ErrorClass::Auth,
        ));
    }
    Ok(())
}

fn bot_payload(conv: &str, query: &str, model: &str, temperature: f64, top_p: f64, think: bool) -> Value {
    let raw = uuid_like().replace('-', "");
    let msg_id: String = raw.chars().take(32).collect();
    json!({
        "msgId": msg_id,
        "conversationId": conv,
        "query": query,
        "isEditedQuery": false,
        "modelConfig": {
            "enableThinking": think,
            "webSearchStatus": "disabled",
            "model": model,
            "temperature": temperature,
            "topP": top_p,
        },
        "multiMedias": [],
    })
}

/// 非流式：建会话 → bot/chat → 聚合 SSE 正文。
pub async fn chat_nonstream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<Value, RelayError> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let (model, query, temperature, top_p, think) = read_request(&v);
    if query.trim().is_empty() {
        return Err(er(400, "messages 里没有可用文本", ErrorClass::Rejected));
    }
    let hs = headers(spec, &cred);
    let conv = uuid_like();
    conv_save(http, spec, &hs, &cred, &conv).await?;
    let url = format!(
        "{}/open-apis/bot/chat?xiaomichatbot_ph={}",
        base_of(spec),
        urlencode(&cred.ph_token)
    );
    let payload = bot_payload(&conv, &query, &model, temperature, top_p, think);
    let resp = apply(http.post(&url), &hs)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| er(502, format!("MiMo 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    let txt = resp.text().await.unwrap_or_default();
    if status != 200 {
        let class = if status == 401 || status == 403 { ErrorClass::Auth } else { ErrorClass::Transient };
        return Err(er(status, txt, class));
    }

    let mut buf = txt;
    let mut filter = ThinkFilter::new();
    let mut content = String::new();
    let mut usage: Option<Value> = None;
    while let Some((pos, sep)) = crate::find_frame_end(&buf) {
        let block: String = buf.drain(..pos + sep).collect();
        let (ev, data) = block_event_data(&block);
        if data.is_empty() {
            continue;
        }
        let Ok(fv) = serde_json::from_str::<Value>(&data) else { continue };
        match frame_type(&ev, &fv).as_str() {
            "message" | "text" => {
                if let Some(c) = fv.get("content").and_then(|x| x.as_str()) {
                    content.push_str(&filter.feed(&c.replace('\u{0}', "")));
                }
            }
            "usage" => {
                if let Some(u) = fv.get("usage") {
                    usage = Some(u.clone());
                }
            }
            _ => {}
        }
    }
    content.push_str(&filter.finish());
    let content = strip_citations(&content);
    if content.trim().is_empty() {
        return Err(er(
            401,
            "MiMo 未返回正文（serviceToken/userId/xiaomichatbot_ph 可能已失效，请重新登录 aistudio.xiaomimimo.com 复制整段 Cookie）",
            ErrorClass::Auth,
        ));
    }
    let (mut tin, mut tout) = match &usage {
        Some(u) => usage_tokens(u),
        None => (0, 0),
    };
    if tin == 0 && tout == 0 {
        tin = ((body.len() / 4).max(1)) as i64;
        tout = ((content.chars().count() / 2).max(1)) as i64;
    }
    Ok(json!({
        "id": format!("chatcmpl-{conv}"),
        "object": "chat.completion",
        "created": secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout},
    }))
}

/// 流式：把 MiMo 的 event/message 帧改写成 OpenAI chunk。
pub async fn chat_stream(
    http: &reqwest::Client,
    spec: &ProviderSpec,
    secret: &str,
    body: &[u8],
) -> Result<
    (
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        mpsc::Receiver<StreamStat>,
    ),
    RelayError,
> {
    let cred = parse_cred(secret).map_err(|e| er(401, e, ErrorClass::Auth))?;
    let v: Value = serde_json::from_slice(body).unwrap_or_else(|_| json!({}));
    let (model, query, temperature, top_p, think) = read_request(&v);
    if query.trim().is_empty() {
        return Err(er(400, "messages 里没有可用文本", ErrorClass::Rejected));
    }
    let hs = headers(spec, &cred);
    let conv = uuid_like();
    conv_save(http, spec, &hs, &cred, &conv).await?;
    let url = format!(
        "{}/open-apis/bot/chat?xiaomichatbot_ph={}",
        base_of(spec),
        urlencode(&cred.ph_token)
    );
    let payload = bot_payload(&conv, &query, &model, temperature, top_p, think);
    let resp = apply(http.post(&url), &hs)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| er(502, format!("MiMo 对话请求失败：{e}"), ErrorClass::Transient))?;
    let status = resp.status().as_u16();
    if status != 200 {
        let txt = resp.text().await.unwrap_or_default();
        let class = if status == 401 || status == 403 { ErrorClass::Auth } else { ErrorClass::Transient };
        return Err(er(status, txt, class));
    }

    let (tx, rx) = mpsc::channel::<StreamStat>(1);
    let (bytes_tx, bytes_rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let in_bytes = body.len();
    tokio::spawn(async move {
        let rid = format!("chatcmpl-{}", uuid_like());
        let created = secs();
        let model_out = model.clone();
        let mk = move |delta: Value, finish: Option<&str>| -> String {
            format!(
                "data: {}\n\n",
                json!({
                    "id": rid,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_out,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                })
            )
        };
        let mut stat = StreamStat::default();
        let mut out_chars: usize = 0;
        let mut buf = String::new();
        let mut dec = crate::Utf8Decoder::new();
        let mut filter = ThinkFilter::new();
        let mut cite = CitationFilter::new();
        let mut sent_role = false;
        let mut usage: Option<Value> = None;
        let mut stream = resp.bytes_stream();
        while let Some(item) = stream.next().await {
            let chunk = match item {
                Ok(b) => b,
                Err(e) => {
                    stat.error = Some(ErrorClass::Transient);
                    stat.error_msg = format!("mid-stream: {e}");
                    break;
                }
            };
            buf.push_str(&dec.push(&chunk));
            let mut out = String::new();
            while let Some((pos, sep)) = crate::find_frame_end(&buf) {
                let block: String = buf.drain(..pos + sep).collect();
                let (ev, data) = block_event_data(&block);
                if data.is_empty() {
                    continue;
                }
                let Ok(fv) = serde_json::from_str::<Value>(&data) else { continue };
                match frame_type(&ev, &fv).as_str() {
                    "message" | "text" => {
                        let Some(c) = fv.get("content").and_then(|x| x.as_str()) else { continue };
                        let raw = filter.feed(&c.replace('\u{0}', ""));
                        let text = cite.feed(&raw);
                        emit_delta(&mut out, &mut sent_role, &mut out_chars, &mk, &text);
                    }
                    "usage" => {
                        if let Some(u) = fv.get("usage") {
                            usage = Some(u.clone());
                        }
                    }
                    _ => {}
                }
            }
            if !out.is_empty() && bytes_tx.send(Ok(Bytes::from(out))).await.is_err() {
                break;
            }
        }
        let mut tail_text = cite.feed(&filter.finish());
        tail_text.push_str(&cite.finish());
        let mut tail = String::new();
        emit_delta(&mut tail, &mut sent_role, &mut out_chars, &mk, &tail_text);
        if stat.error.is_none() && !sent_role {
            stat.error = Some(ErrorClass::Auth);
            stat.error_msg =
                "MiMo 未返回正文（serviceToken/userId/xiaomichatbot_ph 可能已失效，请重新登录 aistudio.xiaomimimo.com 复制整段 Cookie）"
                    .into();
        }
        if let Some(ec) = stat.error {
            let etype = match ec {
                ErrorClass::Rate => "rate_limit_error",
                ErrorClass::Auth => "authentication_error",
                ErrorClass::Quota => "insufficient_quota",
                _ => "api_error",
            };
            let msg = stat.error_msg.replace('"', "'");
            tail.push_str(&format!(
                "data: {}\n\n",
                json!({"error": {"message": msg, "type": etype}})
            ));
        } else {
            tail.push_str(&mk(json!({}), Some("stop")));
            stat.finish_reason = Some("stop".into());
        }
        tail.push_str("data: [DONE]\n\n");
        let _ = bytes_tx.send(Ok(Bytes::from(tail))).await;
        let (mut tin, mut tout) = match &usage {
            Some(u) => usage_tokens(u),
            None => (0, 0),
        };
        if tin == 0 && tout == 0 {
            let (a, b) = crate::estimate_stream_tokens(in_bytes, out_chars);
            tin = a;
            tout = b;
        }
        stat.tokens_in = tin;
        stat.tokens_out = tout;
        let _ = tx.send(stat).await;
    });
    let stream = tokio_stream::wrappers::ReceiverStream::new(bytes_rx);
    Ok((Box::pin(stream), rx))
}

/// 验活：GET conversation/list（code=0 即凭据有效）。
pub async fn verify(http: &reqwest::Client, spec: &ProviderSpec, secret: &str) -> bool {
    let Ok(c) = parse_cred(secret) else {
        return false;
    };
    let url = format!(
        "{}/open-apis/chat/conversation/list?xiaomichatbot_ph={}&pageNum=1&pageSize=1",
        base_of(spec),
        urlencode(&c.ph_token)
    );
    let hs = headers(spec, &c);
    match apply(http.get(&url), &hs)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => {
            if !r.status().is_success() {
                return false;
            }
            let txt = r.text().await.unwrap_or_default();
            let v: Value = serde_json::from_str(&txt).unwrap_or_else(|_| json!({}));
            v.get("code").and_then(|x| x.as_i64()) == Some(0)
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;

    const SSE: &str = "event: message\ndata: {\"content\":\"\u{4f60}\"}\n\n\
event: message\ndata: {\"content\":\"\u{597d}(citation:1)\"}\n\n\
event: usage\ndata: {\"usage\":{\"promptTokens\":3,\"completionTokens\":2}}\n\n";

    async fn mock() -> String {
        let app = Router::new()
            .route(
                "/open-apis/chat/conversation/save",
                post(|| async { axum::Json(json!({"code": 0})) }),
            )
            .route(
                "/open-apis/bot/chat",
                post(|| async {
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(SSE))
                        .unwrap()
                }),
            )
            .route(
                "/open-apis/chat/conversation/list",
                get(|| async { axum::Json(json!({"code": 0, "data": {"dataList": []}})) }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}")
    }

    fn spec_for(base: &str) -> ProviderSpec {
        ProviderSpec {
            id: "mimo".into(),
            name: "MiMo".into(),
            enabled: true,
            auth: aigw_registry::AuthKind::Cookie,
            login_methods: Vec::new(),
            base_url: base.to_string(),
            format: "openai".into(),
            models: vec!["MiMo-V2.5".into()],
            models_policy: aigw_registry::ModelsPolicy::Static,
            stream_dialect: aigw_registry::StreamDialect::SseDelta,
            headers: std::collections::BTreeMap::new(),
            chat_path: String::new(),
            models_path: String::new(),
            models_filter: String::new(),
            upstream_kind: "mimo".into(),
            signer: String::new(),
            biz_err: vec![],
            hosts: vec![],
            login_url: String::new(),
            notes: String::new(),
            chat_supported: true,
            region: "global".into(),
        }
    }

    const SECRET: &str = "serviceToken=st-1; userId=uid-1; xiaomichatbot_ph=ph-1";

    fn req_body(model: &str) -> Vec<u8> {
        json!({
            "model": model,
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hello"}
            ]
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn parse_cred_accepts_cookie_and_json() {
        let c = parse_cred(SECRET).unwrap();
        assert_eq!(c.service_token, "st-1");
        assert_eq!(c.user_id, "uid-1");
        assert_eq!(c.ph_token, "ph-1");
        let j = parse_cred("{\"service_token\":\"a\",\"user_id\":\"b\",\"ph_token\":\"c\"}").unwrap();
        assert_eq!(j.ph_token, "c");
        assert!(parse_cred("serviceToken=x; userId=y").is_err());
    }

    #[test]
    fn think_filter_splits_tags() {
        let mut f = ThinkFilter::new();
        let mut out = f.feed("<thi");
        out.push_str(&f.feed("nk>reason"));
        out.push_str(&f.feed("ing</think"));
        out.push_str(&f.feed(">hello"));
        out.push_str(&f.finish());
        assert_eq!(out, "hello");
    }

    #[test]
    fn strip_citations_forms() {
        assert_eq!(strip_citations("hi(citation:1)there"), "hithere");
        assert_eq!(strip_citations("a citation:2 b"), "a  b");
        assert_eq!(strip_citations("x从(citation:3)中：y"), "xy");
        assert_eq!(strip_citations("a(citation:1, citation:2)b"), "ab");
        assert_eq!(strip_citations("从(citation:1, citation:2)中：好"), "好");
        // 全角括号（MiMo 中文正文里同样出现）
        assert_eq!(strip_citations("（citation:1）开头"), "开头");
        assert_eq!(strip_citations("（citation:1, citation:2）并列"), "并列");
        // 松散形态后面跟冒号，不能留下孤立的冒号
        assert_eq!(strip_citations("参考 citation:1： 内容"), "参考 内容");
    }

    #[test]
    fn citation_filter_never_leaks_citation_across_chunks() {
        let texts = [
            "你好(citation:1)世界，今天天气不错(citation:12)结束",
            "（citation:1）开头，末尾（citation:2, citation:3）结束",
            "参考 citation:4： 内容，再一段 citation:5： 尾巴",
            "从(citation:6)中：引用来源，正文继续",
        ];
        for full in texts {
            let want = strip_citations(full);
            let chars: Vec<char> = full.chars().collect();
            for cut in 0..=chars.len() {
                let (a, b) = chars.split_at(cut);
                let a: String = a.iter().collect();
                let b: String = b.iter().collect();
                let mut f = CitationFilter::new();
                let mut got = f.feed(&a);
                got.push_str(&f.feed(&b));
                got.push_str(&f.finish());
                assert_eq!(got, want, "「{full}」第一块切在 {cut} 字符处时引用剥离结果不一致");
            }
        }
    }

    #[tokio::test]
    async fn mimo_nonstream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let v = chat_nonstream(&http, &spec, SECRET, &req_body("MiMo-V2.5"))
            .await
            .unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "\u{4f60}\u{597d}");
        assert_eq!(v["usage"]["prompt_tokens"], 3);
        assert_eq!(v["usage"]["completion_tokens"], 2);
        assert_eq!(v["model"], "MiMo-V2.5");
    }

    #[tokio::test]
    async fn mimo_stream_via_mock() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        let (mut s, mut rx) = chat_stream(&http, &spec, SECRET, &req_body("MiMo-V2.5"))
            .await
            .unwrap();
        let mut all = String::new();
        while let Some(item) = s.next().await {
            let b = item.unwrap();
            all.push_str(std::str::from_utf8(&b).unwrap());
        }
        assert!(all.contains("\u{4f60}"), "流里应含首字: {all}");
        assert!(all.contains("\u{597d}"), "流里应含次字");
        assert!(!all.contains("citation:"), "引用标记应被剥掉: {all}");
        assert!(all.contains("[DONE]"));
        let stat = rx.try_recv().unwrap();
        assert!(stat.error.is_none(), "不应有错误: {:?}", stat.error_msg);
        assert_eq!(stat.tokens_in, 3);
        assert_eq!(stat.tokens_out, 2);
    }

    #[tokio::test]
    async fn mimo_verify_and_bad_cred() {
        let base = mock().await;
        let spec = spec_for(&base);
        let http = reqwest::Client::new();
        assert!(verify(&http, &spec, SECRET).await);
        assert!(!verify(&http, &spec, "{}").await);
        assert!(!verify(&http, &spec, "").await);
    }
}
