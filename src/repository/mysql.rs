//! 基于 MySQL 的仓储实现。

use chrono::{NaiveDateTime, Utc};
use sqlx::types::Json;
use sqlx::MySqlPool;

use super::{
    CreationHistory, LinkFilter, LinkOverview, LinkPage, LinkQuery, LinkRepository,
    OverviewWindow, RepoError,
};
use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::ShortLink;
use crate::types::BoxFuture;

/// 字段顺序必须与 `ShortLink` 的字段一一对应，新增列时同步修改。
const COLUMNS: &str = "id, code, long_url, created_at, expire_at, \
     max_visits, visit_count, password, ip_limit, ua_limit, interstitial";

/// 与 [`COLUMNS`] 相同的字段，但都带上表别名，供 join 使用。
const LINK_COLUMNS: &str = "l.id, l.code, l.long_url, l.created_at, l.expire_at, \
     l.max_visits, l.visit_count, l.password, l.ip_limit, l.ua_limit, l.interstitial";

const INSERT_SQL: &str = "INSERT INTO short_links \
     (code, long_url, expire_at, max_visits, password, ip_limit, ua_limit, interstitial) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)";

const ADD_VISITS_SQL: &str = "UPDATE short_links SET visit_count = visit_count + ? WHERE code = ?";

const DELETE_SQL: &str = "DELETE FROM short_links WHERE code = ?";

/// 同一访客重复创建同一短码时刷新时间，而不是报冲突。
///
/// `VALUES()` 在 MySQL 8.0.20 起标记为废弃，但仍是兼容性最好的写法，
/// 换成 `AS new` 语法会要求 8.0.19 以上。
const RECORD_SQL: &str = "INSERT INTO link_creations (visitor_id, code, created_at) \
     VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE created_at = VALUES(created_at)";

/// 只保留最近 `keep` 条。子查询必须再包一层派生表，
/// 否则 MySQL 会拒绝「在 DELETE 的子查询里读同一张表」。
const TRIM_SQL: &str = "DELETE FROM link_creations WHERE visitor_id = ? AND id NOT IN ( \
     SELECT id FROM (SELECT id FROM link_creations WHERE visitor_id = ? \
     ORDER BY created_at DESC, id DESC LIMIT ?) AS kept)";

const RECENT_CONDITION: &str = "FROM link_creations c JOIN short_links l ON l.code = c.code \
     WHERE c.visitor_id = ? ORDER BY c.created_at DESC, c.id DESC LIMIT ?";

const FORGET_SQL: &str = "DELETE FROM link_creations WHERE visitor_id = ? AND code = ?";

const CLEAR_SQL: &str = "DELETE FROM link_creations WHERE visitor_id = ?";

/// 概览用一条 SQL 算完，避免四次往返。
///
/// `SUM(布尔表达式)` 在 MySQL 里等价于计数；`CAST(... AS UNSIGNED)` 是为了让
/// `DECIMAL` 能直接解码成 `u64`（聚合结果默认不是无符号整数）。
const OVERVIEW_SQL: &str = "SELECT \
     CAST(COUNT(*) AS UNSIGNED) AS total_links, \
     CAST(COALESCE(SUM(visit_count), 0) AS UNSIGNED) AS total_visits, \
     CAST(COALESCE(SUM(created_at >= ?), 0) AS UNSIGNED) AS created_today, \
     CAST(COALESCE(SUM(expire_at IS NOT NULL AND expire_at > ? AND expire_at <= ?), 0) AS UNSIGNED) \
       AS expiring_soon \
     FROM short_links";

pub struct MySqlLinkRepository {
    pool: MySqlPool,
}

impl MySqlLinkRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

impl LinkRepository for MySqlLinkRepository {
    fn insert<'a>(
        &'a self,
        link: &'a NewShortLink,
        code: &'a str,
    ) -> BoxFuture<'a, Result<ShortLink, RepoError>> {
        Box::pin(async move {
            let result = sqlx::query(INSERT_SQL)
                .bind(code)
                .bind(&link.url)
                .bind(link.expire_at)
                .bind(link.max_visits)
                .bind(&link.password)
                .bind(link.ip_limit.as_ref().map(Json))
                .bind(link.ua_limit.as_ref().map(Json))
                .bind(link.interstitial)
                .execute(&self.pool)
                .await;

            match result {
                // 回读一次，拿到数据库生成的 created_at 与默认值
                Ok(_) => self.find_by_code(code).await?.ok_or(RepoError::NotFound),
                Err(err) if is_unique_violation(&err) => Err(RepoError::DuplicateCode),
                Err(err) => Err(err.into()),
            }
        })
    }

    fn find_by_code<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<Option<ShortLink>, RepoError>> {
        Box::pin(async move {
            let sql = format!("SELECT {COLUMNS} FROM short_links WHERE code = ? LIMIT 1");
            sqlx::query_as::<_, ShortLink>(&sql)
                .bind(code)
                .fetch_optional(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn add_visit_count<'a>(
        &'a self,
        code: &'a str,
        delta: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            sqlx::query(ADD_VISITS_SQL)
                .bind(delta)
                .bind(code)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    fn list<'a>(
        &'a self,
        query: &'a LinkQuery,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<LinkPage, RepoError>> {
        Box::pin(async move {
            let conditions = build_conditions(query);
            let pattern = query.keyword.as_deref().map(like_pattern);

            let select_sql = format!(
                "SELECT {COLUMNS} FROM short_links{conditions} \
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
            );
            let count_sql = format!("SELECT CAST(COUNT(*) AS UNSIGNED) FROM short_links{conditions}");

            // 两条 SQL 的占位符顺序必须与各自的绑定顺序严格一致
            let mut select = sqlx::query_as::<_, ShortLink>(&select_sql);
            let mut count = sqlx::query_scalar::<_, u64>(&count_sql);
            if let Some(pattern) = &pattern {
                select = select.bind(pattern).bind(pattern);
                count = count.bind(pattern).bind(pattern);
            }
            if matches!(query.filter, LinkFilter::Active | LinkFilter::Expired) {
                select = select.bind(now);
                count = count.bind(now);
            }

            let offset = u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size);
            let items = select
                .bind(i64::from(query.page_size))
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await?;
            let total = count.fetch_one(&self.pool).await?;

            Ok(LinkPage { items, total })
        })
    }

    fn overview(
        &self,
        window: OverviewWindow,
    ) -> BoxFuture<'_, Result<LinkOverview, RepoError>> {
        Box::pin(async move {
            let (total_links, total_visits, created_today, expiring_soon) =
                sqlx::query_as::<_, (u64, u64, u64, u64)>(OVERVIEW_SQL)
                    .bind(window.today_start)
                    .bind(window.now)
                    .bind(window.expiring_before)
                    .fetch_one(&self.pool)
                    .await?;

            Ok(LinkOverview {
                total_links,
                total_visits,
                created_today,
                expiring_soon,
            })
        })
    }

    fn delete_by_code<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(DELETE_SQL)
                .bind(code)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }
}

