import io

p = 'v1.rs'
s = io.open(p, encoding='utf-8').read()

# 1) 把错位的 instrument 属性从 helper 上方摘掉，挂回 chat_completions
bad = '''#[tracing::instrument(skip(st, headers, body))]
/// 号池治理中继执行器（2026-10-16，S3；治理规则见 `server/pool.rs`）。'''
good = '''/// 号池治理中继执行器（2026-10-16，S3；治理规则见 `server/pool.rs`）。'''
assert bad in s, 'attr anchor'
s = s.replace(bad, good, 1)

bad2 = '''pub async fn chat_completions(
    State(st): State<AppState>,'''
good2 = '''#[tracing::instrument(skip(st, headers, body))]
pub async fn chat_completions(
    State(st): State<AppState>,'''
assert bad2 in s, 'cc anchor'
s = s.replace(bad2, good2, 1)

# 2) relay_pooled：FnMut(String) → FnMut(&str)
s = s.replace('''    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, (i64, String)>>,''',
              '''    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = Result<T, (i64, String)>>,''', 1)
s = s.replace('        let r = attempt(a.secret.clone()).await;',
              '        let r = attempt(&a.secret).await;')

# 3) 调用点去掉多余的 &
for old, new in [
    ('|sec| relay::web::chat(pid, &msgs, &sec, dm, 180)', '|sec| relay::web::chat(pid, &msgs, sec, dm, 180)'),
    ('|sec| relay::doubao::chat_stream(&msgs, &sec, dm, 180)', '|sec| relay::doubao::chat_stream(&msgs, sec, dm, 180)'),
    ('|sec| relay::doubao::chat(&msgs, &sec, dm, 180)', '|sec| relay::doubao::chat(&msgs, sec, dm, 180)'),
    ('|sec| relay::yuanbao::chat_stream(&msgs, &sec, dm, 180)', '|sec| relay::yuanbao::chat_stream(&msgs, sec, dm, 180)'),
    ('|sec| relay::yuanbao::chat(&msgs, &sec, dm, 180)', '|sec| relay::yuanbao::chat(&msgs, sec, dm, 180)'),
    ('|sec| relay::loomy::chat_stream(&msgs, &sec, dm, 180)', '|sec| relay::loomy::chat_stream(&msgs, sec, dm, 180)'),
    ('|sec| relay::loomy::chat(&msgs, &sec, dm, 180)', '|sec| relay::loomy::chat(&msgs, sec, dm, 180)'),
    ('|sec| relay::native::chat_stream(&spec, &body, &sec, 180)', '|sec| relay::native::chat_stream(&spec, &body, sec, 180)'),
    ('|sec| relay::native::chat(&spec, &body, &sec, 180)', '|sec| relay::native::chat(&spec, &body, sec, 180)'),
    ('relay::doubao::chat_stream(&msgs, &sec, &model, 180)', 'relay::doubao::chat_stream(&msgs, sec, &model, 180)'),
    ('relay::doubao::chat(&msgs, &sec, &model, 180)', 'relay::doubao::chat(&msgs, sec, &model, 180)'),
    ('relay::native::chat_stream(&spec, &openai_bytes, &sec, 180)', 'relay::native::chat_stream(&spec, &openai_bytes, sec, 180)'),
    ('relay::native::chat(&spec, &openai_bytes, &sec, 180)', 'relay::native::chat(&spec, &openai_bytes, sec, 180)'),
]:
    if old in s:
        s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('v1 fixed; &sec left =', s.count('&sec'))

# ── pool.rs：补 Value 导入 ──
p2 = '../pool.rs'
s2 = io.open(p2, encoding='utf-8').read()
s2 = s2.replace('''    pub fn snapshot(&self) -> serde_json::Value {
        use serde_json::json;''', '''    pub fn snapshot(&self) -> serde_json::Value {
        use serde_json::{json, Value};''', 1)
io.open(p2, 'w', encoding='utf-8', newline='\n').write(s2)
print('pool fixed')
