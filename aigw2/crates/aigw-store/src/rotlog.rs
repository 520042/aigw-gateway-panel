//! 落盘日志：按大小滚动的文件日志，并且同时写 stdout。
//!
//! 为什么需要这个：之前日志只走 stdout，落不落盘取决于别人怎么启动进程——
//! 手动 Start-Process -RedirectStandardOutput _run.out 才有文件，而 _run.out 每次
//! 启动被截断、长期运行也没有上限；用计划任务 Aigw2Service 启动时甚至一行都没有，
//! 出问题只能靠猜。日志是排障的第一现场，不能依赖启动方式。
//!
//! 现在的语义：
//! - 永远写 stdout（保留 _run.out / 容器 stdout 的老用法，管道重定向不失效）；
//! - 同时写 <data>/logs/aigw.log，单文件超过 MAX_BYTES 就滚成 aigw.log.1，
//!   最多保留 KEEP 份历史（总计约 (KEEP+1)*MAX_BYTES，默认约 24MB 封顶）；
//! - 写日志永不影响服务：任何 IO 错误都被吞掉，不让一次磁盘满把请求打死；
//! - 新建的日志文件带 UTF-8 BOM：Windows 上 PowerShell 5.1 的 Get-Content / type
//!   默认按 ANSI 解码，没有 BOM 时中文日志会显示成乱码（实测踩过）。
//!
//! 滚动在每次「取一个 writer」时检查当前文件大小后执行（一条日志一检查），
//! 所以不需要后台任务，也不需要跨线程共享文件句柄。

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing_subscriber::fmt::writer::MakeWriter;

/// 单个日志文件的上限（4 MiB）。
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 保留的历史文件份数（aigw.log.1 … aigw.log.KEEP）。
pub const KEEP: usize = 5;
/// 当前日志文件名。
pub const BASE: &str = "aigw.log";

/// 数据目录下的日志目录。
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// 当前日志族的字节数与文件个数（含滚动出来的历史文件）。
///
/// 只用于 /api/health 观测：面板上看一眼就知道日志是不是在滚、滚到多大了。
pub fn stats(data_dir: &Path) -> (u64, usize) {
    let dir = log_dir(data_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };
    let mut bytes = 0u64;
    let mut files = 0usize;
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !(name == BASE || name.starts_with(&format!("{BASE}."))) {
            continue;
        }
        if let Ok(md) = ent.metadata() {
            if md.is_file() {
                bytes += md.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// 同时写 stdout 与滚动文件的 writer 工厂（tracing 的 MakeWriter）。
#[derive(Clone, Debug)]
pub struct Tee {
    dir: PathBuf,
    max_bytes: u64,
    keep: usize,
    /// 是否同时写 stdout（单元测试里关掉，避免测试输出被日志内容淹没）。
    to_stdout: bool,
}

impl Tee {
    /// 写到 <data_dir>/logs/aigw.log，用默认上限。
    pub fn new(data_dir: &Path) -> io::Result<Self> {
        Self::with_limits(log_dir(data_dir), MAX_BYTES, KEEP)
    }

    /// 指定目录与上限（测试用小上限；生产走 Tee::new）。
    pub fn with_limits(dir: PathBuf, max_bytes: u64, keep: usize) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            max_bytes: max_bytes.max(1),
            keep,
            to_stdout: true,
        })
    }

    /// 只写文件、不写 stdout（测试用；生产永远两个都写）。
    pub fn silent(mut self) -> Self {
        self.to_stdout = false;
        self
    }

    /// 当前日志文件路径。
    pub fn path(&self) -> PathBuf {
        self.dir.join(BASE)
    }

    fn rotated(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{BASE}.{n}"))
    }

    /// 超过上限就滚动。顺序不能反：先把最老的删掉，再从老到新逐级改名，
    /// 否则 .1 会覆盖还没搬走的 .2。
    fn rotate_if_needed(&self) {
        let cur = self.path();
        match std::fs::metadata(&cur) {
            Ok(md) if md.len() >= self.max_bytes => {}
            _ => return,
        }
        if self.keep == 0 {
            let _ = std::fs::remove_file(&cur);
            return;
        }
        let _ = std::fs::remove_file(self.rotated(self.keep));
        for i in (1..self.keep).rev() {
            let from = self.rotated(i);
            if from.exists() {
                let _ = std::fs::rename(&from, self.rotated(i + 1));
            }
        }
        let _ = std::fs::rename(&cur, self.rotated(1));
    }
}

