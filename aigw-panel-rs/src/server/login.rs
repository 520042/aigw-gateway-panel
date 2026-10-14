//! 登录会话管理 + 账号池。
//!
//! 闭环范围（对齐前端 app.js 的登录契约）：
//! - `start`  → 生成会话（status=waiting + auth_url 跳转登录页）
//! - `poll`   → 前端 2s 轮询；submit 保存凭据后会话转 success，前端自动收尾
//! - `submit` → 三种分支（带 id 完成会话 / 不带 id 手动添加 / cookie 与 secret 两种键）
//! - `cancel` → 丢弃会话
//! - 账号池   → 落库 `data/accounts.json`（结构对齐 Python accounts.py：
//!   id/platform/name/type/secret/enabled/obtained_at…），列表接口掩码
//!
//! ★ 诚实边界：浏览器自动捕捉凭据（CDP/无头）与远程扫码是后续模块；
//!   当前「跳转登录 + 手动粘贴」全流程真实可用，不做任何假捕捉。
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

static SEQ: AtomicU32 = AtomicU32::new(0);

fn new_id(prefix: &str) -> String {
    let n = SystemNanos();
    let s = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{}{:x}{:02x}", prefix, n, s)
}

fn SystemNanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn now_str() -> String {
    // 本地时间字符串（不引 chrono，用 unix 秒即可满足排序与展示）
    let secs = SystemNanos() / 1_000_000_000;
    format!("@{}", secs)
}

pub struct LoginManager {
    sessions: Mutex<HashMap<String, Value>>,
    data_dir: std::path::PathBuf,
}

