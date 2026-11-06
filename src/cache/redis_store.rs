//! 基于 Redis 的缓存与计数器实现。
//!
//! 所有 key 的拼接规则集中在本文件，业务代码不再出现 `"short:code:..."` 之类的字面量。

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Context};
use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, Client};

use super::{CacheError, GateTicket, LinkCache, PendingVisit, VisitCounter};
use crate::domain::short_link::CachedLink;
use crate::security::token;
use crate::settings::redact_credentials;
use crate::types::BoxFuture;

/// 建连超时。对端地址不可达时 TCP 会一直重传到系统默认超时（几十秒），
/// 启动阶段卡住却给不出原因，是最难排查的一类问题。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

// key 带版本号：缓存结构发生不兼容变更时递增版本，旧数据自然过期，无需迁移
// v4：缓存投影加了 `interstitial`，v3 的值缺字段
const LINK_KEY_PREFIX: &str = "short:v4:code:";
// 计数 key 存的是纯整数，结构从未变过；跟着升版本会丢掉尚未回刷的增量，因此停在 v3
const VISIT_KEY_PREFIX: &str = "short:v3:visits:";
/// 票据 key。自带 TTL，过期即消失，不需要清理任务
const GATE_KEY_PREFIX: &str = "short:gate:";
/// 单次 SCAN 的批量大小
const SCAN_BATCH_SIZE: usize = 200;

fn link_key(code: &str) -> String {
    format!("{LINK_KEY_PREFIX}{code}")
}

fn visit_key(code: &str) -> String {
    format!("{VISIT_KEY_PREFIX}{code}")
}

fn gate_key(ticket: &str) -> String {
    format!("{GATE_KEY_PREFIX}{ticket}")
}

/// 建立 Redis 连接。
///
/// 失败信息里带上脱敏后的地址：默认的 `broken pipe` 看不出连的是本地还是远端，
/// 而这类问题几乎总是 `.env` 里的地址写错了。
pub async fn connect(url: &str) -> anyhow::Result<MultiplexedConnection> {
    let endpoint = redact_credentials(url);
    let client = Client::open(url).with_context(|| format!("invalid Redis URL: {endpoint}"))?;

    tokio::time::timeout(CONNECT_TIMEOUT, client.get_multiplexed_async_connection())
        .await
        .map_err(|_| {
            anyhow!("connecting to Redis timed out after {CONNECT_TIMEOUT:?}: {endpoint}")
        })?
        .with_context(|| format!("failed to connect to Redis at {endpoint}"))
}

pub struct RedisLinkCache {
    /// MultiplexedConnection 的 clone 是廉价的句柄复制，按需 clone 即可，无需连接池
    conn: MultiplexedConnection,
}

impl RedisLinkCache {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

impl LinkCache for RedisLinkCache {
    fn get<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<Option<CachedLink>, CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let raw: Option<String> = conn.get(link_key(code)).await?;

            match raw {
                Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
                None => Ok(None),
            }
        })
    }

    fn put<'a>(
        &'a self,
        code: &'a str,
        link: &'a CachedLink,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async move {
            let payload = serde_json::to_string(link)?;
            let mut conn = self.conn.clone();
            let _: () = conn.set_ex(link_key(code), payload, ttl.as_secs()).await?;
            Ok(())
        })
    }

    fn evict<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            // 只删内容缓存，计数 key 留给回刷任务处理：
            // 提前删掉会丢失尚未落库的访问量
            let _: () = conn.del(link_key(code)).await?;
            Ok(())
        })
    }
}

pub struct RedisVisitCounter {
    conn: MultiplexedConnection,
}

