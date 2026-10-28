//! 系统提示词注入（对齐 magpie-community/plugins packages/system-prompt）。
//!
//! 场景：客户端（Claude Code / Codex / Cursor…）会用**自己的** system prompt，
//! 用户想在所有请求上加一句自己的约束（"用中文回答" / "不要输出道歉" 之类）。
//! 这个模块把这句话注入到**每种协议各自的位置**：
//!
//! | 协议 | 注入位置 |
//! |---|---|
//! | chat（OpenAI） | messages[0] 的 system/developer |
//! | anthropic | `system`（字符串或 content block 数组） |
//! | responses | `instructions` |
//! | gemini | `systemInstruction.parts[]` |
//!
//! ## 语义（照抄参考项目，顺序不可改）
//! 1. 请求**没有**系统提示词 → 直接放我们的
//! 2. 请求**有**自己的 → 默认**不动**（`override=false`）
//! 3. `override=true` 时才叠加，位置由 `position` 决定：
//!    - `append`（默认）：放在原提示词**之后** —— 智能体会缓存自己的前缀，
//!      追加在后面不破坏该缓存
//!    - `prepend`：放在**之前**（New API 的老做法），代价是每轮都要重读整个提示词
//! 4. `models` 给前缀列表时只对匹配的模型生效；`agents` 按调用方标识过滤。

use serde_json::{json, Value};

/// 注入配置。
#[derive(Debug, Clone, Default)]
pub struct SystemPromptCfg {
    /// 要注入的提示词（空串 = 不启用）。
    pub prompt: String,
    /// 请求已有自己的提示词时，是否叠加。
    pub override_existing: bool,
    /// `true` = 放前面（prepend）；`false` = 放后面（append，默认）。
    pub prepend: bool,
    /// 仅在模型名以这些前缀开头时生效（空 = 全部）。
    pub models: Vec<String>,
}

impl SystemPromptCfg {
    /// 从 JSON 配置构造（对齐参考项目的 options 形状）。
    pub fn from_json(o: &Value) -> Self {
        SystemPromptCfg {
            prompt: o
                .get("system_prompt")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            override_existing: o.get("override").and_then(|x| x.as_bool()).unwrap_or(false),
            prepend: o.get("position").and_then(|x| x.as_str()) == Some("prepend"),
            models: o
                .get("models")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.prompt.is_empty()
    }

    /// 模型前缀过滤。
    fn applies_to_model(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|m| model.starts_with(m.as_str()))
    }
}

/// 协议种类（由调用方告知）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Proto {
    Chat,
    Anthropic,
    Responses,
    Gemini,
}

/// 按协议注入。返回 true 表示请求体被改动了。
pub fn apply(body: &mut Value, cfg: &SystemPromptCfg, proto: Proto) -> bool {
    if !cfg.is_enabled() {
        return false;
    }
    let model = body
        .get("model")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if !cfg.applies_to_model(&model) {
        return false;
    }
    match proto {
        Proto::Chat => chat(body, cfg),
        Proto::Anthropic => anthropic(body, cfg),
        Proto::Responses => responses(body, cfg),
        Proto::Gemini => gemini(body, cfg),
    }
}

fn joined(original: &str, prompt: &str, prepend: bool) -> String {
    if prepend {
        format!("{prompt}\n{original}")
    } else {
        format!("{original}\n{prompt}")
    }
}

fn chat(body: &mut Value, cfg: &SystemPromptCfg) -> bool {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return false;
    };
    let is_sys = msgs
        .first()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        .map(|r| r == "system" || r == "developer")
        .unwrap_or(false);
    if !is_sys {
        msgs.insert(0, json!({"role": "system", "content": cfg.prompt}));
        return true;
    }
    if !cfg.override_existing {
        return false;
    }
    let Some(first) = msgs.first_mut() else { return false };
    match first.get_mut("content") {
        Some(Value::String(s)) => {
            let nv = joined(s, &cfg.prompt, cfg.prepend);
            *s = nv;
            true
        }
        Some(Value::Array(arr)) => {
            let block = json!({"type": "text", "text": cfg.prompt});
            if cfg.prepend {
                arr.insert(0, block);
            } else {
                arr.push(block);
            }
            true
        }
        _ => false,
    }
}

