//! 缓存端口。
//!
//! - [`LinkCache`]：短链内容缓存，读多写少，失败可降级；
//! - [`VisitCounter`]：访问计数的写缓冲，由后台任务批量回刷；
//! - [`GateTicket`]：中转页「继续访问」的一次性凭据。

use std::collections::HashMap;
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

    /// 批量读取增量，用于列表页把「库里的值 + 未回刷的增量」还原成真实访问量。
    ///
    /// 默认实现逐个查，语义与实现无关；Redis 实现覆盖为一次 MGET。
    /// 返回的 map 只包含有增量的短码，调用方按 0 处理缺项。
    fn pending_of_many<'a>(
        &'a self,
        codes: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, u32>, CacheError>> {
        Box::pin(async move {
            let mut found = HashMap::new();
            for code in codes {
                let count = self.pending_of(code).await?;
                if count > 0 {
                    found.insert(code.clone(), count);
                }
            }
            Ok(found)
        })
    }

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

/// 中转页「继续访问」的一次性凭据。
///
/// 中转页只是确认页，不该消耗访问次数；真正算一次访问的是点「继续访问」之后
/// 那次跳转。要把这两次请求分开，就得证明后者来自刚刚通过鉴权的那位访问者，
/// 票据就是这个证明：一次性、短期、只对签发它的短码有效。
///
/// 不把密码塞进「继续访问」的链接，是因为链接会进浏览器历史、访问日志和截图。
pub trait GateTicket: Send + Sync {
    /// 签发一张票据，返回值可直接放进 URL。
    fn issue<'a>(
        &'a self,
        code: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<String, CacheError>>;

    /// 兑换票据，返回签发时绑定的短码。
    ///
    /// 一次性：取走即失效，重放同一张票据只会得到 `None`。
    fn redeem<'a>(&'a self, ticket: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::RecordingCounter;

    #[tokio::test]
    async fn pending_of_many_reports_only_codes_with_a_backlog() {
        // 列表页要按短码查增量，缺项一律当 0；默认实现逐个查，语义与 Redis 的 MGET 一致
        let counter = RecordingCounter::new();
        counter.seed("hot", 3);

        let found = counter
            .pending_of_many(&["hot".to_string(), "cold".to_string()])
            .await
            .expect("lookup should succeed");

        assert_eq!(found.get("hot"), Some(&3));
        assert!(
            !found.contains_key("cold"),
            "没有增量的短码不该出现在结果里"
        );
    }

    #[tokio::test]
    async fn pending_of_many_tolerates_an_empty_page() {
        let counter = RecordingCounter::new();
        let found = counter
            .pending_of_many(&[])
            .await
            .expect("empty batch should succeed");
        assert!(found.is_empty());
    }
}
