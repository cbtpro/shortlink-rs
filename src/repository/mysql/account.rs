//! 账号与会话的 MySQL 实现。

use chrono::NaiveDateTime;
use sqlx::MySqlPool;

use super::is_unique_violation;
use crate::domain::account::{NewUser, User};
use crate::repository::{RepoError, SessionRepository, UserRepository};
use crate::types::BoxFuture;

const INSERT_USER_SQL: &str =
    "INSERT INTO users (email, display_name, password_hash) VALUES (?, ?, ?)";

const FIND_USER_BY_EMAIL_SQL: &str = "SELECT id, email, display_name, password_hash, created_at \
     FROM users WHERE email = ? LIMIT 1";

const INSERT_SESSION_SQL: &str =
    "INSERT INTO user_sessions (token_hash, user_id, expires_at) VALUES (?, ?, ?)";

/// 会话与用户在一条 SQL 里取回：鉴权发生在每个请求上，不该多一次往返。
const SESSION_USER_SQL: &str =
    "SELECT u.id, u.email, u.display_name, u.password_hash, u.created_at \
     FROM user_sessions s JOIN users u ON u.id = s.user_id \
     WHERE s.token_hash = ? AND s.expires_at > ? LIMIT 1";

const DELETE_SESSION_SQL: &str = "DELETE FROM user_sessions WHERE token_hash = ?";

const PURGE_SESSIONS_SQL: &str = "DELETE FROM user_sessions WHERE expires_at <= ?";

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
    ) -> BoxFuture<'a, Result<User, RepoError>> {
        Box::pin(async move {
            let result = sqlx::query(INSERT_USER_SQL)
                .bind(&new.email)
                .bind(new.resolve_display_name())
                .bind(password_hash)
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
        let expected = ["id", "email", "display_name", "password_hash", "created_at"];
        for column in expected {
            assert!(
                FIND_USER_BY_EMAIL_SQL.contains(column),
                "邮箱查询缺少 {column}"
            );
            assert!(SESSION_USER_SQL.contains(column), "会话查询缺少 {column}");
        }
    }

    #[test]
    fn session_lookup_requires_an_unexpired_row() {
        // 过期判定写在 SQL 里，避免「查回来再在内存里比时间」时漏掉某个分支
        assert!(SESSION_USER_SQL.contains("expires_at > ?"));
    }
}
