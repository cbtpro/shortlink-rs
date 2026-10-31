//! 本地中间件编排：把 docker compose 的常用操作收成一句。
//!
//! 应用本身永远跑在宿主机上（热重启靠它），compose 只负责 MySQL 与 Redis。

use crate::shell;

/// 启动依赖。返回是否确认中间件就绪。
///
/// 起不来不直接中止：数据库可能是远端实例，只需要一句提醒。
pub fn ensure() -> bool {
    if !shell::available("docker", &["compose", "version"]) {
        eprintln!("未检测到 docker compose，跳过中间件启动（MySQL/Redis 可能部署在别处）");
        return false;
    }

    println!("启动 MySQL / Redis …");
    if shell::run_checked("docker", &["compose", "up", "-d", "--wait"]) {
        return true;
    }

    // compose < 2.1 没有 --wait
    eprintln!("`--wait` 不可用，改为后台启动，稍后用 `docker compose ps` 确认健康状态");
    shell::run_checked("docker", &["compose", "up", "-d"])
}

pub fn run(action: &str, assume_yes: bool) -> bool {
    if !shell::available("docker", &["compose", "version"]) {
        eprintln!("未检测到 docker compose，无法执行 `db {action}`");
        return false;
    }

    match action {
        "up" => ensure(),
        "down" => shell::run_checked("docker", &["compose", "down"]),
        "restart" => shell::run_checked("docker", &["compose", "restart"]),
        "ps" => shell::run_checked("docker", &["compose", "ps"]),
        "logs" => shell::run_checked("docker", &["compose", "logs", "--tail", "80", "-f"]),
        "reset" => {
            if !assume_yes {
                eprintln!(
                    "`db reset` 会删除数据卷（MySQL 与 Redis 数据全部丢失）。\n\
                     确认要重置请加 --yes：cargo xtask db reset --yes"
                );
                return false;
            }
            println!("删除数据卷并重建 …");
            shell::run_checked("docker", &["compose", "down", "-v"]) && ensure()
        }
        other => {
            eprintln!("未知的 db 动作：{other}（可用：up | down | restart | ps | logs | reset）");
            false
        }
    }
}
