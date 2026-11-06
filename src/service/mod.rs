//! 应用服务层：编排领域规则、仓储与缓存，向上暴露与协议无关的用例。

pub mod auth;
pub mod console;
pub mod history;
pub mod my_links;
pub mod redirect;
pub mod shorten;

use crate::error::AppError;

/// 在阻塞线程池里跑一段 CPU 密集的活，并把 panic 转成 500。
///
/// PBKDF2 是故意设计成「算得慢」的，几十万轮下来单次几百毫秒。
/// 直接在 async 函数里跑会占死一个 actix worker 线程 —— worker 数和 CPU 核数相当，
/// 几个人同时登录就能把服务堵住。改密、重置密码同样要走这条路径。
pub(crate) async fn run_blocking<T, F>(job: F) -> Result<T, AppError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(value) => Ok(value),
        // 散列实现 panic 属于程序缺陷，用户侧只该看到 500；细节留给日志
        Err(err) => Err(AppError::Internal(format!(
            "password hashing failed: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blocking_work_hands_back_its_value() {
        assert_eq!(run_blocking(|| 6 * 7).await.unwrap(), 42);
    }

    #[tokio::test]
    async fn a_panicking_job_becomes_a_server_error() {
        // 散列实现 panic 时，用户该看到 500，而不是连接被重置或者 worker 静默少一个
        let result = run_blocking(|| -> u8 { panic!("hasher exploded") }).await;
        assert!(matches!(result, Err(AppError::Internal(_))));
    }
}
