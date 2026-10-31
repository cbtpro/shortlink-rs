//! 缓存端口。
//!
//! - [`LinkCache`]：短链内容缓存，读多写少，失败可降级；
//! - [`VisitCounter`]：访问计数的写缓冲，由后台任务批量回刷。

use std::fmt;
use std::time::Duration;

use crate::domain::short_link::CachedLink;
use crate::types::BoxFuture;

// 模块名不用 `redis`，否则会遮蔽同名外部 crate，导致 `redis::RedisError` 解析到本地模块
pub mod redis_store;

#[derive(Debug)]
pub enum CacheError {
    Backend(redis::RedisError),
    Serialize(serde_json::Error),
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheError::Backend(err) => write!(f, "redis failure: {err}"),
            CacheError::Serialize(err) => write!(f, "cache payload is not valid json: {err}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CacheError::Backend(err) => Some(err),
            CacheError::Serialize(err) => Some(err),
        }
    }
}

impl From<redis::RedisError> for CacheError {
    fn from(err: redis::RedisError) -> Self {
        CacheError::Backend(err)
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(err: serde_json::Error) -> Self {
        CacheError::Serialize(err)
    }
}

/// 短链内容缓存。
pub trait LinkCache: Send + Sync {
    fn get<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<Option<CachedLink>, CacheError>>;

    fn put<'a>(
        &'a self,
        code: &'a str,
        link: &'a CachedLink,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<(), CacheError>>;

    /// 删除缓存条目。
    ///
    /// 删链后必须调用，否则已删除的短链还能凭借缓存跳到 TTL 结束。
    fn evict<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), CacheError>>;
}

/// 一批待回刷的访问计数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingVisit {
    pub code: String,
    pub count: u32,
}

/// 访问计数缓冲。
pub trait VisitCounter: Send + Sync {
    /// 读取尚未回刷的增量（无记录时返回 0）。
    fn pending_of<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<u32, CacheError>>;

    /// 自增并返回累计增量。
    fn increment<'a>(
        &'a self,
        code: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<u32, CacheError>>;

    /// 扫描所有待回刷的计数。
    fn scan_pending<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PendingVisit>, CacheError>>;

    /// 回刷成功后抵扣增量，避免回刷期间新产生的计数被覆盖。
    fn consume<'a>(&'a self, visit: PendingVisit) -> BoxFuture<'a, Result<(), CacheError>>;
}
