//! 注册、登录、注销与请求鉴权。
//!
//! 会话用的是「不透明令牌 + 服务端记录」，不是 JWT：
//! 注销要能立刻生效，JWT 做不到（除非再引入一份黑名单，等于把会话表换个形式存回来）。

use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};

use crate::domain::account::{NewUser, User};
use crate::error::AppError;
use crate::repository::{SessionRepository, UserRepository};
use crate::security::token;
use crate::security::PasswordHasher;

/// 登录成功后的凭据：令牌写进 cookie，用户信息回给前端。
pub struct IssuedSession {
    pub token: String,
    pub user: User,
}

impl std::fmt::Debug for IssuedSession {
    /// 手写 Debug 而不是 derive：这个结构会出现在测试失败信息和日志里，
    /// 令牌本身不该跟着被打印出来。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedSession")
            .field("token", &"<redacted>")
            .field("user", &self.user)
            .finish()
    }
}

pub struct AuthService {
    users: Arc<dyn UserRepository>,
    sessions: Arc<dyn SessionRepository>,
    hasher: Arc<dyn PasswordHasher>,
    session_ttl: Duration,
    /// 邮箱不存在时拿来做等价耗时校验的靶子。
    ///
    /// 不这么做的话，「查不到用户」会立刻返回，比「密码算错」快上几百毫秒，
    /// 光凭响应时间就能问出某个邮箱有没有注册过。
    decoy_hash: String,
}

impl AuthService {
    pub fn new(
        users: Arc<dyn UserRepository>,
        sessions: Arc<dyn SessionRepository>,
        hasher: Arc<dyn PasswordHasher>,
        session_ttl: Duration,
    ) -> Self {
        let decoy_hash = hasher.hash("decoy-password-nobody-uses");
        Self {
            users,
            sessions,
            hasher,
            session_ttl,
            decoy_hash,
        }
    }

    /// 会话有效期。api 层据此设置 cookie 的 `Max-Age`。
    pub fn session_ttl(&self) -> Duration {
        self.session_ttl
    }