impl CreationHistory for MySqlLinkRepository {
    fn record<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
        keep: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            sqlx::query(RECORD_SQL)
                .bind(visitor)
                .bind(code)
                .bind(Utc::now().naive_utc())
                .execute(&self.pool)
                .await?;

            sqlx::query(TRIM_SQL)
                .bind(visitor)
                .bind(visitor)
                .bind(i64::from(keep.max(1)))
                .execute(&self.pool)
                .await?;

            Ok(())
        })
    }

    fn recent<'a>(
        &'a self,
        visitor: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<ShortLink>, RepoError>> {
        Box::pin(async move {
            let sql = format!("SELECT {LINK_COLUMNS} {RECENT_CONDITION}");
            sqlx::query_as::<_, ShortLink>(&sql)
                .bind(visitor)
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn forget<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(FORGET_SQL)
                .bind(visitor)
                .bind(code)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }

    fn clear<'a>(&'a self, visitor: &'a str) -> BoxFuture<'a, Result<u64, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(CLEAR_SQL)
                .bind(visitor)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected)
        })
    }
}

/// 拼 WHERE 片段。`keyword` 与 `filter` 的占位符顺序必须与列表查询里的绑定顺序一致。
fn build_conditions(query: &LinkQuery) -> String {
    let mut conditions = String::from(" WHERE 1 = 1");

    if query.keyword.is_some() {
        conditions.push_str(" AND (code LIKE ? OR long_url LIKE ?)");
    }

    // 判定条件与 `ShortLink::state` 一一对应，改一处必须同步另一处
    conditions.push_str(match query.filter {
        LinkFilter::All => "",
        LinkFilter::Active => {
            " AND (expire_at IS NULL OR expire_at > ?) \
             AND (max_visits IS NULL OR visit_count < max_visits)"
        }
        LinkFilter::Expired => " AND expire_at IS NOT NULL AND expire_at <= ?",
        LinkFilter::Exhausted => " AND max_visits IS NOT NULL AND visit_count >= max_visits",
    });

    conditions
}

/// 转义 `LIKE` 的通配符，避免用户输入 `%` 时全表匹配。
///
/// MySQL 默认把反斜杠当转义符，所以这里只用反斜杠转义。
fn like_pattern(keyword: &str) -> String {
    let escaped = keyword
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// 唯一键冲突判定。依赖具体错误码而不是字符串匹配，换 MySQL 版本也不会失效。
fn is_unique_violation(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .is_some_and(|db_err| db_err.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(keyword: Option<&str>, filter: LinkFilter) -> LinkQuery {
        LinkQuery {
            page: 1,
            page_size: 20,
            keyword: keyword.map(str::to_string),
            filter,
        }
    }

    #[test]
    fn conditions_follow_placeholder_order() {
        // 顺序：keyword 两个 → filter 一个；绑定代码依赖这个顺序
        let sql = build_conditions(&query(Some("abc"), LinkFilter::Active));
        let code_pos = sql.find("code LIKE").expect("keyword clause");
        let filter_pos = sql.find("expire_at IS NULL").expect("filter clause");
        assert!(code_pos < filter_pos);
        assert_eq!(sql.matches('?').count(), 3);
    }

    #[test]
    fn exhausted_filter_binds_nothing() {
        // `ALL` 与 `EXHAUSTED` 都不需要时间参数，绑定逻辑必须与之一致
        assert_eq!(build_conditions(&query(None, LinkFilter::All)).matches('?').count(), 0);
        assert_eq!(
            build_conditions(&query(None, LinkFilter::Exhausted))
                .matches('?')
                .count(),
            0
        );
        assert_eq!(
            build_conditions(&query(None, LinkFilter::Expired))
                .matches('?')
                .count(),
            1
        );
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("100%"), "%100\\%%");
        assert_eq!(like_pattern("a_b"), "%a\\_b%");
        assert_eq!(like_pattern("c:\\tmp"), "%c:\\\\tmp%");
    }
}
