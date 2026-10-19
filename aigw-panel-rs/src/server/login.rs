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

/// 选号候选：凭据 + 账号身份 + 排序依据。
/// ★ urgency/last_used 由 `credits` / 账号池维护，`v1.rs::relay_pooled`
///   在此之上再叠加 RecentPick 惩罚与冷却过滤。
#[derive(Debug, Clone)]
pub struct RankedAccount {
    /// accounts.json 的 `id`（治理状态的键；老数据可能为空串）
    pub id: String,
    /// 积分到期紧迫度（`credits::urgency_score`，小 = 更该先用）
    pub urgency: i64,
    /// 上次使用时间串（紧迫度相同时等价轮询）
    pub last_used: String,
    /// 未脱敏 secret
    pub secret: String,
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
    /// ⚠ 顺序 = accounts.json 里的物理顺序，不带任何优先级。
    ///   请求路径请用 `secrets_ranked()`（紧迫度优先）。
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

    /// 把积分快照算出的紧迫度写回账号池。
    /// `items` = `[(account_id, urgency_score, remain, days_left)]`，
    /// 由 `credits::account_urgency()` 产出；写不进 / 账号已删的行直接跳过。
    ///
    /// ★ 设计：紧迫度**只落盘不实时算**。请求路径上绝不能去打上游余额接口
    ///   （那会给每次对话增加几百毫秒 ~ 几秒），所以由 `/api/credits` 的
    ///   60s 缓存刷新周期顺手写回，选号时只读内存里的一个整数。
    pub fn set_urgency(&self, items: &[(String, i64, f64, Option<i64>)]) {
        if items.is_empty() {
            return;
        }
        let mut rows = self.load_rows();
        let mut changed = false;
        let ts = crate::server::handlers::api::routers::util::now_local();
        for a in rows.iter_mut() {
            let Some(id) = a.get("id").and_then(|v| v.as_str()) else { continue };
            let Some((_, score, remain, days)) = items.iter().find(|(i, ..)| i == id) else {
                continue;
            };
            let obj = a.as_object_mut();
            let Some(m) = obj else { continue };
            m.insert("urgency".to_string(), json!(score));
            m.insert("urgency_remain".to_string(), json!(remain));
            m.insert("urgency_days".to_string(), json!(days));
            m.insert("urgency_updated".to_string(), json!(ts));
            changed = true;
        }
        if changed {
            self.save_rows(&rows);
        }
    }

    /// 按紧迫度排序的有效凭据 —— **请求路径选号入口**。
    ///
    /// 规则（对齐参考框架 `admin/pool.py::urgency_rank`）：
    /// 1. `urgency` 小的排前（`credits::urgency_score`：到期日越近越靠前；
    ///    无到期日时余额多的靠前；两者都没有排最后）
    /// 2. `urgency` 相同（都等于 i64::MAX = 都没数据）→ 按 `last_used` 升序，
    ///    等价于轮询，避免同一个账号被打爆
    /// 3. 都没有 `last_used` → 保持文件顺序（稳定，可复现）
    /// 按紧迫度排序的有效凭据（**请求路径选号入口**，带账号 id）。
    ///
    /// 返回 `RankedAccount` 而不只是 secret，是为了让调用方（`v1.rs::relay_pooled`）
    /// 能把失败/在途状态记到**具体账号**上 —— 只拿 secret 字符串是没法做
    /// (账号,模型) 粒度治理的。
    pub fn accounts_ranked(&self, platform: &str) -> Vec<RankedAccount> {
        let mut rows: Vec<RankedAccount> = self
            .load_rows()
            .iter()
            .filter(|a| {
                a.get("platform").and_then(|v| v.as_str()) == Some(platform)
                    && a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true)
                    && !a.get("secret").and_then(|v| v.as_str()).unwrap_or("").is_empty()
            })
            .filter_map(|a| {
                let sec = a.get("secret").and_then(|v| v.as_str())?.to_string();
                Some(RankedAccount {
                    id: a.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    urgency: a.get("urgency").and_then(|v| v.as_i64()).unwrap_or(i64::MAX),
                    last_used: a.get("last_used").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    secret: sec,
                })
            })
            .collect();
        rows.sort_by(|a, b| a.urgency.cmp(&b.urgency).then_with(|| a.last_used.cmp(&b.last_used)));
        rows
    }

    /// 按紧迫度排序的有效凭据（兼容老签名：只要 secret 列表）。
    pub fn secrets_ranked(&self, platform: &str) -> Vec<String> {
        self.accounts_ranked(platform)
            .into_iter()
            .map(|a| a.secret)
            .collect()
    }

    /// 取平台的第一个可用凭据 + 其类型（用于通用实时拉取判断怎么带鉴权头）
    pub fn secret_and_type(&self, platform: &str) -> Option<(String, String)> {
        self.load_rows().iter().find_map(|a| {
            if a.get("platform").and_then(|v| v.as_str()) != Some(platform) {
                return None;
            }
            if !a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
                return None;
            }
            let sec = a.get("secret").and_then(|v| v.as_str()).unwrap_or("");
            if sec.is_empty() {
                return None;
            }
            let typ = a.get("type").and_then(|v| v.as_str()).unwrap_or("token").to_string();
            Some((sec.to_string(), typ))
        })
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
    /// ★ 2026-10-15 兼容修复：Python 版 `accounts.py` 落盘是**裸数组**
    ///   （store.put("accounts", rows) → 文件内容就是 rows），而 Rust 历史实现
    ///   写的是 `{"accounts": [...]}`。只认后者会导致「用 Python 版攒下的账号池
    ///   在 Rust 面板里读出来 0 个」。这里**两种都认**。
    fn load_rows(&self) -> Vec<Value> {
        let Ok(s) = std::fs::read_to_string(self.accounts_path()) else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<Value>(&s) else {
            return Vec::new();
        };
        if let Some(arr) = v.as_array() {
            return arr.clone();
        }
        v.get("accounts")
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn save_rows(&self, rows: &[Value]) {
        let _ = std::fs::create_dir_all(&self.data_dir);
        // ★ 写**裸数组**（与 Python 版同格式），保证两个面板互相可读、不丢账号
        let _ = std::fs::write(
            self.accounts_path(),
            serde_json::to_string_pretty(&Value::Array(rows.to_vec())).unwrap_or_default(),
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

    /// 账号池原始行（**secret 未脱敏**）。
    /// ⚠ 只在服务端内部使用；`list_accounts()` 给前端的是掩码版，
    ///    拿掩码去打上游必然 401 —— 积分/余额类后台任务必须用本方法。
    pub fn raw_rows(&self) -> Vec<Value> {
        self.load_rows()
    }

    /// 刷新所有账号的积分/余额（按 catalog.gwextra 的 points_balance 动作）。
    pub async fn refresh_balances(&self, gwextra: &Value) {
        let mut rows = self.load_rows();
        let mut changed = false;
        let ts = now_str();
        for a in rows.iter_mut() {
            let platform = a.get("platform").and_then(|v| v.as_str()).unwrap_or("");
            let secret = a.get("secret").and_then(|v| v.as_str()).unwrap_or("");
            let typ = a.get("type").and_then(|v| v.as_str()).unwrap_or("token");
            let enabled = a.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            if platform.is_empty() || secret.is_empty() || !enabled {
                continue;
            }
            if let Some(balance) = crate::server::balance::fetch_balance(platform, secret, typ, gwextra).await {
                a["balance"] = json!(balance);
                a["balance_updated"] = json!(ts);
                changed = true;
            }
        }
        if changed {
            self.save_rows(&rows);
        }
    }
}
