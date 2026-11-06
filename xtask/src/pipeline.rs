//! 构建与质量闸门。

use std::path::Path;

use crate::shell;

pub fn build(release: bool) -> bool {
    let mut args = vec!["build"];
    if release {
        args.push("--release");
        // 发布必须可复现：锁文件漂移直接失败，而不是悄悄升级依赖
        args.push("--locked");
    }

    println!("$ cargo {}", args.join(" "));
    shell::run_checked("cargo", &args)
}

/// 发布：release 构建 + 组装可直接拷走的 `dist/`。
pub fn release() -> bool {
    if !build(true) {
        return false;
    }

    if let Err(err) = assemble_dist() {
        eprintln!("组装 dist 失败：{err}");
        return false;
    }

    true
}

pub fn fmt(check: bool) -> bool {
    let args = if check {
        vec!["fmt", "--all", "--", "--check"]
    } else {
        vec!["fmt", "--all"]
    };

    println!("$ cargo {}", args.join(" "));
    shell::run_checked("cargo", &args)
}

/// 提交前跑一遍：格式、lint、测试。任一失败即中断。
pub fn check() -> bool {
    if !fmt(true) {
        eprintln!("格式不符合 rustfmt，执行 `cargo xtask fmt` 修正");
        return false;
    }

    println!("$ cargo clippy --all-targets -- -D warnings");
    if !shell::run_checked(
        "cargo",
        &["clippy", "--all-targets", "--", "-D", "warnings"],
    ) {
        return false;
    }

    println!("$ cargo test");
    if !shell::run_checked("cargo", &["test"]) {
        return false;
    }

    if !web_test(false) {
        eprintln!("前端用例未通过，单独复跑：cargo xtask web-test");
        return false;
    }

    true
}

/// 前端测试的工作目录。
const WEB_TEST_DIR: &str = "web-test";

/// 前端单元测试：`node --test` + jsdom，被测的是 `web/` 里那几个文件本身，
/// 不打包、不转译 —— 断言跑的就是线上加载的同一份代码。
///
/// 依赖不参与 Rust 构建，所以分两种口径：独立命令（`strict`）缺依赖直接失败，
/// 免得以为「跑过了」；`check` 里则跳过并给出安装命令，不让没装 node 的机器被卡住。
pub fn web_test(strict: bool) -> bool {
    let dir = Path::new(WEB_TEST_DIR);

    if !dir.join("node_modules").is_dir() {
        let hint = "前端测试的依赖还没装：cd web-test && npm install";
        if strict {
            eprintln!("{hint}");
            return false;
        }
        println!("跳过前端测试（{hint}）");
        return true;
    }

    println!("$ node --test   （工作目录 web-test/）");
    shell::run_checked_in(dir, "node", &["--test"])
}

fn assemble_dist() -> std::io::Result<()> {
    let dist = Path::new("dist");
    std::fs::create_dir_all(dist)?;

    let binary = Path::new("target")
        .join("release")
        .join(shell::binary_name());
    let target = dist.join(shell::binary_name());
    std::fs::copy(&binary, &target)?;

    // 配置目录必须跟着走：base.env 已编进二进制，但 profile 覆盖层在磁盘上
    copy_dir(Path::new("config"), &dist.join("config"))?;
    if Path::new(".env.example").is_file() {
        std::fs::copy(".env.example", dist.join(".env.example"))?;
    }

    let size = std::fs::metadata(&target)
        .map(|meta| meta.len())
        .unwrap_or(0);
    println!(
        "\n发布产物：{}\n  可执行文件 {}（{:.1} MB）\n  配置目录   {}/\n\n启动方式：\n  APP_ENV=production DATABASE_URL=... REDIS_URL=... ADMIN_EMAIL=... ./{}",
        dist.display(),
        target.display(),
        size as f64 / 1024.0 / 1024.0,
        dist.join("config").display(),
        shell::binary_name()
    );

    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;

    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let destination = to.join(entry.file_name());

        if entry.file_type()?.is_dir() {
            copy_dir(&path, &destination)?;
        } else {
            std::fs::copy(&path, &destination)?;
        }
    }

    Ok(())
}
