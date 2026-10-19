//! 面板配置：端口 / 绑定 / 静态目录 / 上游透传 / 面板令牌。
//!
//! 开箱即用时 `upstream_base` 留空，所有模型默认走 Pollinations 免密钥车道
//! （见 [`crate::relay`]）；仅当用户填入自有上游 key 才透传。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub panel_token: String,
    pub port: u16,
    pub bind_host: String,
    pub static_dir: String,
    /// /v1/chat/completions 上游（用户自有 OpenAI 兼容端点，如 OpenRouter）。
    pub upstream_base: String,
    pub upstream_key: String,
    /// Lobster 反代自定义服务器（@lobster 规格占位符替换）。
    pub lobster_server: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            panel_token: String::new(),
            port: 8800,
            bind_host: "127.0.0.1".to_string(),
            static_dir: default_static_dir(),
            // ★ 开箱即走免费车道：具体模型与 auto/free 都落 Pollinations，
            //   避免误填 OpenRouter 导致具体模型 404。
            upstream_base: String::new(),
            upstream_key: String::new(),
            lobster_server: String::new(),
        }
    }
}

pub fn default_static_dir() -> String {
    if let Ok(exe) = std::env::current_exe() {
        // ★ 2026-10-15：symlink 启动（桌面快捷方式）时 `current_exe()` 可能是**链接自身**
        //   的路径，且进程 CWD 不一定等于 exe 目录 —— 只靠 `../../aigw-panel/app/static`
        //   和相对 "app/static" 都会失配，导致 /app.js 等**全部 404（前端白屏）**。
        //   这里：① 先 canonicalize 去掉链接；② 再按「便携安装位 exe 同级 app/static」
        //   →「开发期 target 下」的顺序找。全部转成绝对路径，与 CWD 无关。
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(parent) = exe.parent() {
            let cands = [
                parent.join("app/static"),                   // 便携安装位：<install>/app/static
                parent.join("../../aigw-panel/app/static"),  // 开发期：dist 在 target 下
                parent.join("../app/static"),
            ];
            for cand in cands {
                if cand.is_dir() {
                    // canonicalize 在 Windows 会带 `\\?\` 扩展前缀；去掉更干净（仅做 fs join，去掉无碍）
                    let s = cand.to_string_lossy().into_owned();
                    return s.strip_prefix(r"\\?\").map(|x| x.to_string()).unwrap_or(s);
                }
            }
        }
    }
    if let Ok(p) = std::env::var("AIGW_STATIC_DIR") {
        return p;
    }
    "app/static".to_string()
}

pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            return p.join("data");
        }
    }
    PathBuf::from("data")
}

pub fn load_settings() -> Settings {
    let path = data_dir().join("settings.json");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Settings>(&s) {
            return v;
        }
    }
    Settings::default()
}

pub fn save_settings(s: &Settings) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(s) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(dir.join("settings.json"), s);
    }
}

/// 给前端回显时隐藏敏感字段。
pub fn settings_public(s: &Settings) -> serde_json::Value {
    let key_set = !s.upstream_key.is_empty();
    serde_json::json!({
        "panel_token": if s.panel_token.is_empty() { "" } else { "******" },
        "port": s.port,
        "bind_host": s.bind_host,
        "static_dir": s.static_dir,
        "upstream_base": s.upstream_base,
        "upstream_key_set": key_set,
        "lobster_server": s.lobster_server,
    })
}

#[allow(dead_code)]
pub fn is_hidden(_p: &Path) -> bool {
    false
}
