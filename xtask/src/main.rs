//! 项目自带的构建工具，用法见 `cargo xtask help`。
//!
//! 放在独立 crate 里而不是 shell 脚本：Windows、Linux、macOS 共用一套逻辑，
//! 也不依赖 bash、make 或 node。

mod dev;
mod infra;
mod pipeline;
mod shell;
mod watch;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => {
            print_help();
            return ExitCode::FAILURE;
        }
    };

    let ok = match command {
        "dev" => match parse_dev_options(rest) {
            Ok(options) => dev::run(options),
            Err(message) => {
                eprintln!("{message}");
                false
            }
        },
        "build" => pipeline::build(false),
        "release" => pipeline::release(),
        "check" => pipeline::check(),
        "fmt" => pipeline::fmt(rest.iter().any(|arg| arg == "--check")),
        "db" => match rest.first() {
            Some(action) => infra::run(action, rest.iter().any(|arg| arg == "--yes")),
            None => {
                eprintln!("用法：cargo xtask db <up|down|restart|ps|logs|reset>");
                false
            }
        },
        "help" | "--help" | "-h" => {
            print_help();
            true
        }
        other => {
            eprintln!("未知命令：{other}\n");
            print_help();
            false
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn parse_dev_options(args: &[String]) -> Result<dev::Options, String> {
    let mut options = dev::Options {
        port: None,
        infra: true,
        once: false,
        release: false,
    };

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "--port 需要一个端口号".to_string())?;
                options.port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("`{value}` 不是合法端口"))?,
                );
            }
            "--no-infra" => options.infra = false,
            // 只构建并运行一次，供 CI 或脚本使用
            "--once" => {
                options.once = true;
                options.infra = false;
            }
            "--release" => options.release = true,
            other => return Err(format!("dev 不支持参数：{other}")),
        }
        index += 1;
    }

    Ok(options)
}

fn print_help() {
    println!(
        "shortlink-rs 开发工具

用法：cargo xtask <命令> [参数]

命令：
  dev [--port N] [--no-infra] [--once] [--release]
      构建并启动服务，改 Rust 代码自动重建重启（默认先拉起 MySQL/Redis）
        --port N     覆盖监听端口（等价于 SERVER_PORT=N）
        --no-infra   不启动 docker compose
        --once       只构建并运行一次，不监听文件
        --release    用 release 产物跑，便于复现线上行为

  build              调试构建
  release            release 构建并组装 dist/（可执行文件 + 配置目录）
  check              依次执行 fmt --check、clippy -D warnings、test
  fmt [--check]      格式化（--check 只检查）

  db <动作>          本地中间件：up | down | restart | ps | logs | reset
                     reset 会删数据卷，必须加 --yes"
    );
}