impl LoginManager {
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        LoginManager {
            sessions: Mutex::new(HashMap::new()),
            data_dir,
        }
    }

    fn accounts_path(&self) -> std::path::PathBuf {
        self.data_dir.join("accounts.json")
    }

    // ── 会话 ────────────────────────────────────────────────
    pub fn start(&self, platform: &str, method: &str, login_url: &str) -> Value {
        self.start_ex_state(platform, method, login_url, "", "")
    }

    /// start 带自定义引导文案 + 设备授权 state（wb 族自动登录用）。
    pub fn start_ex_state(&self, platform: &str, method: &str, login_url: &str, message: &str, state: &str) -> Value {
        let id = new_id("s");
        let msg = if message.is_empty() {
            "已生成登录会话。点「打开登录页」完成登录，然后把 Cookie / Token 粘贴到下方输入框保存（自动捕捉凭据模块待接入）"
        } else {
            message
        };
        let sess = json!({
            "id": id,
            "platform": platform,
            "method": method,
            "status": "waiting",
            "auth_url": login_url,
            "state": state,
            "seconds_left": 600,
            "message": msg,
        });
        self.sessions
            .lock()
            .unwrap()
            .insert(sess["id"].as_str().unwrap_or("").to_string(), sess.clone());
        sess
    }

    pub fn poll(&self, id: &str) -> Option<Value> {
        self.sessions.lock().unwrap().get(id).cloned()
    }

    /// 回写会话（自动登录轮询更新 message/status 用）。
    pub fn update_session(&self, sess: &Value) {
        if let Some(id) = sess.get("id").and_then(|v| v.as_str()) {
            self.sessions.lock().unwrap().insert(id.to_string(), sess.clone());
        }
    }

    pub fn complete(&self, id: &str, message: &str) -> Option<Value> {
        let mut m = self.sessions.lock().unwrap();
        let sess = m.get_mut(id)?;
        sess["status"] = json!("success");
        sess["message"] = json!(message);
        Some(sess.clone())
    }

    pub fn cancel(&self, id: &str) {
        self.sessions.lock().unwrap().remove(id);
    }

    /// 某平台的全部有效凭据（enabled 且 secret 非空）——原生中继轮询用。
    pub fn secrets_for(&self, platform: &str) -> Vec<String> {
        self.load_rows()
            .iter()
            .filter(|a| {
                a.get("platform").and_then(|v| v.as_str()) == Some(platform)
                    && a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)
                    && !a.get("secret").and_then(|v| v.as_str()).unwrap_or("").is_empty()
            })
            .filter_map(|a| a.get("secret").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .collect()
    }

    /// 写入/更新 refresh_token（保活轮换用，对齐 Python 账号池 refresh_token 字段）。
    pub fn update_refresh(&self, platform: &str, refresh: &str) {
        if refresh.is_empty() {
            return;
        }
        let mut rows = self.load_rows();
        let mut changed = false;
        for a in rows.iter_mut() {
            if a.get("platform").and_then(|v| v.as_str()) == Some(platform)
                && a.get("refresh_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .is_empty()
            {
                a["refresh_token"] = json!(refresh);
                changed = true;
            }
        }
        if changed {
            self.save_rows(&rows);
        }
    }

    /// 在途会话 id（status=waiting/pending，按平台前缀过滤）——自动授权完成时批量收尾。
    pub fn pending_sessions(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| {
                let st = s.get("status").and_then(|v| v.as_str()).unwrap_or("");
                (st == "waiting" || st == "pending")
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    // ── 账号池（data/accounts.json，结构对齐 Python） ────────
    fn load_rows(&self) -> Vec<Value> {
        match std::fs::read_to_string(self.accounts_path()) {
            Ok(s) => serde_json::from_str::<Value>(&s)
                .ok()
                .and_then(|v| v.get("accounts").and_then(|a| a.as_array()).cloned())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_rows(&self, rows: &[Value]) {
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::write(
            self.accounts_path(),
            serde_json::to_string_pretty(&json!({"accounts": rows})).unwrap_or_default(),
        );
    }

    /// 新增或更新（同 platform+name 视为同一账号；空 secret 不覆盖已有密钥）。
    pub fn add_account(&self, platform: &str, name: &str, typ: &str, secret: &str) -> Value {
        let mut rows = self.load_rows();
        let now = now_str();
        for a in rows.iter_mut() {
            if a.get("platform").and_then(|v| v.as_str()) == Some(platform)
                && a.get("name").and_then(|v| v.as_str()) == Some(name)
            {
                a["type"] = json!(typ);
                if !secret.is_empty() {
                    a["secret"] = json!(secret);
                }
                a["obtained_at"] = json!(now);
                a["enabled"] = json!(true);
                let out = a.clone();
                self.save_rows(&rows);
                return out;
            }
        }
        let item = json!({
            "id": new_id("a"),
            "platform": platform,
            "name": if name.is_empty() { platform } else { name },
            "type": typ,
            "secret": secret,
            "source": "manual",
            "edition": "",
            "enabled": true,
            "obtained_at": now,
            "last_used": "",
            "last_check": "",
            "status": "unknown",
            "quota": Value::Null,
            "note": "",
        });
        rows.push(item.clone());
        self.save_rows(&rows);
        item
    }

    pub fn delete_account(&self, id: &str) -> bool {
        let mut rows = self.load_rows();
        let before = rows.len();
        rows.retain(|a| a.get("id").and_then(|v| v.as_str()) != Some(id));
        let changed = rows.len() != before;
        if changed {
            self.save_rows(&rows);
        }
        changed
    }

    pub fn toggle_account(&self, id: &str, on: bool) -> bool {
        let mut rows = self.load_rows();
        let mut changed = false;
        for a in rows.iter_mut() {
            if a.get("id").and_then(|v| v.as_str()) == Some(id) {
                a["enabled"] = json!(on);
                changed = true;
            }
        }
        if changed {
            self.save_rows(&rows);
        }
        changed
    }

    /// 列表（secret 掩码，对齐 Python accounts.list）。
    pub fn list_accounts(&self) -> (Vec<Value>, Value) {
        let rows = self.load_rows();
        let with_secret = rows
            .iter()
            .filter(|a| !a.get("secret").and_then(|v| v.as_str()).unwrap_or("").is_empty())
            .count();
        let mut out = Vec::new();
        for a in &rows {
            let mut o = a.clone();
            if let Value::Object(ref mut m) = o {
                let s = a.get("secret").and_then(|v| v.as_str()).unwrap_or("");
                let masked = if s.is_empty() {
                    json!("")
                } else if s.len() > 12 {
                    json!(format!("{}…{}", &s[..6.min(s.len())], &s[s.len() - 4..]))
                } else {
                    json!("*".repeat(s.len()))
                };
                m.insert("secret".to_string(), masked);
            }
            out.push(o);
        }
        (out, json!({"total": rows.len(), "with_secret": with_secret}))
    }
}
