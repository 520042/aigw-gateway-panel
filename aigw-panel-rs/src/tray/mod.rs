//! 系统托盘（Windows）。
//!
//! 当前为占位实现：纯异步 HTTP 服务无需常驻 GUI 托盘。后续接入 `windows-sys`
//! 实现最小化托盘菜单（显示 / 退出 / 开机自启）。接口已预留，不影响编译与服务启动。
#[cfg(windows)]
pub fn run_tray() -> Option<()> {
    None
}

#[cfg(not(windows))]
pub fn run_tray() -> Option<()> {
    None
}