fn anthropic(body: &mut Value, cfg: &SystemPromptCfg) -> bool {
    let empty = match body.get("system") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    if empty {
        body["system"] = json!(cfg.prompt);
        return true;
    }
    if !cfg.override_existing {
        return false;
    }
    match body.get_mut("system") {
        Some(Value::String(s)) => {
            let nv = joined(s, &cfg.prompt, cfg.prepend);
            *s = nv;
            true
        }
        Some(Value::Array(arr)) => {
            let block = json!({"type": "text", "text": cfg.prompt});
            if cfg.prepend {
                arr.insert(0, block);
            } else {
                arr.push(block);
            }
            true
        }
        _ => false,
    }
}

fn responses(body: &mut Value, cfg: &SystemPromptCfg) -> bool {
    let cur = body.get("instructions").and_then(|x| x.as_str()).unwrap_or("");
    if cur.is_empty() {
        body["instructions"] = json!(cfg.prompt);
        return true;
    }
    if !cfg.override_existing {
        return false;
    }
    let nv = joined(cur, &cfg.prompt, cfg.prepend);
    body["instructions"] = json!(nv);
    true
}

fn gemini(body: &mut Value, cfg: &SystemPromptCfg) -> bool {
    // 上游两种拼写都有：优先沿用已有的那个 key
    let key = if body.get("system_instruction").is_some() {
        "system_instruction"
    } else {
        "systemInstruction"
    };
    let has_parts = body
        .get(key)
        .and_then(|s| s.get("parts"))
        .and_then(|p| p.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if !has_parts {
        body[key] = json!({"parts": [{"text": cfg.prompt}]});
        return true;
    }
    if !cfg.override_existing {
        return false;
    }
    if let Some(parts) = body
        .get_mut(key)
        .and_then(|s| s.get_mut("parts"))
        .and_then(|p| p.as_array_mut())
    {
        let part = json!({"text": cfg.prompt});
        if cfg.prepend {
            parts.insert(0, part);
        } else {
            parts.push(part);
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(prompt: &str, over: bool, prepend: bool) -> SystemPromptCfg {
        SystemPromptCfg {
            prompt: prompt.into(),
            override_existing: over,
            prepend,
            models: vec![],
        }
    }

    #[test]
    fn chat_injects_when_no_system() {
        let mut b = json!({"model":"m","messages":[{"role":"user","content":"hi"}]});
        assert!(apply(&mut b, &cfg("用中文", false, false), Proto::Chat));
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], "用中文");
    }

    #[test]
    fn chat_keeps_existing_by_default() {
        let mut b = json!({"messages":[{"role":"system","content":"orig"},{"role":"user","content":"hi"}]});
        assert!(!apply(&mut b, &cfg("用中文", false, false), Proto::Chat));
        assert_eq!(b["messages"][0]["content"], "orig");
    }

    #[test]
    fn chat_appends_when_override() {
        let mut b = json!({"messages":[{"role":"system","content":"orig"}]});
        assert!(apply(&mut b, &cfg("EXTRA", true, false), Proto::Chat));
        assert_eq!(b["messages"][0]["content"], "orig\nEXTRA");
    }

    #[test]
    fn chat_prepends_when_asked() {
        let mut b = json!({"messages":[{"role":"system","content":"orig"}]});
        assert!(apply(&mut b, &cfg("EXTRA", true, true), Proto::Chat));
        assert_eq!(b["messages"][0]["content"], "EXTRA\norig");
    }

    #[test]
    fn chat_developer_role_counts_as_system() {
        let mut b = json!({"messages":[{"role":"developer","content":"d"}]});
        assert!(!apply(&mut b, &cfg("X", false, false), Proto::Chat));
        assert_eq!(b["messages"].as_array().unwrap().len(), 1, "不该再插一条 system");
    }

    #[test]
    fn anthropic_string_and_array() {
        let mut a = json!({"model":"c"});
        assert!(apply(&mut a, &cfg("SYS", false, false), Proto::Anthropic));
        assert_eq!(a["system"], "SYS");

        let mut b = json!({"system":[{"type":"text","text":"orig"}]});
        assert!(apply(&mut b, &cfg("SYS", true, false), Proto::Anthropic));
        assert_eq!(b["system"].as_array().unwrap().len(), 2);
        assert_eq!(b["system"][1]["text"], "SYS");
    }

    #[test]
    fn responses_uses_instructions() {
        let mut b = json!({"model":"r"});
        assert!(apply(&mut b, &cfg("INS", false, false), Proto::Responses));
        assert_eq!(b["instructions"], "INS");
    }

    #[test]
    fn gemini_uses_system_instruction() {
        let mut b = json!({"model":"g"});
        assert!(apply(&mut b, &cfg("G", false, false), Proto::Gemini));
        assert_eq!(b["systemInstruction"]["parts"][0]["text"], "G");
    }

    #[test]
    fn model_prefix_filter() {
        let mut c = cfg("X", false, false);
        c.models = vec!["deepseek".into()];
        let mut a = json!({"model":"deepseek-v4","messages":[]});
        assert!(apply(&mut a, &c, Proto::Chat), "匹配前缀应注入");
        let mut b = json!({"model":"gpt-5","messages":[]});
        assert!(!apply(&mut b, &c, Proto::Chat), "不匹配不该注入");
    }

    #[test]
    fn empty_prompt_is_noop() {
        let mut b = json!({"messages":[{"role":"user","content":"hi"}]});
        assert!(!apply(&mut b, &cfg("", false, false), Proto::Chat));
    }

    #[test]
    fn config_from_json() {
        let o = json!({"system_prompt":"用中文","override":true,"position":"prepend","models":["glm"]});
        let c = SystemPromptCfg::from_json(&o);
        assert_eq!(c.prompt, "用中文");
        assert!(c.override_existing);
        assert!(c.prepend);
        assert_eq!(c.models, vec!["glm".to_string()]);
    }
}

// ─────────────────────────────────────────────────────────────
// 全局配置（照抄 proxy.rs 的 load/save_and_apply 模式）
// ─────────────────────────────────────────────────────────────

static CFG: std::sync::Mutex<Option<SystemPromptCfg>> = std::sync::Mutex::new(None);

/// 从 data 目录加载（不存在 → 关闭）。
/// 文件存在但解析失败必须出声：静默按「关闭」跑等于用户的提示词悄悄失效。
pub fn load(data_dir: &std::path::Path) -> SystemPromptCfg {
    let p = data_dir.join("sysprompt.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => SystemPromptCfg::from_json(&v),
            Err(e) => {
                tracing::warn!("sysprompt.json 解析失败（{e}），本次不注入系统提示词");
                SystemPromptCfg::default()
            }
        },
        Err(_) => SystemPromptCfg::default(),
    }
}

