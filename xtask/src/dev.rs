//! `cargo xtask dev`：构建 → 启动 → 监听文件 → 重建重启。
//!
//! 前端不需要重启：开发模式下资源是每次请求读磁盘的，浏览器那边靠
//! `/__dev/revision` 轮询自己刷新。所以这里只盯 Rust 源码与配置。

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::shell;
use crate::watch::Snapshot;

const POLL_INTERVAL: Duration = Duration::from_millis(400);
const DEFAULT_DEV_PORT: u16 = 9981;

pub struct Options {
    pub port: Option<u16>,
    pub infra: bool,
    pub once: bool,
    pub release: bool,
}

pub fn run(options: Options) -> bool {
    if options.infra {
        crate::infra::ensure();
    }

    let roots = watch_roots();
    let mut snapshot = Snapshot::capture(&roots);

    loop {
        if !crate::pipeline::build(options.release) {
            eprintln!("\n构建失败。修好后会自动重试（Ctrl-C 退出）");

            if options.once {
                return false;
            }
            snapshot.wait_for_change(&roots);
            continue;
        }

        let mut child = match spawn(&options) {
            Ok(child) => child,
            Err(err) => {
                eprintln!("无法启动服务：{err}");
                return false;
            }
        };

        if options.once {
            return child.wait().map(|status| status.success()).unwrap_or(false);
        }

        print_hints(&options);

        match wait_for_trigger(&mut child, &roots, &mut snapshot) {
            Trigger::Changed => {
                stop(&mut child);
                println!("\n检测到改动，重新构建 …\n");
            }
            Trigger::Exited(status) => {
                match status {
                    Some(status) => println!("\n服务已退出（{status}），等待文件变化后重启"),
                    None => println!("\n服务已退出，等待文件变化后重启"),
                }
                snapshot.wait_for_change(&roots);
            }
        }
    }
}

enum Trigger {
    Changed,
    Exited(Option<ExitStatus>),
}

/// 同时盯着「文件变了」和「进程没了」：后者通常是端口被占用或配置写错，
/// 不该让开发工具陷入反复重启的死循环。
fn wait_for_trigger(child: &mut Child, roots: &[PathBuf], snapshot: &mut Snapshot) -> Trigger {
    loop {
        std::thread::sleep(POLL_INTERVAL);

        match child.try_wait() {
            Ok(Some(status)) => return Trigger::Exited(Some(status)),
            Ok(None) => {}
            Err(err) => {
                eprintln!("无法获取子进程状态：{err}");
                return Trigger::Exited(None);
            }
        }

        if snapshot.changed_since(roots) {
            *snapshot = Snapshot::capture(roots);
            return Trigger::Changed;
        }
    }
}

fn spawn(options: &Options) -> std::io::Result<Child> {
    let profile = if options.release { "release" } else { "debug" };
    let binary = PathBuf::from("target")
        .join(profile)
        .join(shell::binary_name());

    let mut command = Command::new(binary);
    if let Some(port) = options.port {
        // 用环境变量覆盖，和部署时改配置的方式保持一致
        command.env("SERVER_PORT", port.to_string());
    }

    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn print_hints(options: &Options) {
    let port = options
        .port
        .or_else(|| {
            std::env::var("SERVER_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(DEFAULT_DEV_PORT);

    println!("  首页    http://127.0.0.1:{port}/");
    println!("  后台    http://127.0.0.1:{port}/admin");
    println!("  健康    http://127.0.0.1:{port}/healthz");
    println!("\n改 Rust 代码自动重启；改 web/ 下文件刷新浏览器即可（Ctrl-C 退出）\n");
}

/// 只盯需要重新编译的路径。`web/` 不在其中 —— 前端走磁盘读取。
fn watch_roots() -> Vec<PathBuf> {
    ["src", "config", "xtask/src"]
        .into_iter()
        .map(PathBuf::from)
        .chain(
            ["Cargo.toml", "build.rs", "xtask/Cargo.toml"]
                .into_iter()
                .map(PathBuf::from),
        )
        .filter(|path| path.exists())
        .collect()
}
