//! 轻量 JSON 文件存储（data/<name>.json）——对齐 Python store 的键值语义。
use serde_json::Value;
use std::path::PathBuf;

fn path_for(name: &str) -> PathBuf {
    crate::core::config::data_dir().join(format!("{}.json", name))
}

pub fn load(name: &str) -> Value {
    match std::fs::read_to_string(path_for(name)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

pub fn save(name: &str, v: &Value) {
    let p = path_for(name);
    let _ = std::fs::create_dir_all(p.parent().unwrap_or_else(|| std::path::Path::new(".")));
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = std::fs::write(p, s);
    }
}
