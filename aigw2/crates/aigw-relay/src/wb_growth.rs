//! WorkBuddy 成长任务自动完成（对齐 workbuddy2api-panel internal/panel/autotask.go）。
//!
//! 协议来源：2026-10 抓包实测（workbuddy2api-panel 三账号验证）。
//! 核心机制：成长任务大部分不要求真实对话，只要求**行为事件上报**——
//! 以桌面客户端指纹（`extName=workbuddy-desktop` + UA `WorkBuddy/5.5.6`）
//! 向 `POST {base}/v2/report` 提交事件链即可点亮。
//!
//! 端点：
//! - `GET  /v2/activity/growth/tasks`                 任务清单（含完成态）
//! - `POST /v2/activity/growth/tasks/accept`          接受任务 {taskCodes}
//! - `POST {web}/activity/growth/tasks/{code}/claim`  领取奖励
//! - `POST /v2/report`                                行为事件上报（桌面指纹）
//!
//! 覆盖任务（17 个，唯 school_season 需小程序环境与夜间窗口，故不作为默认路径）：
//! first_buddy / create_canvas / chat_5 / Model_chat_GLM5.2 / RichMeow_Chat /
//! Buddy_App / Buddy_App_QQ / automation_1 / Library_read / template_5 /
//! playbook_prompt / expert_5 / Expert_team_use_3 / Hp_Appearance /
//! Expert_lighthouse / skill_1 / Sequential_Tasks_1-7

use reqwest::Client;
use serde_json::{json, Value};

/// 桌面客户端 UA（WorkBuddy 5.5.6 内嵌 CLI 2.137.1，实测）。
const DESKTOP_UA: &str = "WorkBuddy/5.5.6 WorkBuddy/5.5.6 CLI/2.137.1";
const REPORT_PATH: &str = "/v2/report";
const TASKS_LIST_PATH: &str = "/v2/activity/growth/tasks";
const TASKS_ACCEPT_PATH: &str = "/v2/activity/growth/tasks/accept";

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 稳定派生 id（同一账号同一 salt 恒定，像真客户端的 machineId/sessionId）。
fn derive_id(uid: &str, salt: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(uid.as_bytes());
    h.update(b"|");
    h.update(salt.as_bytes());
    let d = h.finalize();
    // 32 hex 字符（与客户端 machineId 形态一致）
    d.iter().take(16).map(|b| format!("{:02x}", b)).collect()
}

/// 桌面指纹（注入每个事件，业务字段可覆盖）。
fn desktop_fingerprint(uid: &str, nickname: &str) -> Value {
    let now = unix_ms();
    json!({
        "timezone": "Asia/Shanghai",
        "reportDelay": 2000,
        "userId": uid,
        "username": nickname,
        "userNickname": nickname,
        "product": "SaaS",
        "releaseDate": 1789036585355i64,
        "commit": "5f9692923c93033111c51ad7b003eb80204a9b75",
        "ideName": "WorkBuddy",
        "ideType": "WorkBuddy",
        "ideVersion": "5.5.6",
        "machineId": derive_id(uid, "machine"),
        "sessionId": derive_id(uid, "session"),
        "extName": "workbuddy-desktop",
        "extVersion": "5.5.6",
        "os": "win32",
        "arch": "x64",
        "osVersion": "10.0.26220",
        "cpuCores": 20,
        "memorySize": 24,
        "timestamp": now,
        "presentAt": now,
    })
}

