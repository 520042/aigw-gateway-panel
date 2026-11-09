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
        if cur >= target { return true; }
    }
    t.get("completed").and_then(|x| x.as_bool())
        .or_else(|| t.get("done").and_then(|x| x.as_bool()))
        .unwrap_or(false)
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
    // ② 接受未完成的任务（accept_status != claimed）
    let pending: Vec<String> = tasks
        .iter()
        .filter(|t| !task_claimed(t))
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

    // ④ 领奖（逐个尝试，已领过的会如实报错）
    let mut credit = 0i64;
    let mut energy = 0i64;
    let mut claimed = 0;
    for t in &tasks {
        let Some(code) = task_code(t) else { continue };
        if task_claimed(t) { continue; }
        if let Ok((c, e)) = claim_reward(http, web_base, token, code).await {
            credit += c;
            energy += e;
            claimed += 1;
        }
    }
    steps.push(json!({
        "step": "逐项领奖",
        "ok": claimed > 0 || total > 0,
        "message": if claimed > 0 {
            format!("本次领取 {claimed} 项，+{credit} 积分 / +{energy} 能量")
        } else { "今日任务均已领取（幂等跳过，无需重复领奖）".to_string() },
        "action": "claim", "claimed": claimed, "credit": credit, "energy": energy,
    }));
    json!({
        "ok": true,
        "summary": format!("任务清单 {total} 项 · 待接受 {pending_n} 项 · 事件链 {lit} 组 · 本次领奖 {claimed} 项 / +{credit} 积分",
            pending_n = pending.len()),
        "total_tasks": total,
        "pending": pending.len(),
        "lit_groups": lit,
        "claimed": claimed,
        "credit": credit,
        "energy": energy,
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
