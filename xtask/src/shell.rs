//! 子进程封装：统一「要不要中断流程」的判断，避免每个命令重复写一遍。

use std::io;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// 应用的产物名，按平台补后缀。
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "shortlink-rs.exe"
    } else {
        "shortlink-rs"
    }
}

pub fn run(program: &str, args: &[&str]) -> io::Result<ExitStatus> {
    Command::new(program).args(args).status()
}

/// 执行并返回是否成功。命令不存在也算失败，但会说清是哪个命令。
pub fn run_checked(program: &str, args: &[&str]) -> bool {
    match run(program, args) {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("无法执行 `{program}`：{err}");
            false
        }
    }
}

/// 在指定目录里执行。前端测试要跑在 `web-test/` 下，才能按自己的
/// `node_modules` 解析依赖，而不是跟着调用者的当前目录漂。
pub fn run_in(dir: &Path, program: &str, args: &[&str]) -> io::Result<ExitStatus> {
    Command::new(program).args(args).current_dir(dir).status()
}

pub fn run_checked_in(dir: &Path, program: &str, args: &[&str]) -> bool {
    match run_in(dir, program, args) {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("无法在 {} 执行 `{program}`：{err}", dir.display());
            false
        }
    }
}

/// 只关心「有没有这个命令」，用于探测 docker 是否可用。
pub fn available(program: &str, probe: &[&str]) -> bool {
    Command::new(program)
        .args(probe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
