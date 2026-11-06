//! 短链与访客创建记录的 MySQL 实现。

use chrono::{NaiveDateTime, Utc};
use sqlx::types::Json;
use sqlx::MySqlPool;

use super::is_unique_violation;
use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::ShortLink;
use crate::repository::{
    CreationHistory, LinkFilter, LinkOverview, LinkPage, LinkQuery, LinkRepository, OverviewWindow,
    RepoError,
};
use crate::types::BoxFuture;

/// 字段顺序必须与 `ShortLink` 的字段一一对应，新增列时同步修改。
const COLUMNS: &str = "id, code, user_id, long_url, created_at, expire_at, \
     max_visits, visit_count, password, ip_limit, ua_limit, interstitial";

/// 与 [`COLUMNS`] 相同的字段，但都带上表别名，供 join 使用。
const LINK_COLUMNS: &str = "l.id, l.code, l.user_id, l.long_url, l.created_at, l.expire_at, \
     l.max_visits, l.visit_count, l.password, l.ip_limit, l.ua_limit, l.interstitial";

const INSERT_SQL: &str = "INSERT INTO short_links \
     (code, user_id, long_url, expire_at, max_visits, password, ip_limit, ua_limit, interstitial) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";

const ADD_VISITS_SQL: &str = "UPDATE short_links SET visit_count = visit_count + ? WHERE code = ?";

const DELETE_SQL: &str = "DELETE FROM short_links WHERE code = ?";

const DELETE_OWNED_SQL: &str = "DELETE FROM short_links WHERE code = ? AND user_id = ?";

const CODES_OF_OWNER_SQL: &str = "SELECT code FROM short_links WHERE user_id = ?";

const DELETE_ALL_OWNED_SQL: &str = "DELETE FROM short_links WHERE user_id = ?";

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
///
/// 归属过滤放在 `WHERE` 之前拼（见 [`overview_sql`]），保证「我的后台」看到的是
/// 自己那几个数字，而不是全站汇总。
const OVERVIEW_BODY: &str = "SELECT \
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
                .bind(link.owner_id)
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
            let result = sqlx::query(ADD_VISITS_SQL)
                .bind(delta)
                .bind(code)
                .execute(&self.pool)
                .await?;

            // UPDATE 不报「没这行」。不判影响行数的话，回刷任务会以为写成功了，
            // 于是一边抵扣增量一边丢掉这段访问量，而且永远不会触发「链接已删除」的清理。
            if result.rows_affected() == 0 {
                return Err(RepoError::NotFound);
            }

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
            let count_sql =
                format!("SELECT CAST(COUNT(*) AS UNSIGNED) FROM short_links{conditions}");

            // 两条 SQL 的占位符顺序必须与各自的绑定顺序严格一致
            let mut select = sqlx::query_as::<_, ShortLink>(&select_sql);
            let mut count = sqlx::query_scalar::<_, u64>(&count_sql);
            if let Some(pattern) = &pattern {
                select = select.bind(pattern).bind(pattern);
                count = count.bind(pattern).bind(pattern);
            }
            if let Some(owner) = query.owner {
                select = select.bind(owner);
                count = count.bind(owner);
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
        owner: Option<u64>,
    ) -> BoxFuture<'_, Result<LinkOverview, RepoError>> {
        Box::pin(async move {
            let sql = match owner {
                Some(_) => format!("{OVERVIEW_BODY} WHERE user_id = ?"),
                None => OVERVIEW_BODY.to_string(),
            };

            let mut query = sqlx::query_as::<_, (u64, u64, u64, u64)>(&sql)
                .bind(window.today_start)
                .bind(window.now)
                .bind(window.expiring_before);
            if let Some(owner) = owner {
                query = query.bind(owner);
            }

            let (total_links, total_visits, created_today, expiring_soon) =
                query.fetch_one(&self.pool).await?;

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

    fn list_by_owner(
        &self,
        owner_id: u64,
        limit: u32,
    ) -> BoxFuture<'_, Result<Vec<ShortLink>, RepoError>> {
        Box::pin(async move {
            let sql = format!(
                "SELECT {COLUMNS} FROM short_links WHERE user_id = ? \
                 ORDER BY created_at DESC, id DESC LIMIT ?"
            );
            sqlx::query_as::<_, ShortLink>(&sql)
                .bind(owner_id)
                .bind(i64::from(limit))
                .fetch_all(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn delete_owned<'a>(
        &'a self,
        owner_id: u64,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(DELETE_OWNED_SQL)
                .bind(code)
                .bind(owner_id)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }

    fn delete_all_owned(&self, owner_id: u64) -> BoxFuture<'_, Result<Vec<String>, RepoError>> {
        Box::pin(async move {
            // 先查后删：缓存要按短码逐条失效，而 MySQL 的 DELETE 不回报删了哪些行。
            // 放在同一事务里，避免「查到了，删之前已经被别的请求删掉」造成错配。
            let mut tx = self.pool.begin().await?;
            let codes: Vec<String> = sqlx::query_scalar(CODES_OF_OWNER_SQL)
                .bind(owner_id)
                .fetch_all(&mut *tx)
                .await?;

            if !codes.is_empty() {
                sqlx::query(DELETE_ALL_OWNED_SQL)
                    .bind(owner_id)
                    .execute(&mut *tx)
                    .await?;
            }

            tx.commit().await?;
            Ok(codes)
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

    // 归属过滤排在关键字之后、状态之前：绑定顺序必须与 list() 一致
    if query.owner.is_some() {
        conditions.push_str(" AND user_id = ?");
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
/// 对 `account` 模块可见：用户列表的关键字搜索用的是同一套规则。
pub(super) fn like_pattern(keyword: &str) -> String {
    let escaped = keyword
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
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
            owner: None,
        }
    }

    #[test]
    fn conditions_follow_placeholder_order() {
        // 顺序：keyword 两个 → owner 一个 → filter 一个；绑定代码依赖这个顺序
        let sql = build_conditions(&LinkQuery {
            owner: Some(7),
            ..query(Some("abc"), LinkFilter::Active)
        });
        let code_pos = sql.find("code LIKE").expect("keyword clause");
        let owner_pos = sql.find("user_id = ?").expect("owner clause");
        let filter_pos = sql.find("expire_at IS NULL").expect("filter clause");
        assert!(code_pos < owner_pos, "归属条件要排在关键字之后");
        assert!(owner_pos < filter_pos, "归属条件要排在状态之前");
        assert_eq!(sql.matches('?').count(), 4);
    }

    #[test]
    fn exhausted_filter_binds_nothing() {
        // `ALL` 与 `EXHAUSTED` 都不需要时间参数，绑定逻辑必须与之一致
        assert_eq!(
            build_conditions(&query(None, LinkFilter::All))
                .matches('?')
                .count(),
            0
        );
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