/// 一次日志写入的目标：文件 + stdout。
pub struct TeeWriter {
    file: Option<std::fs::File>,
    out: Option<io::Stdout>,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(f) = self.file.as_mut() {
            // 磁盘满 / 文件被删 / 无权限都不该影响服务，静默放弃这一行即可。
            let _ = f.write_all(buf);
        }
        // stdout 也可能已经被关掉（父进程退出后重定向句柄失效），同样不当错误。
        if let Some(o) = self.out.as_mut() {
            let _ = o.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
        if let Some(o) = self.out.as_mut() {
            let _ = o.flush();
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Tee {
    type Writer = TeeWriter;

    fn make_writer(&self) -> Self::Writer {
        self.rotate_if_needed();
        let path = self.path();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        // 新建（或刚被滚动掉）的日志文件先写 UTF-8 BOM，否则 PowerShell 5.1
        // 的 Get-Content 会把中文按 ANSI 解成乱码。只在空文件上写，不会重复。
        if let Some(f) = file.as_mut() {
            let empty = f.metadata().map(|m| m.len() == 0).unwrap_or(false);
            if empty {
                let _ = f.write_all(&[0xEF, 0xBB, 0xBF]);
            }
        }
        TeeWriter {
            file,
            out: if self.to_stdout { Some(io::stdout()) } else { None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读日志并按「去掉 BOM」后的文本比较（BOM 由 new_files_carry_utf8_bom 专门测）。
    fn read_log(p: &Path) -> String {
        std::fs::read_to_string(p)
            .unwrap()
            .trim_start_matches('\u{feff}')
            .to_string()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn under_cap_stays_in_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = log_dir(tmp.path());
        let tee = Tee::with_limits(dir.clone(), 1024 * 1024, 3).unwrap().silent();
        let mut w = tee.make_writer();
        w.write_all(b"hello ").unwrap();
        w.write_all(b"world\n").unwrap();
        w.flush().unwrap();
        assert_eq!(names(&dir), vec![BASE.to_string()]);
        assert_eq!(read_log(&tee.path()), "hello world\n");
        // stats 按「数据目录」算，日志目录是它的子目录
        let (bytes, files) = stats(tmp.path());
        assert_eq!(files, 1);
        assert_eq!(bytes, 15, "含 3 字节 UTF-8 BOM");
    }

    #[test]
    fn rotates_and_keeps_bounded_history() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = log_dir(tmp.path());
        // 上限 100 字节、只留 2 份历史
        let tee = Tee::with_limits(dir.clone(), 100, 2).unwrap().silent();
        for i in 0..6 {
            let mut w = tee.make_writer();
            // 每次写 150 字节 → 必定超过 100 的上限触发滚动
            w.write_all(format!("{i}").repeat(150).as_bytes()).unwrap();
            w.flush().unwrap();
        }
        let n = names(&dir);
        // 当前 + 2 份历史，绝不无限增长
        assert_eq!(
            n,
            vec![
                BASE.to_string(),
                format!("{BASE}.1"),
                format!("{BASE}.2")
            ],
            "滚动文件集合：{n:?}"
        );
        // 最新的一份是最后一次写入的
        assert!(read_log(&tee.path()).starts_with('5'));
        // 历史里能读到上一份（.1 是第 4 次写入）
        assert!(read_log(&tee.rotated(1)).starts_with('4'));
        let (bytes, files) = stats(tmp.path());
        assert_eq!(files, 3);
        assert_eq!(bytes, 459, "3 个文件各 153 字节（150 内容 + 3 字节 BOM）");
    }

    #[test]
    fn new_files_carry_utf8_bom_for_windows_powershell() {
        let tmp = tempfile::tempdir().unwrap();
        let tee = Tee::with_limits(log_dir(tmp.path()), 1024 * 1024, 3)
            .unwrap()
            .silent();
        let mut w = tee.make_writer();
        w.write_all("中文日志\n".as_bytes()).unwrap();
        w.flush().unwrap();
        let raw = std::fs::read(tee.path()).unwrap();
        assert_eq!(&raw[..3], b"\xEF\xBB\xBF", "文件必须以 UTF-8 BOM 开头");
        assert_eq!(
            String::from_utf8(raw[3..].to_vec()).unwrap(),
            "中文日志\n"
        );
        // 追加第二行不会再加一个 BOM
        let mut w2 = tee.make_writer();
        w2.write_all("第二行\n".as_bytes()).unwrap();
        w2.flush().unwrap();
        let raw2 = std::fs::read(tee.path()).unwrap();
        assert_eq!(
            raw2.windows(3).filter(|w| *w == b"\xEF\xBB\xBF").count(),
            1,
            "只能有一个 BOM"
        );
    }

    #[test]
    fn write_errors_do_not_fail_the_caller() {
        let tmp = tempfile::tempdir().unwrap();
        let tee = Tee::with_limits(log_dir(tmp.path()), 1024, 2).unwrap().silent();
        // 把日志文件换成一个目录：open 会失败，但 writer 仍然成功
        std::fs::create_dir_all(tee.path()).unwrap();
        let mut w = tee.make_writer();
        assert_eq!(w.write(b"still alive").unwrap(), 11);
        assert!(w.flush().is_ok());
        // 目录不是文件，stats 不计入
        assert_eq!(stats(tmp.path()), (0, 0));
    }
}
