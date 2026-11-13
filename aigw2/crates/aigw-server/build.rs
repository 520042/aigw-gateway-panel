//! 构建期自证：把当前 git 提交嵌进二进制。
//!
//! 为什么：关于页此前只能并列显示「本地版本号 + 远端 commit 哈希」，
//! 不给「已是最新 / 不一致」的判定——因为运行中的 exe 不知道自己是
//! 哪个提交构建的，后端注释里明确拒绝了「假算术」。有了构建期 sha，
//! 前端就能做真实比对（sha 相同 = 已是最新；不同 = 落后或含未推送改动）。
//!
//! 无 git 环境（如直接拷源码编译）时 sha 为空串，前端据此回退到旧行为，
//! 不显示判定——宁可少说，不说错话。

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // 仓库根在包目录上两级（工作区根）。仓库元数据变化时尽量重跑，
    // 让新提交能刷新 sha；漏判时靠部署流程的 touch lib.rs 兜底
    // （包内任何文件变化都会触发 build.rs 重跑）。
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=AIGW_GIT_SHA={sha}");

    // 工作区是否干净也一并记录：sha 相同但工作区脏 = 构建内容可能 ≠ 提交内容。
    // ★ 只看**已跟踪**文件（--untracked-files=no）：日志、截图、杂项目录
    //   这类未跟踪产物不影响构建，把它们算成脏会让「✓ 已是最新」永远亮不出来。
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=AIGW_GIT_DIRTY={}", dirty);
}
