import io
import re

p = 'v1.rs'
s = io.open(p, encoding='utf-8').read()

# ── 1) 用宏替换 async fn relay_pooled ──
start = s.index('/// 号池治理中继执行器（2026-10-16，S3')
end_marker = '''    Err(last_err.unwrap_or((
        503,
        format!("{} 的可用账号都在冷却或在途，请稍后重试", platform),
    )))
}
'''
end = s.index(end_marker) + len(end_marker)

macro = r'''/// 号池治理中继宏（2026-10-16，S3；治理规则见 `server/pool.rs`）。
///
/// **为什么是宏而不是 `async fn` + 闭包**：闭包返回的 future 借用入参 `&str`，
/// 而 `FnMut(&str) -> Fut` 要求 `Fut` 与入参生命周期无关（Rust 没有 lending
/// closure）—— 写成函数必然撞 E0515。宏把循环体原地展开，借用检查自然通过。
///
/// 行为：按「紧迫度 → RecentPick → last_used」排序，跳过冷却 / WAF 拉黑 /
/// 在途已满的账号；成功即返回并 `mark_ok`；失败 `mark_fail`（401/403 账号级
/// 拉黑）后换下一个（单次请求最多 3 个）；全部被挡住 → 半开最紧迫的一个。
/// ★ 只读内存，绝不打上游余额/探测接口。
///
/// 用法（`$call` = 用 `$sec` 打一次上游的 async 表达式）：
/// ```ignore
/// match pooled_attempt!(&st, pid, dm, |sec| relay::web::chat(pid, &msgs, sec, dm, 180)) {
///     Ok(payload) => { /* 成功 */ }
///     Err((status, msg)) => { /* 失败 */ }
/// }
/// ```
macro_rules! pooled_attempt {
    ($st:expr, $pid:expr, $model:expr, |$sec:ident| $call:expr) => {{
        let __st: &crate::server::state::AppState = $st;
        let __pid: &str = $pid;
        let __model: &str = $model;
        let mut __ranked = __st.inner.login.accounts_ranked(__pid);
        if __ranked.is_empty() {
            Err((503i64, format!("{} 需要先在「接入源」登录获取凭据", __pid)))
        } else {
            // RecentPick：紧迫度相同（都没到期数据）时让给窗口内被选次数少的
            __ranked.sort_by(|a, b| {
                a.urgency
                    .cmp(&b.urgency)
                    .then_with(|| {
                        __st
                            .inner
                            .pool
                            .recent_count(__pid, &a.id, __model)
                            .cmp(&__st.inner.pool.recent_count(__pid, &b.id, __model))
                    })
                    .then_with(|| a.last_used.cmp(&b.last_used))
            });
            let mut __last: Option<(i64, String)> = None;
            let mut __tried = 0usize;
            let mut __out: Result<_, (i64, String)> = Err((503i64, String::new()));
            for __a in &__ranked {
                if __tried >= 3 {
                    break;
                }
                if __st.inner.pool.is_blocked(__pid, &__a.id, __model) {
                    continue;
                }
                let Some(__lease) = __st.inner.pool.try_acquire(__pid, &__a.id, __model) else {
                    continue; // 在途已满，换号
                };
                __tried += 1;
                __st.inner.pool.note_pick(__pid, &__a.id, __model);
                let $sec: &str = __a.secret.as_str();
                let __r = $call.await;
                drop(__lease);
                match __r {
                    Ok(v) => {
                        __st.inner.pool.mark_ok(&__a.id, __model);
                        __out = Ok(v);
                        break;
                    }
                    Err((status, msg)) => {
                        __st.inner.pool.mark_fail(__pid, &__a.id, __model, status as u16);
                        __last = Some((status, msg));
                    }
                }
            }
            // 半开：全部被治理状态挡住时，放行最紧迫的一个（不清冷却，靠本次结果说话）
            if __tried == 0 {
                let __a = &__ranked[0];
                if let Some(__lease) = __st.inner.pool.try_acquire(__pid, &__a.id, __model) {
                    __st.inner.pool.note_pick(__pid, &__a.id, __model);
                    let $sec: &str = __a.secret.as_str();
                    let __r = $call.await;
                    drop(__lease);
                    match __r {
                        Ok(v) => {
                            __st.inner.pool.mark_ok(&__a.id, __model);
                            __out = Ok(v);
                        }
                        Err((status, msg)) => {
                            __st.inner.pool.mark_fail(__pid, &__a.id, __model, status as u16);
                            __last = Some((status, msg));
                        }
                    }
                }
            }
            if __out.is_ok() {
                __out
            } else {
                Err(__last.unwrap_or((
                    503i64,
                    format!("{} 的可用账号都在冷却或在途，请稍后重试", __pid),
                )))
            }
        }
    }};
}

'''
s = s[:start] + macro + s[end:]

# ── 2) 调用点：relay_pooled(...).await → pooled_attempt!(...) ──
s = re.sub(
    r'relay_pooled\((&st), ([^,]+), ([^,]+), \|sec\| ([^)]+)\)\s*\n?\s*\.await',
    r'pooled_attempt!(\1, \2, \3, |sec| \4)',
    s,
)
# messages handler 里的多行闭包形式
s = s.replace('''match relay_pooled(&st, "apk-doubao", &model, |sec| {
                relay::doubao::chat_stream(&msgs, sec, &model, 180)
            })
            .await
            {''', '''match pooled_attempt!(&st, "apk-doubao", &model, |sec| {
                relay::doubao::chat_stream(&msgs, sec, &model, 180)
            }) {''')
s = s.replace('''return match relay_pooled(&st, "apk-doubao", &model, |sec| {
            relay::doubao::chat(&msgs, sec, &model, 180)
        })
        .await
        {''', '''return match pooled_attempt!(&st, "apk-doubao", &model, |sec| {
            relay::doubao::chat(&msgs, sec, &model, 180)
        }) {''')
s = s.replace('''match relay_pooled(&st, pid, &model, |sec| {
                        relay::native::chat_stream(&spec, &openai_bytes, sec, 180)
                    })
                    .await
                    {''', '''match pooled_attempt!(&st, pid, &model, |sec| {
                        relay::native::chat_stream(&spec, &openai_bytes, sec, 180)
                    }) {''')
s = s.replace('''return match relay_pooled(&st, pid, &model, |sec| {
                    relay::native::chat(&spec, &openai_bytes, sec, 180)
                })
                .await
                {''', '''return match pooled_attempt!(&st, pid, &model, |sec| {
                    relay::native::chat(&spec, &openai_bytes, sec, 180)
                }) {''')

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
left = s.count('relay_pooled')
print('relay_pooled left =', left, '| pooled_attempt! count =', s.count('pooled_attempt!('))