    pub async fn register(&self, new: NewUser) -> Result<IssuedSession, AppError> {
        let new = new.normalized();
        new.validate()?;

        let password_hash = self.hash(&new.password).await?;
        let user = self.users.create(&new, &password_hash).await?;
        let token = self.issue(&user).await?;

        Ok(IssuedSession { token, user })
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<IssuedSession, AppError> {
        let email = email.trim().to_ascii_lowercase();
        let found = self.users.find_by_email(&email).await?;

        // 无论有没有这个邮箱，都跑一次同样代价的校验
        let (user, hash) = match &found {
            Some(user) => (Some(user.clone()), user.password_hash.clone()),
            None => (None, self.decoy_hash.clone()),
        };
        let matches = self.verify(password.to_owned(), hash).await?;

        match (user, matches) {
            (Some(user), true) => {
                let token = self.issue(&user).await?;
                Ok(IssuedSession { token, user })
            }
            // 两种情况给同一个错误：不告诉对方是邮箱还是密码错了
            _ => Err(AppError::InvalidCredentials),
        }
    }

    /// 用 cookie 里的令牌换用户。令牌无效 / 过期 / 格式不对都返回 `None`。
    pub async fn authenticate(&self, token: &str) -> Result<Option<User>, AppError> {
        if !token::is_well_formed(token) {
            return Ok(None);
        }

        let user = self
            .sessions
            .find_user(&token::fingerprint(token), Utc::now().naive_utc())
            .await?;
        Ok(user)
    }

    /// 注销。令牌无效时静默成功 —— 客户端要的结果是「这个会话没了」。
    pub async fn logout(&self, token: Option<&str>) -> Result<(), AppError> {
        let Some(token) = token.filter(|value| token::is_well_formed(value)) else {
            return Ok(());
        };

        self.sessions.delete(&token::fingerprint(token)).await?;
        Ok(())
    }

    /// 清理过期会话，供后台任务调用。
    pub async fn purge_expired_sessions(&self) -> Result<u64, AppError> {
        Ok(self.sessions.purge_expired(Utc::now().naive_utc()).await?)
    }

    /// 散列口令。
    ///
    /// PBKDF2 是故意设计成「算得慢」的，几十万轮下来单次几百毫秒。
    /// 直接在 async 函数里跑会占死一个 actix worker 线程 —— worker 数和 CPU 核数相当，
    /// 几个人同时注册就能把服务堵住。所以丢进阻塞线程池。
    async fn hash(&self, password: &str) -> Result<String, AppError> {
        let hasher = self.hasher.clone();
        let password = password.to_owned();
        run_blocking(move || hasher.hash(&password)).await
    }

    /// 校验口令，同样丢进阻塞线程池。
    async fn verify(&self, password: String, encoded: String) -> Result<bool, AppError> {
        let hasher = self.hasher.clone();
        run_blocking(move || hasher.verify(&password, &encoded)).await
    }

    /// 签发会话。每次登录都换一个新令牌：旧令牌留在别的设备上也该各自独立失效。
    async fn issue(&self, user: &User) -> Result<String, AppError> {
        let token = token::mint();
        let expires_at = Utc::now().naive_utc() + self.ttl();

        self.sessions
            .create(user.id, &token::fingerprint(&token), expires_at)
            .await?;

        Ok(token)
    }

    fn ttl(&self) -> ChronoDuration {
        ChronoDuration::seconds(self.session_ttl.as_secs().min(i64::MAX as u64) as i64)
    }
}

/// 在阻塞线程池里跑一段 CPU 密集的活，并把 panic 转成 500。
async fn run_blocking<T, F>(job: F) -> Result<T, AppError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(value) => Ok(value),
        // 散列实现 panic 属于程序缺陷，用户侧只该看到 500；细节留给日志
        Err(err) => Err(AppError::Internal(format!(
            "password hashing failed: {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{InMemoryLinkRepository, PlainHasher};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    struct Harness {
        service: AuthService,
    }

    fn harness() -> Harness {
        harness_with_ttl(DAY)
    }

    fn harness_with_ttl(ttl: Duration) -> Harness {
        let store = Arc::new(InMemoryLinkRepository::new());
        let service = AuthService::new(store.clone(), store.clone(), Arc::new(PlainHasher), ttl);
        Harness { service }
    }

    fn signup(email: &str, password: &str) -> NewUser {
        NewUser {
            email: email.into(),
            password: password.into(),
            display_name: None,
        }
    }

    #[tokio::test]
    async fn register_creates_a_usable_session() {
        let h = harness();

        let session = h
            .service
            .register(signup("Alice@Example.com", "secret123"))
            .await
            .expect("register should succeed");

        assert_eq!(session.user.email, "alice@example.com");
        assert_eq!(session.user.display_name, "alice");
        assert!(token::is_well_formed(&session.token));

        let current = h
            .service
            .authenticate(&session.token)
            .await
            .unwrap()
            .expect("session should resolve");
        assert_eq!(current.id, session.user.id);
    }

    #[tokio::test]
    async fn register_rejects_duplicate_email_case_insensitively() {
        let h = harness();
        h.service
            .register(signup("alice@example.com", "secret123"))
            .await
            .unwrap();

        let again = h
            .service
            .register(signup("ALICE@example.com", "secret123"))
            .await;

        assert!(matches!(again, Err(AppError::EmailTaken)));
    }

    #[tokio::test]
    async fn register_rejects_weak_input() {
        let h = harness();

        assert!(matches!(
            h.service.register(signup("nope", "secret123")).await,
            Err(AppError::Account(_))
        ));
        assert!(matches!(
            h.service.register(signup("b@example.com", "short")).await,
            Err(AppError::Account(_))
        ));
    }

    #[tokio::test]
    async fn login_accepts_the_right_password() {
        let h = harness();
        h.service
            .register(signup("bob@example.com", "secret123"))
            .await
            .unwrap();

        let session = h
            .service
            .login("  BOB@example.com ", "secret123")
            .await
            .expect("login should succeed");

        assert_ne!(session.user.password_hash, "", "内部要有散列");
        assert!(h
            .service
            .authenticate(&session.token)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn login_rejects_wrong_password_and_unknown_email_alike() {
        let h = harness();
        h.service
            .register(signup("bob@example.com", "secret123"))
            .await
            .unwrap();

        let wrong = h.service.login("bob@example.com", "secret124").await;
        let unknown = h.service.login("nobody@example.com", "secret123").await;

        assert!(matches!(wrong, Err(AppError::InvalidCredentials)));
        assert!(matches!(unknown, Err(AppError::InvalidCredentials)));
        assert_eq!(
            wrong.unwrap_err().to_string(),
            unknown.unwrap_err().to_string(),
            "两种失败必须给出同一句话"
        );
    }

    #[tokio::test]
    async fn each_login_gets_its_own_token() {
        let h = harness();
        h.service
            .register(signup("c@example.com", "secret123"))
            .await
            .unwrap();

        let first = h.service.login("c@example.com", "secret123").await.unwrap();
        let second = h.service.login("c@example.com", "secret123").await.unwrap();

        assert_ne!(first.token, second.token);
        // 两个都该有效：换设备登录不该踢掉另一台
        assert!(h
            .service
            .authenticate(&first.token)
            .await
            .unwrap()
            .is_some());
        assert!(h
            .service
            .authenticate(&second.token)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn logout_invalidates_only_that_token() {
        let h = harness();
        h.service
            .register(signup("d@example.com", "secret123"))
            .await
            .unwrap();
        let first = h.service.login("d@example.com", "secret123").await.unwrap();
        let second = h.service.login("d@example.com", "secret123").await.unwrap();

        h.service.logout(Some(&first.token)).await.unwrap();

        assert!(h
            .service
            .authenticate(&first.token)
            .await
            .unwrap()
            .is_none());
        assert!(h
            .service
            .authenticate(&second.token)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn logout_tolerates_missing_or_bogus_tokens() {
        let h = harness();
        assert!(h.service.logout(None).await.is_ok());
        assert!(h.service.logout(Some("not-a-token")).await.is_ok());
        assert!(h.service.logout(Some(&token::mint())).await.is_ok());
    }

    #[tokio::test]
    async fn expired_sessions_stop_working() {
        // TTL 为 0：刚签发就已过期
        let h = harness_with_ttl(Duration::ZERO);
        let session = h
            .service
            .register(signup("e@example.com", "secret123"))
            .await
            .unwrap();

        assert!(h
            .service
            .authenticate(&session.token)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn malformed_tokens_never_hit_storage() {
        let h = harness();
        for bogus in ["", "abc", &"z".repeat(64)] {
            assert!(h.service.authenticate(bogus).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn purge_removes_only_expired_sessions() {
        let h = harness_with_ttl(Duration::ZERO);
        h.service
            .register(signup("f@example.com", "secret123"))
            .await
            .unwrap();

        assert_eq!(h.service.purge_expired_sessions().await.unwrap(), 1);
        assert_eq!(h.service.purge_expired_sessions().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn exposes_the_configured_ttl() {
        assert_eq!(harness_with_ttl(DAY).service.session_ttl(), DAY);
    }
}