impl RedisVisitCounter {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

impl VisitCounter for RedisVisitCounter {
    fn pending_of<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let raw: Option<i64> = conn.get(visit_key(code)).await?;
            Ok(raw.unwrap_or(0).max(0) as u32)
        })
    }

    fn pending_of_many<'a>(
        &'a self,
        codes: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, u32>, CacheError>> {
        Box::pin(async move {
            // MGET 空 key 列表会被 Redis 当成参数错误，先挡掉
            if codes.is_empty() {
                return Ok(HashMap::new());
            }

            let keys: Vec<String> = codes.iter().map(|code| visit_key(code)).collect();
            let mut conn = self.conn.clone();
            let values: Vec<Option<i64>> = conn.mget(keys).await?;

            // MGET 保证返回顺序与 key 顺序一致，可以按位置配对
            let mut found = HashMap::with_capacity(codes.len());
            for (code, value) in codes.iter().zip(values) {
                let count = value.unwrap_or(0).max(0) as u32;
                if count > 0 {
                    found.insert(code.clone(), count);
                }
            }
            Ok(found)
        })
    }

    fn increment<'a>(
        &'a self,
        code: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let key = visit_key(code);
            let count: i64 = conn.incr(&key, 1).await?;
            // 每次写入都续期，保证回刷任务有足够窗口处理
            let _: () = conn.expire(&key, ttl.as_secs() as i64).await?;
            Ok(count.max(0) as u32)
        })
    }

    fn scan_pending<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PendingVisit>, CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let mut cursor: u64 = 0;
            let mut pending = Vec::new();

            loop {
                let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(format!("{VISIT_KEY_PREFIX}*"))
                    .arg("COUNT")
                    .arg(SCAN_BATCH_SIZE)
                    .query_async(&mut conn)
                    .await?;

                for key in keys {
                    let Some(code) = key.strip_prefix(VISIT_KEY_PREFIX) else {
                        continue;
                    };
                    let raw: Option<i64> = conn.get(&key).await?;
                    if let Some(count) = raw {
                        if count > 0 {
                            pending.push(PendingVisit {
                                code: code.to_string(),
                                count: count as u32,
                            });
                        }
                    }
                }

                cursor = next_cursor;
                if cursor == 0 {
                    break;
                }
            }

            Ok(pending)
        })
    }

    fn consume<'a>(&'a self, visit: PendingVisit) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async move {
            let mut conn = self.conn.clone();
            let key = visit_key(&visit.code);

            // 用 DECRBY 抵扣而不是直接 DEL：回刷期间新产生的访问不会被误删
            let remaining: i64 = conn.decr(&key, visit.count as i64).await?;
            if remaining <= 0 {
                let _: () = conn.del(&key).await?;
            }

            Ok(())
        })
    }
}

/// 中转页「继续访问」的票据，存在 Redis 里而不是签名放进 URL：
/// 多实例部署时，签发和兑换很可能落在不同进程上，共享存储才不会「点了没反应」。
pub struct RedisGateTicket {
    conn: MultiplexedConnection,
}

impl RedisGateTicket {
    pub fn new(conn: MultiplexedConnection) -> Self {
        Self { conn }
    }
}

impl GateTicket for RedisGateTicket {
    fn issue<'a>(
        &'a self,
        code: &'a str,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<String, CacheError>> {
        Box::pin(async move {
            let token = token::mint();
            let mut conn = self.conn.clone();
            // SETEX：一次往返写值和有效期，不会留下没有 TTL 的 key
            let _: () = conn.set_ex(gate_key(&token), code, ttl.as_secs()).await?;
            Ok(token)
        })
    }

    fn redeem<'a>(&'a self, ticket: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async move {
            // GETDEL 是原子的：同一张票据被并发点到，只有一个请求拿得到
            let mut conn = self.conn.clone();
            let code: Option<String> = conn.get_del(gate_key(ticket)).await?;
            Ok(code)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced_and_versioned() {
        assert_eq!(link_key("abc"), "short:v4:code:abc");
        assert_eq!(visit_key("abc"), "short:v3:visits:abc");
        // 同一短码的两类 key 必须分开，否则计数会覆盖内容缓存
        assert_ne!(link_key("abc"), visit_key("abc"));
    }

    #[test]
    fn keys_of_different_codes_do_not_collide() {
        assert_ne!(link_key("abc"), link_key("abd"));
        assert_ne!(visit_key("a"), visit_key("a1"));
    }

    #[test]
    fn cached_link_survives_json_round_trip() {
        // 缓存里存的就是 serde_json 文本，字段类型必须能往返
        let link = CachedLink {
            id: 1,
            long_url: "https://example.com".into(),
            expire_at: None,
            max_visits: Some(2),
            visit_count: 7,
            interstitial: true,
        };

        let raw = serde_json::to_string(&link).expect("should serialize");
        let restored: CachedLink = serde_json::from_str(&raw).expect("should deserialize");

        assert_eq!(restored.max_visits, Some(2));
        assert_eq!(restored.visit_count, 7);
        assert!(restored.interstitial);
        assert!(restored.expire_at.is_none());
    }

    #[tokio::test]
    async fn connect_error_redacts_credentials() {
        let error = connect("redis://user:secret@127.0.0.1:1/0")
            .await
            .expect_err("port 1 should not accept redis traffic");

        let message = format!("{error:#}");
        assert!(
            !message.contains("secret"),
            "凭据不应出现在错误信息里: {message}"
        );
        assert!(
            message.contains("***@127.0.0.1:1"),
            "应保留脱敏后的地址用于定位问题: {message}"
        );
    }
}
