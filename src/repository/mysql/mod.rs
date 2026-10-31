//! MySQL 适配层。
//!
//! 短链与账号各一个实现，共用同一个连接池与错误判定：两边的唯一键冲突
//! 都要落到「业务可解释的原因」（短码重复 / 邮箱已注册），不能混成一个。
//!
//! `sqlx` 的查询一律用运行期绑定（`query` / `query_as`）而不是编译期宏：
//! 宏要求构建时能连上数据库，而发布构建不该依赖一个活的 MySQL。

mod account;
mod link;

pub use account::MySqlAccountRepository;
pub use link::MySqlLinkRepository;

/// 唯一键冲突判定。依赖具体错误码而不是字符串匹配，换 MySQL 版本也不会失效。
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(|db_err| db_err.is_unique_violation())
}