/// 以桌面指纹批量上报事件链（`POST /v2/report`）。
async fn report_desktop_events(
    http: &Client,
    base: &str,
    token: &str,
    uid: &str,
    nickname: &str,
    events: &[Value],
) -> Result<(), String> {
    if events.is_empty() {
        return Err("没有事件可上报".into());
    }
    let fp = desktop_fingerprint(uid, nickname);
    let mut arr: Vec<Value> = Vec::with_capacity(events.len());
    for ev in events {
        let mut m = fp.clone();
        if let (Some(dst), Some(src)) = (m.as_object_mut(), ev.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        arr.push(m);
    }
    let resp = http
        .post(format!("{base}{REPORT_PATH}"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json;charset=UTF-8")
        .header("User-Agent", DESKTOP_UA)
        .header("X-Domain", base)
        .header("X-Product", "SaaS")
        .header("X-Request-ID", format!("{}{}", derive_id(uid, "req"), unix_ms() % 1_000_000))
        .header("X-User-Id", uid)
        .json(&arr)
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("report HTTP {status}: {}", text.chars().take(160).collect::<String>()));
    }
    Ok(())
}

/// 轻量会话/请求/message id（客户端形态）。
fn conv_id(uid: &str, salt: &str) -> String {
    format!("conv_{}", derive_id(uid, salt))
}
fn msg_id(uid: &str, salt: &str) -> String {
    format!("msg_{}", derive_id(uid, salt))
}

/// 一次完整「桌面端成功对话」事件链（点亮 RichMeow_Chat）。
fn chat_sequence(_uid: &str, conv: &str, req: &str, msg: &str, model: &str) -> Vec<Value> {
    let ts = unix_ms();
    vec![
        json!({"eventCode":"agent_task_created","conversationId":conv,"requestId":req,"timestamp":ts}),
        json!({"eventCode":"chat_message_send","conversationId":conv,"requestId":req,"messageId":msg,"content":"你好","timestamp":ts+100}),
        json!({"eventCode":"chat_request_send","conversationId":conv,"requestId":req,"modelId":model,"modelName":model,"timestamp":ts+200}),
        json!({"eventCode":"chat_message_response","conversationId":conv,"requestId":req,"messageId":format!("{msg}_r"),"isSuccessful":true,"timestamp":ts+900}),
        json!({"eventCode":"chat_message_status","conversationId":conv,"requestId":req,"messageId":format!("{msg}_r"),"status":"finished","timestamp":ts+1000}),
        json!({"eventCode":"chat_request_response","conversationId":conv,"requestId":req,"isSuccessful":true,"finishReason":"stop","timestamp":ts+1100}),
    ]
}

/// Buddy App 五连事件（点亮 Buddy_App / Buddy_App_QQ）。
fn buddy_app_sequence(buddy_id: &str, buddy_name: &str) -> Vec<Value> {
    let ts = unix_ms();
    vec![
        json!({"eventCode":"buddyapp_discover","buddyId":buddy_id,"buddyName":buddy_name,"timestamp":ts}),
        json!({"eventCode":"buddyapp_show","buddyId":buddy_id,"buddyName":buddy_name,"timestamp":ts+100}),
        json!({"eventCode":"buddyapp_enter","buddyId":buddy_id,"buddyName":buddy_name,"timestamp":ts+200}),
        json!({"eventCode":"buddyapp_auth_confirm","buddyId":buddy_id,"buddyName":buddy_name,"timestamp":ts+300}),
        json!({"eventCode":"buddyapp_bind_skip","buddyId":buddy_id,"buddyName":buddy_name,"timestamp":ts+400}),
    ]
}

/// 模板使用事件（点亮 template_5，需 5 次）。
fn template_use_event(idx: usize) -> Value {
    json!({"eventCode":"template_used","templateId":format!("tpl_{idx}"),"timestamp":unix_ms()+idx as i64})
}

/// 灵感案例「做同款」（点亮 playbook_prompt）。
fn playbook_prompt_event() -> Value {
    json!({"eventCode":"playbook_prompt_send","playbookId":"pb_default","timestamp":unix_ms()})
}

/// 设计画布创建组（点亮 create_canvas）。
fn design_canvas_events() -> Vec<Value> {
    let ts = unix_ms();
    vec![
        json!({"eventCode":"wbx_design_canvas_create","timestamp":ts}),
        json!({"eventCode":"wbx_design_canvas_open","timestamp":ts+100}),
        json!({"eventCode":"wbx_design_canvas_save","timestamp":ts+200}),
    ]
}

/// 自动化任务创建事件（点亮 automation_1）。
fn automation_create_event() -> Value {
    json!({"eventCode":"automation_task_create","automationName":"每日总结","timestamp":unix_ms()})
}

/// 资料库阅读点击（点亮 Library_read，web 域上报）。
fn library_read_event() -> Value {
    json!({"eventCode":"library_read_click","docId":"doc_default","timestamp":unix_ms()})
}

/// 技能加载（点亮 skill_1，需配合真实对话）。
fn skill_load_event(skill: &str) -> Value {
    json!({"eventCode":"skill_info","skillName":skill,"timestamp":unix_ms()})
}

/// 专家召唤+使用（点亮 expert_5 / Expert_team_use_3 / Expert_lighthouse）。
fn expert_use_events(uid: &str, expert_id: &str, tag: &str) -> Vec<Value> {
    let conv = conv_id(uid, &format!("exp{expert_id}{tag}"));
    let req = msg_id(uid, &format!("expreq{expert_id}{tag}"));
    let ts = unix_ms();
    vec![
        json!({"eventCode":"expert_summon","expertId":expert_id,"tag":tag,"conversationId":conv,"requestId":req,"timestamp":ts}),
        json!({"eventCode":"chat_request_send","expertId":expert_id,"conversationId":conv,"requestId":req,"modelId":"fast-model","timestamp":ts+100}),
        json!({"eventCode":"expert_actual_use","expertId":expert_id,"conversationId":conv,"requestId":req,"timestamp":ts+900}),
    ]
}

/// 任务清单读取。
pub async fn list_tasks(http: &Client, base: &str, token: &str) -> Result<Vec<Value>, String> {
    let resp = http
        .get(format!("{base}{TASKS_LIST_PATH}"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", crate::wb::cli_ua())
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}: {}", text.chars().take(160).collect::<String>()));
    }
    let v: Value = serde_json::from_str(&text).map_err(|_| {
        format!("上游返回非 JSON（可能登录态失效）：{}", text.chars().take(120).collect::<String>())
    })?;
    let data = v.get("data").cloned().unwrap_or(v);
    Ok(data
        .get("tasks")
        .and_then(|t| t.as_array())
        .or_else(|| data.as_array())
        .cloned()
        .unwrap_or_default())
}

/// 接受任务。
pub async fn accept_tasks(http: &Client, base: &str, token: &str, codes: &[String]) -> Result<(), String> {
    if codes.is_empty() {
        return Ok(());
    }
    let resp = http
        .post(format!("{base}{TASKS_ACCEPT_PATH}"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", crate::wb::cli_ua())
        .json(&json!({"taskCodes": codes}))
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let t = resp.text().await.unwrap_or_default();
        return Err(format!("accept HTTP {status}: {}", t.chars().take(120).collect::<String>()));
    }
    Ok(())
}

/// 领取某任务奖励 → (credit, energy)。
pub async fn claim_reward(
    http: &Client,
    web_base: &str,
    token: &str,
    code: &str,
) -> Result<(i64, i64), String> {
    let resp = http
        .post(format!("{web_base}/activity/growth/tasks/{code}/claim"))
        .timeout(std::time::Duration::from_secs(20))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("User-Agent", crate::wb::cli_ua())
        .json(&json!({}))
        .send()
        .await
        .map_err(|e| format!("网络失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        return Err(format!("claim HTTP {status}: {}", text.chars().take(120).collect::<String>()));
    }
    let data = v.get("data").cloned().unwrap_or(v);
    Ok((
        data.get("credit").and_then(|x| x.as_i64()).unwrap_or(0),
        data.get("energy").and_then(|x| x.as_i64()).unwrap_or(0),
    ))
}

/// 一键完成成长任务（对齐 workbuddy2api 的 17 任务闭环）。
///
/// 流程：读清单 → 接受未完成任务 → 逐个点亮（事件链/真实对话）→ 领取奖励。
/// 返回逐任务结果，失败不阻断其余。
/// 上游任务对象的字段兼容。
///
/// ★ 2026-11-07 实测（GET /v2/activity/growth/tasks）：字段已从旧版的
///   `taskCode/taskName/claimed/completed` 改成
///   `task_code/title/accept_status/progress/reward_credit`。
///   旧的 `taskCode` 取不到 → pending 恒 0、逐项领奖整段被跳过，
///   用户点「立即执行」只看到事件链跑了一下，以为只做一项。
///   两版都认，避免上游再次改名时又静默失效。
fn task_code(t: &Value) -> Option<&str> {
    t.get("task_code").or_else(|| t.get("taskCode")).and_then(|x| x.as_str())
}
fn task_title(t: &Value) -> Option<&str> {
    t.get("title").or_else(|| t.get("taskName"))
        .or_else(|| t.get("name")).and_then(|x| x.as_str())
}
fn task_claimed(t: &Value) -> bool {
    if let Some(s) = t.get("accept_status").and_then(|x| x.as_str()) {
        return s.eq_ignore_ascii_case("claimed") || s.eq_ignore_ascii_case("done");
    }
    t.get("claimed").and_then(|x| x.as_bool()).unwrap_or(false)
}
fn task_completed(t: &Value) -> bool {
    if let Some(p) = t.get("progress").and_then(|x| x.as_object()) {
        let cur = p.get("current").and_then(|x| x.as_i64()).unwrap_or(0);
        let target = p.get("target").and_then(|x| x.as_i64()).unwrap_or(1);
        // ★ WorkBuddy-Daily（562★，2026-11-07 更新）明确：领奖按
        //   progress.current >= progress.target 补领，不依赖 accept_status
        //   （它可能延迟或回滚，把「已达标未领」误判成「已领取」）。
        return cur >= target;
    }
    t.get("completed").and_then(|x| x.as_bool())
        .or_else(|| t.get("done").and_then(|x| x.as_bool()))
        .unwrap_or(false)
}
/// 达标但尚未领奖的任务 = 需要打 claim 接口的（幂等补缺）。
fn task_claimable(t: &Value) -> bool {
    task_completed(t) && !task_claimed(t)
}
/// 需要打 accept 接口的：既未领取、也**未接受**。
///
/// ★ 2026-11-07 实测：`accept_status` 有三档（pending / accepted / claimed）。
///   旧代码把 accepted 也当成「待接受」重复 POST accept，上游直接回
///   `400 invalid ...`，每次执行都多出一条失败记录——那其实是无害的重复请求。
fn task_needs_accept(t: &Value) -> bool {
    if task_claimed(t) { return false; }
    match t.get("accept_status").and_then(|x| x.as_str()) {
        Some(s) => s.eq_ignore_ascii_case("pending") || s.is_empty(),
        None => !t.get("claimed").and_then(|x| x.as_bool()).unwrap_or(false),
    }
}
/// 只能人工完成的任务（关注公众号等）：事件链永远点不亮，不算失败。
fn task_manual_only(t: &Value) -> bool {
    let code = task_code(t).unwrap_or("");
    let title = task_title(t).unwrap_or("");
    code.contains("wechat") || title.contains("公众号") || title.contains("关注")
}
fn task_reward(t: &Value) -> Value {
    t.get("reward_credit").or_else(|| t.get("credit"))
        .or_else(|| t.get("reward")).cloned().unwrap_or(Value::Null)
}

pub async fn run_all_growth(
    http: &Client,
    base: &str,
    web_base: &str,
    token: &str,
    uid: &str,
    nickname: &str,
) -> Value {
    let mut steps: Vec<Value> = Vec::new();
    // ① 任务清单
    let tasks = match list_tasks(http, base, token).await {
        Ok(t) => t,
        Err(e) => return json!({"ok": false, "message": format!("读任务清单失败：{e}"), "steps": steps}),
    };
    let total = tasks.len();
    // ★ 2026-11-07：把上游任务清单（裁剪成展示字段）放进结果里——
    //   参考项目（MiraiHub / WorkBuddy-Daily）的结果面板是**逐任务**显示的，
    //   只回一句「点亮 14 组」用户无从知道 18 个任务各自是已完成还是本次领到。
    //   按 completed / claimed 归一出 status，前端不必再猜字段名。
    let task_rows: Vec<Value> = tasks
        .iter()
        .map(|t| {
            let code = task_code(t).unwrap_or("").to_string();
            let name = task_title(t).unwrap_or(&code).to_string();
            let claimed = task_claimed(t);
            let completed = task_completed(t);
            json!({
                "code": code,
                "name": name,
                "status": if claimed { "claimed" } else if completed { "completed" } else { "pending" },
                "credit": task_reward(t),
            })
        })
        .collect();
    steps.push(json!({"step":"读取任务清单","ok":true,"action":"list","total":total,"tasks":task_rows,
                      "message":format!("上游返回 {total} 项任务")}));
    // ② 接受未完成的任务（只接受 accept_status=pending 的；
    //    accepted 重复接受会被上游 400 拒绝）
    let pending: Vec<String> = tasks
        .iter()
        .filter(|t| task_needs_accept(t))
        .filter_map(|t| task_code(t).map(|s| s.to_string()))
        .collect();
    // ★ 2026-11-07 对齐 workbuddy2api-master/admin/growth_plan.py：
    //   每个动作一条 {step, ok, message}，逐账号结果里能直接展开看「做了什么」。
    //   之前只回 4 条聚合（accept/light/claim），前端只能显示一句总览，
    //   用户以为「只做了一项」——其实是展示丢了细节。
    if pending.is_empty() {
        steps.push(json!({"step":"接受任务","ok":true,"message":"今日任务均已接受/完成，无需重复接受"}));
    } else {
        match accept_tasks(http, base, token, &pending).await {
            Ok(()) => steps.push(json!({"step":"接受任务","ok":true,
                       "message":format!("新接受 {} 个任务", pending.len()),
                       "action":"accept","count":pending.len()})),
            Err(e) => steps.push(json!({"step":"接受任务","ok":false,"message":e,
                       "action":"accept","count":pending.len()})),
        }
    }
    // ③ 逐个点亮（事件链上报；失败不阻断）
    let mut lit = 0;
    let mut report_err = String::new();
    // 对话事件链（点亮 RichMeow_Chat / chat_5 / skill_1 / Model_chat）
    let conv = conv_id(uid, "growth");
    let req = msg_id(uid, "growthreq");
    let msg = msg_id(uid, "growthmsg");
    if let Err(e) = report_desktop_events(
        http, base, token, uid, nickname,
        &chat_sequence(uid, &conv, &req, &msg, "glm-5.2"),
    ).await {
        report_err = e;
    } else { lit += 1; }
    // Buddy App（Buddy_App / Buddy_App_QQ）
    for (bid, bname) in [("cb_y5Dy46tPQGGWtueMxXbe", "企鹅教师助手"), ("cb_default", "默认搭子")] {
        if report_desktop_events(http, base, token, uid, nickname, &buddy_app_sequence(bid, bname)).await.is_ok() {
            lit += 1;
        }
    }
    // 模板 5 次（template_5）
    let tpl: Vec<Value> = (0..5).map(template_use_event).collect();
    if report_desktop_events(http, base, token, uid, nickname, &tpl).await.is_ok() { lit += 1; }
    // 灵感案例 + 设计画布 + 自动化 + 资料库 + 技能
    for ev in [vec![playbook_prompt_event()], design_canvas_events(),
               vec![automation_create_event()], vec![library_read_event()],
               vec![skill_load_event("code_review")]] {
        if report_desktop_events(http, base, token, uid, nickname, &ev).await.is_ok() { lit += 1; }
    }
    // 专家链（expert_5 ×5 / Expert_team_use_3 / Expert_lighthouse）
    for (i, tag) in ["expert_a", "expert_b", "expert_c", "expert_d", "expert_e"].iter().enumerate() {
        if report_desktop_events(http, base, token, uid, nickname, &expert_use_events(uid, &format!("ex{i}"), tag)).await.is_ok() {
            lit += 1;
        }
    }
    steps.push(json!({
        "step": "事件链点亮", "ok": lit > 0,
        "message": if lit > 0 {
            format!("点亮 {lit} 组：对话 / Buddy App / 模板 / 灵感 / 设计画布 / 自动化 / 资料库 / 技能 / 专家链")
        } else { format!("事件链上报失败：{report_err}") },
        "action": "light_events", "lit": lit, "last_error": report_err,
    }));

    // ④ 领奖：按进度扫描「达标未领」任务补领（WorkBuddy-Daily 口径），
    //   已领 / 未达标分别计数并进待办分布，不再静默跳过。
    let mut credit = 0i64;
    let mut energy = 0i64;
    let mut claimed = 0;
    let mut already = 0;
    let mut unmet: Vec<Value> = Vec::new();
    let mut claim_errors: Vec<Value> = Vec::new();
    for t in &tasks {
        let code = task_code(t).unwrap_or("");
        let name = task_title(t).unwrap_or(code);
        if task_claimed(t) { already += 1; continue; }
        if !task_completed(t) {
            unmet.push(json!({"code":code, "name":name,
                "current": t.pointer("/progress/current").cloned().unwrap_or(Value::Null),
                "target": t.pointer("/progress/target").cloned().unwrap_or(Value::Null)}));
            continue;
        }
        if code.is_empty() { continue; }
        match claim_reward(http, web_base, token, code).await {
            Ok((c, e)) => { credit += c; energy += e; claimed += 1; }
            Err(e) => claim_errors.push(json!({"code":code,"name":name,"error":e})),
        }
    }
    steps.push(json!({
        "step": "逐项领奖",
        "ok": claim_errors.is_empty(),
        "message": format!("本次领取 {claimed} 项 / +{credit} 积分 · 今日已领 {already} 项 · 待完成 {todo_n} 项", todo_n = unmet.len()),
        "action": "claim", "claimed": claimed, "credit": credit, "energy": energy,
        "already": already, "todo": unmet, "errors": claim_errors,
    }));
    // 待办里区分「机器做不了、只能人工点」的项（关注公众号等）。
    let manual: Vec<Value> = tasks.iter().filter(|t| task_manual_only(t) && !task_claimed(t))
        .map(|t| json!({"code": task_code(t).unwrap_or(""), "name": task_title(t).unwrap_or("")}))
        .collect();
    json!({
        "ok": claim_errors.is_empty(),
        "summary": format!("任务清单 {total} 项 · 待接受 {pending_n} 项 · 事件链 {lit} 组 · 本次领奖 {claimed} 项 / +{credit} 积分 · 待完成 {todo_n} 项 · 需人工 {manual_n} 项",
            pending_n = pending.len(), todo_n = unmet.len(), manual_n = manual.len()),
        "total_tasks": total,
        "pending": pending.len(),
        "lit_groups": lit,
        "claimed": claimed,
        "credit": credit,
        "energy": energy,
        "manual_tasks": manual,
        "steps": steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ 上游任务字段漂移回归：2026-11-07 实测为
    ///   task_code / title / accept_status / progress / reward_credit。
    ///   旧代码只认 taskCode，结果 18 项任务全部被判「无需接受/无需领奖」，
    ///   面板只剩事件链一条——用户看到「只做一项」。
    #[test]
    fn parses_current_upstream_task_shape() {
        let t = json!({
            "task_code": "playbook_prompt", "title": "探索优秀灵感",
            "accept_status": "claimed", "progress": {"current": 1, "target": 1},
            "reward_credit": 100
        });
        assert_eq!(task_code(&t), Some("playbook_prompt"));
        assert_eq!(task_title(&t), Some("探索优秀灵感"));
        assert!(task_claimed(&t));
        assert!(task_completed(&t));
        assert_eq!(task_reward(&t), json!(100));
    }

    #[test]
    fn progress_reached_but_accept_status_stale_is_still_claimable() {
        // WorkBuddy-Daily 的关键口径：progress 达标就该补领，
        // accept_status 可能延迟/回滚，不能只看它判断「已领取」。
        let t = json!({
            "task_code": "template_5", "title": "使用模板5次",
            "accept_status": "accepted",
            "progress": {"current": 5, "target": 5}, "reward_credit": 100
        });
        assert!(task_completed(&t));
        assert!(!task_claimed(&t));
        assert!(task_claimable(&t));
        let claimed = json!({
            "task_code": "template_5", "accept_status": "claimed",
            "progress": {"current": 5, "target": 5}
        });
        assert!(!task_claimable(&claimed));
    }

    /// accept_status 三档：只有 pending 才该打 accept。
    /// 实测 accepted 重复接受 → 上游 400 invalid，每次执行都多一条假失败。
    #[test]
    fn only_pending_needs_accept() {
        let pending = json!({"task_code":"a","accept_status":"pending"});
        let accepted = json!({"task_code":"b","accept_status":"accepted"});
        let claimed = json!({"task_code":"c","accept_status":"claimed"});
        assert!(task_needs_accept(&pending));
        assert!(!task_needs_accept(&accepted));
        assert!(!task_needs_accept(&claimed));
    }

    /// 关注公众号类任务机器做不了，必须标成「需人工」而不是失败。
    #[test]
    fn wechat_task_is_manual_only() {
        let t = json!({"task_code":"wb_wechat_oa_subscribe_task",
            "title":"关注「腾讯 WorkBuddy」官方公众号","accept_status":"accepted",
            "progress":{"current":0,"target":1}});
        assert!(task_manual_only(&t));
        assert!(!task_completed(&t));
        assert!(!task_claimable(&t));
    }

    #[test]
    fn parses_legacy_task_shape() {
        let t = json!({
            "taskCode": "chat_5", "taskName": "桌面端对话",
            "claimed": false, "completed": false, "credit": 20
        });
        assert_eq!(task_code(&t), Some("chat_5"));
        assert_eq!(task_title(&t), Some("桌面端对话"));
        assert!(!task_claimed(&t));
        assert_eq!(task_reward(&t), json!(20));
    }
}
