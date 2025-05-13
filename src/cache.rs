use redis::AsyncCommands;

/// 从 Redis 获取短链接对应的原始 URL
pub async fn get_cached_url(
    conn: &mut impl AsyncCommands,
    code: &str,
) -> redis::RedisResult<Option<String>> {
    conn.get(format!("short:code:{}", code)).await
}

/// 将短链接及其原始 URL 缓存到 Redis，并设置过期时间
pub async fn set_cached_url(
    conn: &mut impl AsyncCommands,
    code: &str,
    url: &str,
    ttl_seconds: u64,
) -> redis::RedisResult<()> {
    conn.set_ex(format!("short:code:{}", code), url, ttl_seconds).await
}