/// 保存并热更新全局配置。
pub fn save_and_apply(data_dir: &std::path::Path, cfg: &SystemPromptCfg) -> std::io::Result<()> {
    let p = data_dir.join("sysprompt.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = json!({
        "system_prompt": cfg.prompt,
        "override": cfg.override_existing,
        "position": if cfg.prepend { "prepend" } else { "append" },
        "models": cfg.models,
    });
    aigw_store::write_json_atomic(&p, &v)?;
    apply_cfg(cfg);
    Ok(())
}

/// 应用配置到全局（进程内热更新；main 启动时也调一次对齐磁盘值）。
pub fn apply_cfg(cfg: &SystemPromptCfg) {
    // 锁中毒不能让「保存提示词」静默失效：面板回 200、用户以为已生效，实际没注。
    let mut g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(cfg.clone());
}

/// 当前全局配置（未初始化视为关闭）。
pub fn current() -> SystemPromptCfg {
    CFG.lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// 管线入口：用全局配置对请求体注入。返回 true = 改动了。
pub fn apply_global(body: &mut Value, proto: Proto) -> bool {
    let cfg = current();
    apply(body, &cfg, proto)
}

#[cfg(test)]
mod global_tests {
    use super::*;

    #[test]
    fn global_apply_uses_current_config() {
        // 设置全局配置
        let mut c = SystemPromptCfg::default();
        c.prompt = "全局提示".into();
        apply_cfg(&c);
        let mut b = json!({"model":"m","messages":[{"role":"user","content":"hi"}]});
        assert!(apply_global(&mut b, Proto::Chat));
        assert_eq!(b["messages"][0]["content"], "全局提示");

        // 清空后不再注入
        apply_cfg(&SystemPromptCfg::default());
        let mut b2 = json!({"model":"m","messages":[{"role":"user","content":"hi"}]});
        assert!(!apply_global(&mut b2, Proto::Chat));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let d = std::env::temp_dir().join(format!("aigw-sysprompt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let mut c = SystemPromptCfg::default();
        c.prompt = "持久化".into();
        c.override_existing = true;
        c.prepend = true;
        c.models = vec!["glm".into()];
        save_and_apply(&d, &c).unwrap();
        let back = load(&d);
        assert_eq!(back.prompt, "持久化");
        assert!(back.override_existing);
        assert!(back.prepend);
        assert_eq!(back.models, vec!["glm".to_string()]);
        apply_cfg(&SystemPromptCfg::default()); // 清理全局，避免影响其它测试
    }
}
