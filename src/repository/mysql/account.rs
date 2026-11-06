//! 账号与会话的 MySQL 实现。

use chrono::NaiveDateTime;
use sqlx::MySqlPool;

use super::is_unique_violation;
use crate::domain::account::{Access, AccountStatus, NewUser, User};
use crate::repository::{
    RepoError, SessionRepository, UserPage, UserQuery, UserRepository, UserSummary,
};
use crate::types::BoxFuture;

/// 字段顺序必须与 `domain::account::User` 的字段一一对应。
///
/// 这里的清单是白名单：用 `SELECT *` 会在加列时悄悄改变解码目标，
/// 也会把将来可能出现的敏感列带进领域对象。
const USER_COLUMNS: &str = "id, email, display_name, password_hash, role, status, \
     must_change_password, created_at";

const INSERT_USER_SQL: &str = "INSERT INTO users \
     (email, display_name, password_hash, role, status, must_change_password) \
     VALUES (?, ?, ?, ?, ?, ?)";

const FIND_USER_BY_EMAIL_SQL: &str =
    "SELECT id, email, display_name, password_hash, role, status, \
     must_change_password, created_at FROM users WHERE email = ? LIMIT 1";

const FIND_USER_BY_ID_SQL: &str = "SELECT id, email, display_name, password_hash, role, status, \
     must_change_password, created_at FROM users WHERE id = ? LIMIT 1";

const FIND_FIRST_ADMIN_SQL: &str = "SELECT id, email, display_name, password_hash, role, status, \
     must_change_password, created_at FROM users WHERE role = 'admin' ORDER BY id LIMIT 1";

/// 列表带上名下短链数量：一次 LEFT 子查询，省掉前端逐行再问一次。
const LIST_USERS_SQL: &str = "SELECT u.id, u.email, u.display_name, u.password_hash, u.role, \
     u.status, u.must_change_password, u.created_at, \
     CAST((SELECT COUNT(*) FROM short_links l WHERE l.user_id = u.id) AS UNSIGNED) AS link_count \
     FROM users u";

const COUNT_USERS_SQL: &str = "SELECT CAST(COUNT(*) AS UNSIGNED) FROM users u";

const SET_PASSWORD_SQL: &str = "UPDATE users \
     SET password_hash = ?, must_change_password = ?, password_changed_at = ? WHERE id = ?";

const SET_STATUS_SQL: &str = "UPDATE users SET status = ? WHERE id = ?";

const INSERT_SESSION_SQL: &str =
    "INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)";

/// 会话与用户在一条 SQL 里取回：鉴权发生在每个请求上，不该多一次往返。
///
/// `u.status = 'active'` 写在这里而不是查回来再判：禁用账号的那一刻，
/// 它所有还活着的会话都该同时失效，而不是等下一个业务分支想起来检查。
const SESSION_USER_SQL: &str =
    "SELECT u.id, u.email, u.display_name, u.password_hash, u.role, u.status, \
     u.must_change_password, u.created_at \
     FROM user_sessions s JOIN users u ON u.id = s.user_id \
     WHERE s.token_hash = ? AND s.expires_at > ? AND u.status = 'active' LIMIT 1";

const DELETE_SESSION_SQL: &str = "DELETE FROM user_sessions WHERE token_hash = ?";

const DELETE_USER_SESSIONS_SQL: &str = "DELETE FROM user_sessions WHERE user_id = ?";

const PURGE_SESSIONS_SQL: &str = "DELETE FROM user_sessions WHERE expires_at <= ?";

/// 列表行：用户本体 + 名下短链数。
///
/// `flatten` 复用 `User` 的手写 `FromRow`，避免把角色/状态的解析规则抄两遍。
#[derive(sqlx::FromRow)]
struct UserRow {
    #[sqlx(flatten)]
    user: User,
    link_count: u64,
}

pub struct MySqlAccountRepository {
    pool: MySqlPool,
}

