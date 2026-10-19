import io

# ── relay/mod.rs：新增 out_client（超时四项分设） ──
p = 'mod.rs'
s = io.open(p, encoding='utf-8').read()

anchor = '''/// 执行中继计划，返回（流式或错误）响应。'''
helper = '''/// 出站客户端（2026-10-16 S3：超时**四项分设**）。
///
/// 原来的实现是一个 `.timeout(N)` 管到底 —— 对流式响应那是**总时长**，
/// 一个超过 N 秒的长回答会被拦腰砍断（客户端只收到半截 SSE）。现在：
///
/// | 项 | 值 | 说明 |
/// |---|---|---|
/// | connect | 10s | 建连（`connect_timeout`） |
/// | 首包 TTFB | 90s | `read_timeout` 在等响应头期间同样生效 |
/// | 包间 idle | 90s | `read_timeout` 每次成功读取后重置 |
/// | 总时长 | 非流式 180s / **流式不设** | 长会话可能持续数分钟 |
///
/// `no_proxy` 只为网页反代（`web`）开启：那类请求带完整登录 Cookie，
/// 不能泄露给系统代理；原生中继走默认代理策略（与改动前一致）。
pub fn out_client(stream: bool, total_s: u64, no_proxy: bool) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(90));
    if no_proxy {
        b = b.no_proxy();
    }
    if !stream {
        b = b.timeout(std::time::Duration::from_secs(total_s));
    }
    b.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 执行中继计划，返回（流式或错误）响应。'''
assert anchor in s
s = s.replace(anchor, helper, 1)

# execute()：原来是 `.timeout(25s)` 总时长管流式 → 改成流式语义（不设总时长）
old = '''    let client = reqwest::Client::new();
    let mut builder = client
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(std::time::Duration::from_secs(RELAY_TIMEOUT));'''
new = '''    // ★ S3：流式语义 —— connect 10s / idle 90s / 不设总时长
    let mut builder = out_client(true, RELAY_TIMEOUT, false)
        .post(&url)
        .header(header::CONTENT_TYPE, "application/json");'''
assert old in s
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('mod.rs ok')

# ── web.rs：http_client 委托 out_client（保留 no_proxy） ──
p = 'web.rs'
s = io.open(p, encoding='utf-8').read()
old = '''fn http_client(timeout: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .expect("reqwest client 构建失败")
}'''
new = '''// 网页反代全是非流式（post_collect 收完整正文再转 SSE），
// 但超时仍按四项分设给：总时长 + 建连 + 读取各自独立。
fn http_client(timeout: u64) -> reqwest::Client {
    super::out_client(false, timeout, true)
}'''
assert old in s
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('web.rs ok')

# ── native.rs ──
p = 'native.rs'
s = io.open(p, encoding='utf-8').read()
old_chat = '''    let url = url(spec);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout));'''
new_chat = '''    let url = url(spec);
    // 非流式：总时长 timeout（+ connect 10s / read 90s，见 out_client）
    let mut req = super::out_client(false, timeout, false).post(&url);'''
assert old_chat in s
s = s.replace(old_chat, new_chat, 1)
old_st = '''    let url = url(spec);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout));'''
new_st = '''    let url = url(spec);
    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let mut req = super::out_client(true, 0, false).post(&url);'''
assert old_st in s
s = s.replace(old_st, new_st, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('native.rs ok')

# ── doubao.rs ──
p = 'doubao.rs'
s = io.open(p, encoding='utf-8').read()
old = '''    let client = reqwest::Client::new();
    let mut req = client.post(&url).timeout(std::time::Duration::from_secs(timeout));'''
new = '''    let mut req = super::out_client(false, timeout, false).post(&url);'''
assert old in s
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('doubao chat ok')
# chat_stream 的客户端（在 axum import 之后）
i = s.index('pub async fn chat_stream(')
head, tail = s[:i], s[i:]
old2 = '''    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout));'''
if old2 in tail:
    tail = tail.replace(old2, '''    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let mut req = super::out_client(true, 0, false).post(&url);''', 1)
    s = head + tail
    io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
    print('doubao chat_stream ok')
else:
    print('!! doubao chat_stream pattern not found')

# ── loomy.rs ──
p = 'loomy.rs'
s = io.open(p, encoding='utf-8').read()
old = '''    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());'''
new = '''    let client = super::out_client(false, timeout, false);'''
assert old in s
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('loomy chat ok')
i = s.index('pub async fn chat_stream(')
head, tail = s[:i], s[i:]
old2 = '''    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());'''
if old2 in tail:
    tail = tail.replace(old2, '''    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let client = super::out_client(true, 0, false);''', 1)
    s = head + tail
    io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
    print('loomy chat_stream ok')
else:
    print('!! loomy chat_stream pattern not found')

# ── yuanbao.rs ──
p = 'yuanbao.rs'
s = io.open(p, encoding='utf-8').read()
old = '''    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout))
        .header("content-type", "application/json");'''
new = '''    let mut req = super::out_client(false, timeout, false)
        .post(&url)
        .header("content-type", "application/json");'''
assert old in s
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('yuanbao chat ok')
i = s.index('pub async fn chat_stream(')
head, tail = s[:i], s[i:]
old2 = '''    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout))
        .header("content-type", "application/json");'''
if old2 in tail:
    tail = tail.replace(old2, '''    let _ = timeout; // 流式：不设总时长（S3 四项分设）
    let mut req = super::out_client(true, 0, false)
        .post(&url)
        .header("content-type", "application/json");''', 1)
    s = head + tail
    io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
    print('yuanbao chat_stream ok')
else:
    print('!! yuanbao chat_stream pattern not found')
