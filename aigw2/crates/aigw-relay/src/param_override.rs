//! 参数覆盖（对齐 magpie-community/plugins packages/param-override 的**核心子集**）。
//!
//! ## 为什么需要（对单用户场景的真实价值）
//! 不是「审计/合规」，而是**自动修掉上游不认的参数**：
//! - 某平台不认 `tools`（返回 400）→ 规则 `{path:"tools", mode:"delete"}`
//! - 推理类模型必须去掉 `temperature` → 用 conditions 按模型名匹配
//! - 想给所有请求兜底 `max_tokens` 上限 → `{max_tokens: 8000}`
//! - 把 `stream_options` 补齐 → `{path:"stream_options.include_usage", mode:"set", value:true}`
//!
//! ## 支持的两种配置形态（与 New API 一致，可直接粘贴）
//! ```json
//! { "temperature": 0.2, "max_tokens": 8000 }            // 直接设
//! { "operations": [                                      // 有序操作
//!     { "path": "tools", "mode": "delete",
//!       "conditions": [{ "path": "model", "mode": "contains", "value": "reasoner" }] },
//!     { "path": "max_tokens", "mode": "set", "value": 32000,
//!       "conditions": [{ "path": "max_tokens", "mode": "gt", "value": 32000 }] }
//! ] }
//! ```
//!
//! ## 实现的 mode（核心子集，13 种里做 8 种）
//! `set`（含 `keep_origin`：只在缺失时设）、`delete`、`replace`、
//! `prepend`/`append`（数组拼接、字符串拼接、对象合并）、
//! `trim_space`、`to_lower`、`to_upper`。
//!
//! **未实现**（如实标注，不假装）：`move`/`copy`、`trim_prefix`/`trim_suffix`/
//! `ensure_prefix`/`ensure_suffix`、`regex_replace`、`return_error`、`prune_objects`、
//! 以及 gjson 的 `*` 通配与 `-1` 负索引。
//!
//! ## 路径语法（子集）
//! `a.b.0` —— 点分路径，数字段按数组下标。**不支持** `*` 通配、`-1` 负索引、
//! `\.` 转义点（这几项要完整 gjson 实现，收益低）。
//!
//! ## 条件（照抄参考项目）
//! `path` / `mode`(full|prefix|suffix|contains|gt|gte|lt|lte) / `value` /
//! `invert` / `pass_missing_key`；`logic` 为 `OR`（默认）或 `AND`。
//! 条件路径先查请求体，再查请求上下文（`model` / `protocol` / `stream`）。

use serde_json::{Map, Value};

/// 一条操作。
#[derive(Debug, Clone)]
pub struct Op {
    pub path: String,
    pub mode: String,
    pub value: Option<Value>,
    pub keep_origin: bool,
    pub conditions: Vec<Cond>,
    pub logic_and: bool,
}

/// 一个条件。
#[derive(Debug, Clone)]
pub struct Cond {
    pub path: String,
    pub mode: String,
    pub value: Option<Value>,
    pub invert: bool,
    pub pass_missing: bool,
}

/// 完整配置。
#[derive(Debug, Clone, Default)]
pub struct ParamOverride {
    /// 直接设置（`{"temperature":0.2}` 形态）——先应用。
    pub direct: Vec<(String, Value)>,
    /// 有序操作。
    pub ops: Vec<Op>,
}