impl MySqlAccountRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

impl UserRepository for MySqlAccountRepository {
    fn create<'a>(
        &'a self,
        new: &'a NewUser,
        password_hash: &'a str,
        access: Access,
    ) -> BoxFuture<'a, Result<User, RepoError>> {
        Box::pin(async move {
            let result = sqlx::query(INSERT_USER_SQL)
                .bind(&new.email)
                .bind(new.resolve_display_name())
                .bind(password_hash)
                .bind(access.role.as_str())
                .bind(AccountStatus::Active.as_str())
                .bind(access.must_change_password)
                .execute(&self.pool)
                .await;

            match result {
                // 回读一次，拿到数据库生成的 id 与 created_at
                Ok(_) => self
                    .find_by_email(&new.email)
                    .await?
                    .ok_or(RepoError::NotFound),
                Err(err) if is_unique_violation(&err) => Err(RepoError::DuplicateEmail),
                Err(err) => Err(err.into()),
            }
        })
    }

    fn find_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            sqlx::query_as::<_, User>(FIND_USER_BY_EMAIL_SQL)
                .bind(email)
                .fetch_optional(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn find_by_id(&self, id: u64) -> BoxFuture<'_, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            sqlx::query_as::<_, User>(FIND_USER_BY_ID_SQL)
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn find_by_ids<'a>(&'a self, ids: &'a [u64]) -> BoxFuture<'a, Result<Vec<User>, RepoError>> {
        Box::pin(async move {
            // 调用方已去重，仍在这里挡空切片：`IN ()` 不是合法 SQL
            if ids.is_empty() {
                return Ok(Vec::new());
            }

            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE id IN ({placeholders})");

            let mut query = sqlx::query_as::<_, User>(&sql);
            for id in ids {
                query = query.bind(id);
            }
            query.fetch_all(&self.pool).await.map_err(Into::into)
        })
    }

    fn find_first_admin(&self) -> BoxFuture<'_, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            sqlx::query_as::<_, User>(FIND_FIRST_ADMIN_SQL)
                .fetch_optional(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn list_users<'a>(
        &'a self,
        query: &'a UserQuery,
    ) -> BoxFuture<'a, Result<UserPage, RepoError>> {
        Box::pin(async move {
            let pattern = query.keyword.as_deref().map(super::link::like_pattern);
            let conditions = if pattern.is_some() {
                " WHERE u.email LIKE ? OR u.display_name LIKE ?"
            } else {
                ""
            };

            let select_sql =
                format!("{LIST_USERS_SQL}{conditions} ORDER BY u.id DESC LIMIT ? OFFSET ?");
            let count_sql = format!("{COUNT_USERS_SQL}{conditions}");

            let mut select = sqlx::query_as::<_, UserRow>(&select_sql);
            let mut count = sqlx::query_scalar::<_, u64>(&count_sql);
            if let Some(pattern) = &pattern {
                select = select.bind(pattern).bind(pattern);
                count = count.bind(pattern).bind(pattern);
            }

            let offset = u64::from(query.page.saturating_sub(1)) * u64::from(query.page_size);
            let rows = select
                .bind(i64::from(query.page_size))
                .bind(offset as i64)
                .fetch_all(&self.pool)
                .await?;
            let total = count.fetch_one(&self.pool).await?;

            Ok(UserPage {
                items: rows
                    .into_iter()
                    .map(|row| UserSummary {
                        user: row.user,
                        link_count: row.link_count,
                    })
                    .collect(),
                total,
            })
        })
    }

    fn set_password<'a>(
        &'a self,
        id: u64,
        password_hash: &'a str,
        must_change_password: bool,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(SET_PASSWORD_SQL)
                .bind(password_hash)
                .bind(must_change_password)
                .bind(chrono::Utc::now().naive_utc())
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }

    fn set_status(&self, id: u64, status: AccountStatus) -> BoxFuture<'_, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(SET_STATUS_SQL)
                .bind(status.as_str())
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }
}

