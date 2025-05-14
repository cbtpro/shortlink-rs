use crate::errors::save_link_error::SaveLinkError;
use crate::models::short_link::ShortLink;
use crate::routes::shorten::ShortenRequest;
use sqlx::types::Json;
use sqlx::MySqlPool;

pub async fn save_link(
    pool: &MySqlPool,
    new_link: &ShortenRequest,
    code: &str,
) -> Result<ShortLink, SaveLinkError> {
    let existing = sqlx::query_scalar!(
        "SELECT COUNT(*) as count FROM short_links WHERE code = ?",
        code
    )
    .fetch_one(pool)
    .await?;

    if existing > 0 {
        return Err(SaveLinkError::CodeExists);
    }
    // 插入数据
    let result = sqlx::query(
        r#"
        INSERT INTO short_links (code, long_url, expire_at, max_visits, password, ip_limit, ua_limit)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(code)
    .bind(&new_link.url)
    .bind(new_link.expire_at)
    .bind(new_link.max_visits)
    .bind(&new_link.password)
    .bind(new_link.ip_limit.as_ref().map(Json))
    .bind(new_link.ua_limit.as_ref().map(Json))
    .execute(pool)
    .await?;
    // 获取刚刚插入的 ID
    let inserted_id: u64 = result.last_insert_id();

    // 再根据 ID 查询完整记录
    let short_link = sqlx::query_as!(
        ShortLink,
        r#"
        SELECT id, code, long_url, created_at, expire_at, max_visits, visit_count, password, ip_limit, ua_limit
        FROM short_links
        WHERE id = ?
        "#,
        inserted_id
    )
    .fetch_one(pool)
    .await?;

    Ok(short_link)
}

pub async fn get_link_by_code(
    pool: &MySqlPool,
    code: &str,
) -> Result<Option<ShortLink>, sqlx::Error> {
    let short_link =
            sqlx::query_as::<_, ShortLink>(
            r#"
            SELECT id, code, long_url, created_at, expire_at, max_visits, visit_count, password, ip_limit, ua_limit
            FROM short_links
            WHERE code = ?
            LIMIT 1
            "#,
        )
            .bind(code)
            .fetch_optional(pool)
            .await?;

    Ok(short_link)
}