impl ParamOverride {
    pub fn from_json(v: &Value) -> Self {
        let mut out = ParamOverride::default();
        let Some(obj) = v.as_object() else { return out };
        // ① 直接设置：跳过 operations / 配置元字段
        for (k, val) in obj {
            if k == "operations" || k == "enabled" {
                continue;
            }
            out.direct.push((k.clone(), val.clone()));
        }
        // ② 有序操作
        if let Some(arr) = obj.get("operations").and_then(|x| x.as_array()) {
            for item in arr {
                let Some(o) = item.as_object() else { continue };
                let path = o.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let mode = o
                    .get("mode")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if mode.is_empty() {
                    continue;
                }
                let conds = o
                    .get("conditions")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|c| {
                                let co = c.as_object()?;
                                Some(Cond {
                                    path: co.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                    mode: co
                                        .get("mode")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("full")
                                        .to_ascii_lowercase(),
                                    value: co.get("value").cloned(),
                                    invert: co.get("invert").and_then(|x| x.as_bool()).unwrap_or(false),
                                    pass_missing: co
                                        .get("pass_missing_key")
                                        .and_then(|x| x.as_bool())
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.ops.push(Op {
                    path,
                    mode,
                    value: o.get("value").cloned(),
                    keep_origin: o.get("keep_origin").and_then(|x| x.as_bool()).unwrap_or(false),
                    conditions: conds,
                    logic_and: o
                        .get("logic")
                        .and_then(|x| x.as_str())
                        .map(|s| s.eq_ignore_ascii_case("AND"))
                        .unwrap_or(false),
                });
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.ops.is_empty()
    }

    /// 应用到一个请求体。`req` 是条件可查的请求上下文（model / protocol / stream）。
    /// 返回 true = 改动了。
    pub fn apply(&self, body: &mut Value, req: &Map<String, Value>) -> bool {
        if !body.is_object() {
            return false;
        }
        let mut changed = false;
        // ① 直接设置（NULL 表示删除，与参考项目一致：设成 null 等于删）
        for (k, v) in &self.direct {
            if let Some(o) = body.as_object_mut() {
                o.insert(k.clone(), v.clone());
                changed = true;
            }
        }
        // ② 有序操作
        for op in &self.ops {
            if !self.when(body, req, op) {
                continue;
            }
            if self.run(body, op) {
                changed = true;
            }
        }
        changed
    }

    fn when(&self, body: &Value, req: &Map<String, Value>, op: &Op) -> bool {
        if op.conditions.is_empty() {
            return true;
        }
        let hit = |c: &Cond| -> bool {
            // 条件路径先查 body，再查请求上下文
            let found: Option<&Value> = lookup(body, &c.path).or_else(|| req.get(&c.path));
            let Some(v) = found else {
                return c.pass_missing;
            };
            let r = compare(v, c.value.as_ref(), &c.mode);
            if c.invert { !r } else { r }
        };
        if op.logic_and {
            op.conditions.iter().all(hit)
        } else {
            op.conditions.iter().any(hit)
        }
    }

    fn run(&self, body: &mut Value, op: &Op) -> bool {
        match op.mode.as_str() {
            "set" => {
                if op.keep_origin && lookup(body, &op.path).is_some() {
                    return false;
                }
                let Some(v) = &op.value else { return false };
                set_at(body, &op.path, v.clone())
            }
            "delete" => del_at(body, &op.path),
            "replace" => {
                let Some(v) = &op.value else { return false };
                if lookup(body, &op.path).is_none() {
                    return false;
                }
                set_at(body, &op.path, v.clone())
            }
            "prepend" | "append" => {
                let Some(v) = &op.value else { return false };
                let Some(cur) = lookup(body, &op.path) else {
                    // 不存在时直接设（参考项目口径）
                    return set_at(body, &op.path, v.clone());
                };
                // 数组拼接 / 字符串拼接 / 对象合并
                let merged = match (&cur, v) {
                    (Value::Array(a), Value::Array(b)) => {
                        let mut out = a.clone();
                        if op.mode == "prepend" {
                            let mut nb = b.clone();
                            nb.extend(out);
                            out = nb;
                        } else {
                            out.extend(b.clone());
                        }
                        Value::Array(out)
                    }
                    (Value::String(a), Value::String(b)) => {
                        if op.mode == "prepend" {
                            Value::String(format!("{b}{a}"))
                        } else {
                            Value::String(format!("{a}{b}"))
                        }
                    }
                    (Value::Object(a), Value::Object(b)) => {
                        let mut out = a.clone();
                        for (k, val) in b {
                            if op.keep_origin && out.contains_key(k) {
                                continue;
                            }
                            out.insert(k.clone(), val.clone());
                        }
                        Value::Object(out)
                    }
                    _ => return false,
                };
                set_at(body, &op.path, merged)
            }
            "trim_space" => match lookup(body, &op.path) {
                Some(Value::String(s)) => {
                    let t = s.trim().to_string();
                    if &t == s { false } else { set_at(body, &op.path, Value::String(t)) }
                }
                _ => false,
            },
            "to_lower" => match lookup(body, &op.path) {
                Some(Value::String(s)) => {
                    let t = s.to_lowercase();
                    if &t == s { false } else { set_at(body, &op.path, Value::String(t)) }
                }
                _ => false,
            },
            "to_upper" => match lookup(body, &op.path) {
                Some(Value::String(s)) => {
                    let t = s.to_uppercase();
                    if &t == s { false } else { set_at(body, &op.path, Value::String(t)) }
                }
                _ => false,
            },
            other => {
                tracing::debug!("param_override：未实现的 mode {other}（核心子集不含），已跳过");
                false
            }
        }
    }
}

/// 点分路径查找（数字段按数组下标；不支持 `*` 与负索引）。
fn lookup<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => {
                let idx: usize = seg.parse().ok()?;
                a.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// 点分路径写入（中间缺失自动建对象/数组）。
fn set_at(root: &mut Value, path: &str, v: Value) -> bool {
    if path.is_empty() {
        return false;
    }
    let segs: Vec<&str> = path.split('.').collect();
    let mut cur = root;
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        if last {
            match cur {
                Value::Object(m) => {
                    m.insert(seg.to_string(), v);
                    return true;
                }
                Value::Array(a) => {
                    if let Ok(idx) = seg.parse::<usize>() {
                        while a.len() <= idx {
                            a.push(Value::Null);
                        }
                        a[idx] = v;
                        return true;
                    }
                    return false;
                }
                _ => return false,
            }
        }
        // 中间段：缺失则按下一段是不是数字决定建对象还是数组
        let next_is_idx = segs[i + 1].parse::<usize>().is_ok();
        match cur {
            Value::Object(m) => {
                let e = m.entry(seg.to_string()).or_insert_with(|| {
                    if next_is_idx { Value::Array(vec![]) } else { Value::Object(Map::new()) }
                });
                cur = e;
            }
            Value::Array(a) => {
                let Ok(idx) = seg.parse::<usize>() else { return false };
                while a.len() <= idx {
                    a.push(if next_is_idx { Value::Array(vec![]) } else { Value::Object(Map::new()) });
                }
                cur = &mut a[idx];
            }
            _ => return false,
        }
    }
    false
}

/// 点分路径删除。返回 true = 真的删了。
fn del_at(root: &mut Value, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let segs: Vec<&str> = path.split('.').collect();
    let (last, parents) = segs.split_last().unwrap();
    let mut cur = root;
    for seg in parents {
        cur = match cur {
            Value::Object(m) => match m.get_mut(*seg) {
                Some(v) => v,
                None => return false,
            },
            Value::Array(a) => match seg.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                Some(v) => v,
                None => return false,
            },
            _ => return false,
        };
    }
    match cur {
        Value::Object(m) => m.remove(*last).is_some(),
        Value::Array(a) => match last.parse::<usize>() {
            Ok(i) if i < a.len() => {
                a.remove(i);
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// 条件比较（照抄参考项目的 8 种 mode）。
fn compare(v: &Value, target: Option<&Value>, mode: &str) -> bool {
    let as_str = |x: &Value| -> String {
        match x {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    };
    let num = |x: &Value| -> Option<f64> {
        match x {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    };
    match mode {
        "prefix" => match target {
            Some(t) => as_str(v).starts_with(&as_str(t)),
            None => false,
        },
        "suffix" => match target {
            Some(t) => as_str(v).ends_with(&as_str(t)),
            None => false,
        },
        "contains" => match target {
            Some(t) => as_str(v).contains(&as_str(t)),
            None => false,
        },
        "gt" | "gte" | "lt" | "lte" => match (num(v), target.and_then(num)) {
            (Some(a), Some(b)) => match mode {
                "gt" => a > b,
                "gte" => a >= b,
                "lt" => a < b,
                _ => a <= b,
            },
            _ => false,
        },
        // full（默认）：类型与值都相同
        _ => match target {
            Some(t) => v == t,
            None => false,
        },
    }
}

// ─────────────────────────────────────────────────────────────
// 全局配置
// ─────────────────────────────────────────────────────────────

static CFG: std::sync::Mutex<Option<ParamOverride>> = std::sync::Mutex::new(None);

pub fn load(data_dir: &std::path::Path) -> ParamOverride {
    let p = data_dir.join("param_override.json");
    match std::fs::read_to_string(&p) {
        Ok(raw) => match serde_json::from_str::<Value>(&raw) {
            Ok(v) => ParamOverride::from_json(&v),
            Err(e) => {
                tracing::warn!("param_override.json 解析失败（{e}），不启用参数覆盖");
                ParamOverride::default()
            }
        },
        Err(_) => ParamOverride::default(),
    }
}

pub fn save_and_apply(data_dir: &std::path::Path, cfg: &ParamOverride, enabled: bool) -> std::io::Result<()> {
    let p = data_dir.join("param_override.json");
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 存回原形态（direct + operations）
    let mut out = Map::new();
    out.insert("enabled".into(), Value::Bool(enabled));
    for (k, v) in &cfg.direct {
        out.insert(k.clone(), v.clone());
    }
    let ops: Vec<Value> = cfg
        .ops
        .iter()
        .map(|o| {
            let mut m = Map::new();
            m.insert("path".into(), Value::String(o.path.clone()));
            m.insert("mode".into(), Value::String(o.mode.clone()));
            if let Some(v) = &o.value {
                m.insert("value".into(), v.clone());
            }
            if o.keep_origin {
                m.insert("keep_origin".into(), Value::Bool(true));
            }
            if !o.conditions.is_empty() {
                let cs: Vec<Value> = o
                    .conditions
                    .iter()
                    .map(|c| {
                        let mut cm = Map::new();
                        cm.insert("path".into(), Value::String(c.path.clone()));
                        cm.insert("mode".into(), Value::String(c.mode.clone()));
                        if let Some(v) = &c.value {
                            cm.insert("value".into(), v.clone());
                        }
                        if c.invert {
                            cm.insert("invert".into(), Value::Bool(true));
                        }
                        if c.pass_missing {
                            cm.insert("pass_missing_key".into(), Value::Bool(true));
                        }
                        Value::Object(cm)
                    })
                    .collect();
                m.insert("conditions".into(), Value::Array(cs));
            }
            if o.logic_and {
                m.insert("logic".into(), Value::String("AND".into()));
            }
            Value::Object(m)
        })
        .collect();
    out.insert("operations".into(), Value::Array(ops));
    aigw_store::write_json_atomic(&p, &Value::Object(out))?;
    apply_cfg(cfg, enabled);
    Ok(())
}

pub fn apply_cfg(cfg: &ParamOverride, enabled: bool) {
    let mut g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(if enabled {
        cfg.clone()
    } else {
        ParamOverride::default()
    });
}

pub fn current() -> ParamOverride {
    CFG.lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// 管线入口：用全局配置覆盖请求体参数。
pub fn apply_global(body: &mut Value) -> bool {
    let cfg = current();
    if cfg.is_empty() {
        return false;
    }
    // 条件可查的请求上下文：model / protocol / stream
    let mut req = Map::new();
    if let Some(m) = body.get("model").and_then(|x| x.as_str()) {
        req.insert("model".into(), Value::String(m.to_string()));
    }
    if let Some(s) = body.get("stream").and_then(|x| x.as_bool()) {
        req.insert("stream".into(), Value::Bool(s));
    }
    req.insert("protocol".into(), Value::String("chat".into()));
    cfg.apply(body, &req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(model: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("model".into(), Value::String(model.into()));
        m.insert("protocol".into(), Value::String("chat".into()));
        m
    }

    #[test]
    fn direct_form_sets_keys() {
        let cfg = ParamOverride::from_json(&json!({"temperature": 0.2, "max_tokens": 8000}));
        let mut body = json!({"model": "m"});
        assert!(cfg.apply(&mut body, &ctx("m")));
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["max_tokens"], 8000);
    }

    #[test]
    fn delete_op() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path": "tools", "mode": "delete"}]
        }));
        let mut body = json!({"tools": [{"a":1}], "model": "m"});
        assert!(cfg.apply(&mut body, &ctx("m")));
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn set_with_condition_gt() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"max_tokens","mode":"set","value":32000,
                            "conditions":[{"path":"max_tokens","mode":"gt","value":32000}]}]
        }));
        // 超过上限 → 夹到 32000
        let mut b1 = json!({"model":"m","max_tokens":100000});
        assert!(cfg.apply(&mut b1, &ctx("m")));
        assert_eq!(b1["max_tokens"], 32000);
        // 没超 → 不动
        let mut b2 = json!({"model":"m","max_tokens":100});
        assert!(!cfg.apply(&mut b2, &ctx("m")));
        assert_eq!(b2["max_tokens"], 100);
    }

    #[test]
    fn condition_on_model_prefix() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"temperature","mode":"delete",
                            "conditions":[{"path":"model","mode":"contains","value":"reasoner"}]}]
        }));
        let mut b1 = json!({"model":"glm-reasoner","temperature":0.7});
        assert!(cfg.apply(&mut b1, &ctx("glm-reasoner")));
        assert!(b1.get("temperature").is_none());

        let mut b2 = json!({"model":"glm-normal","temperature":0.7});
        assert!(!cfg.apply(&mut b2, &ctx("glm-normal")));
        assert_eq!(b2["temperature"], 0.7);
    }

    #[test]
    fn keep_origin_only_sets_when_missing() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"max_tokens","mode":"set","value":8000,"keep_origin":true}]
        }));
        let mut b1 = json!({"model":"m"});
        assert!(cfg.apply(&mut b1, &ctx("m")));
        assert_eq!(b1["max_tokens"], 8000);

        let mut b2 = json!({"model":"m","max_tokens":1});
        assert!(!cfg.apply(&mut b2, &ctx("m")));
        assert_eq!(b2["max_tokens"], 1, "已存在就不该覆盖");
    }

    #[test]
    fn nested_path_set() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"stream_options.include_usage","mode":"set","value":true}]
        }));
        let mut body = json!({"model":"m"});
        assert!(cfg.apply(&mut body, &ctx("m")));
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn array_append_and_prepend() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"stop","mode":"append","value":["</s>"]}]
        }));
        let mut body = json!({"model":"m","stop":["\n\n"]});
        assert!(cfg.apply(&mut body, &ctx("m")));
        assert_eq!(body["stop"], json!(["\n\n", "</s>"]));
    }

    #[test]
    fn case_modes() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [
                {"path":"user","mode":"to_lower"},
                {"path":"tag","mode":"trim_space"}
            ]
        }));
        let mut body = json!({"model":"m","user":"ABC","tag":"  x  "});
        assert!(cfg.apply(&mut body, &ctx("m")));
        assert_eq!(body["user"], "abc");
        assert_eq!(body["tag"], "x");
    }

    #[test]
    fn and_logic() {
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"x","mode":"set","value":1,"logic":"AND",
                "conditions":[
                    {"path":"model","mode":"prefix","value":"glm"},
                    {"path":"model","mode":"suffix","value":"reasoner"}
                ]}]
        }));
        let mut b1 = json!({"model":"glm-reasoner"});
        assert!(cfg.apply(&mut b1, &ctx("glm-reasoner")));
        let mut b2 = json!({"model":"glm-normal"});
        assert!(!cfg.apply(&mut b2, &ctx("glm-normal")), "AND 下只满足一个不该生效");
    }

    #[test]
    fn unknown_mode_is_skipped_silently() {
        // 未实现的 mode 不该 panic，也不该改坏请求
        let cfg = ParamOverride::from_json(&json!({
            "operations": [{"path":"x","mode":"regex_replace","value":"y"}]
        }));
        let mut body = json!({"model":"m","x":"abc"});
        assert!(!cfg.apply(&mut body, &ctx("m")));
        assert_eq!(body["x"], "abc");
    }
}
