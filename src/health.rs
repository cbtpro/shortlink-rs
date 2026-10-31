//! 就绪探测：确认依赖还活着，而不是「进程还在」。
//!
//! 只在 `/readyz` 被调用时执行，不做周期轮询 —— 探活频率应由编排系统决定。

use std::time::Duration;

use redis::aio::MultiplexedConnection;
use sqlx::MySqlPool;

/// 单次探测超时。探活不应该比调用方的超时还慢。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Readiness {
    pool: MySqlPool,
    redis: MultiplexedConnection,
}

impl Readiness {
    pub fn new(pool: MySqlPool, redis: MultiplexedConnection) -> Self {
        Self { pool, redis }
    }

    /// 返回不可用的依赖名，空表示全部就绪。
    pub async fn failed_checks(&self) -> Vec<&'static str> {
        let (mysql, redis) = tokio::join!(self.ping_mysql(), self.ping_redis());

        let mut failed = Vec::with_capacity(2);
        if !mysql {
            failed.push("mysql");
        }
        if !redis {
            failed.push("redis");
        }
        failed
    }

    async fn ping_mysql(&self) -> bool {
        match tokio::time::timeout(PROBE_TIMEOUT, sqlx::query("SELECT 1").execute(&self.pool)).await
        {
            Ok(Ok(_)) => true,
            Ok(Err(err)) => {
                log::warn!("readiness: mysql probe failed: {err}");
                false
            }
            Err(_) => {
                log::warn!("readiness: mysql probe timed out after {PROBE_TIMEOUT:?}");
                false
            }
        }
    }

    async fn ping_redis(&self) -> bool {
        let mut conn = self.redis.clone();
        let command = redis::cmd("PING");
        let ping = command.query_async::<String>(&mut conn);

        match tokio::time::timeout(PROBE_TIMEOUT, ping).await {
            Ok(Ok(_)) => true,
            Ok(Err(err)) => {
                log::warn!("readiness: redis probe failed: {err}");
                false
            }
            Err(_) => {
                log::warn!("readiness: redis probe timed out after {PROBE_TIMEOUT:?}");
                false
            }
        }
    }
}