impl SessionRepository for MySqlAccountRepository {
    fn create<'a>(
        &'a self,
        user_id: u64,
        token_hash: &'a str,
        expires_at: NaiveDateTime,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            sqlx::query(INSERT_SESSION_SQL)
                .bind(token_hash)
                .bind(user_id)
                .bind(expires_at)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    fn find_user<'a>(
        &'a self,
        token_hash: &'a str,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            sqlx::query_as::<_, User>(SESSION_USER_SQL)
                .bind(token_hash)
                .bind(now)
                .fetch_optional(&self.pool)
                .await
                .map_err(Into::into)
        })
    }

    fn delete<'a>(&'a self, token_hash: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(DELETE_SESSION_SQL)
                .bind(token_hash)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected > 0)
        })
    }

    fn delete_for_user(&self, user_id: u64) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(DELETE_USER_SESSIONS_SQL)
                .bind(user_id)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected)
        })
    }

    fn purge_expired(&self, now: NaiveDateTime) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async move {
            let affected = sqlx::query(PURGE_SESSIONS_SQL)
                .bind(now)
                .execute(&self.pool)
                .await?
                .rows_affected();
            Ok(affected)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_column_is_selected_explicitly() {
        // 用 `SELECT *` 会在加列时悄悄改变解码目标；这里的清单必须与
        // `domain::account::User` 字段一一对应。
        let expected = [
            "id",
            "email",
            "display_name",
            "password_hash",
            "role",
            "status",
            "must_change_password",
            "created_at",
        ];
        for column in expected {
            assert!(
                FIND_USER_BY_EMAIL_SQL.contains(column),
                "邮箱查询缺少 {column}"
            );
            assert!(SESSION_USER_SQL.contains(column), "会话查询缺少 {column}");
            assert!(LIST_USERS_SQL.contains(column), "用户列表缺少 {column}");
        }
    }

    #[test]
    fn session_lookup_requires_an_unexpired_active_account() {
        // 过期判定写在 SQL 里，避免「查回来再在内存里比时间」时漏掉某个分支；
        // 禁用判定同理，账号被停用后旧会话立即失效。
        assert!(SESSION_USER_SQL.contains("expires_at > ?"));
        assert!(SESSION_USER_SQL.contains("u.status = 'active'"));
    }

    #[test]
    fn list_counts_links_per_user() {
        assert!(LIST_USERS_SQL.contains("link_count"));
        assert!(LIST_USERS_SQL.contains("short_links"));
    }

    #[test]
    fn insert_writes_every_column_that_decides_access() {
        // 少写 role / status / must_change_password 任何一个，新账号都会落到
        // 数据库默认值上 —— 那等于把权限判断交给表结构，改一次迁移就可能变味。
        for column in [
            "email",
            "display_name",
            "password_hash",
            "role",
            "status",
            "must_change_password",
        ] {
            assert!(INSERT_USER_SQL.contains(column), "建号语句缺少 {column}");
        }
        assert_eq!(INSERT_USER_SQL.matches('?').count(), 6);
    }

    #[test]
    fn password_updates_are_stamped_and_clear_the_flag() {
        assert!(SET_PASSWORD_SQL.contains("must_change_password = ?"));
        // 改密时间只作审计，不参与判定；但既然建了这一列，就得真的写进去
        assert!(SET_PASSWORD_SQL.contains("password_changed_at = ?"));
        // 散列、是否待改密、改密时间、目标 id
        assert_eq!(SET_PASSWORD_SQL.matches('?').count(), 4);
    }

    #[test]
    fn status_updates_touch_nothing_else() {
        // 只改一列：顺手多写几列会让「停用」变成一次隐性改密
        assert_eq!(SET_STATUS_SQL.matches('?').count(), 2);
        assert!(!SET_STATUS_SQL.to_ascii_lowercase().contains("password"));
    }
}
