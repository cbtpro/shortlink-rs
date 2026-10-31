//! 定期清理过期会话。
//!
//! 会话过期是「逻辑过期」：鉴权时按 `expires_at` 判定，行还在也无所谓。
//! 但表会一直长，所以补一个低频清理，把这笔账结掉。

use std::time::Duration;

use log::{error, info};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::service::auth::AuthService;
use std::sync::Arc;

/// 清理周期。会话的有效期以天计，这里没有抢时效的必要。
pub const PERIOD: Duration = Duration::from_secs(6 * 60 * 60);

pub struct SessionPurger {
    auth: Arc<AuthService>,
}

impl SessionPurger {
    pub fn new(auth: Arc<AuthService>) -> Self {
        Self { auth }
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(PERIOD);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // 首次 tick 立即返回，先消费掉，避免启动瞬间白跑一次
            ticker.tick().await;
            info!("session purger started, period = {PERIOD:?}");

            loop {
                ticker.tick().await;
                match self.purge_once().await {
                    Ok(0) => {}
                    Ok(count) => info!("purged {count} expired session(s)"),
                    Err(err) => error!("session purge failed: {err}"),
                }
            }
        })
    }

    pub async fn purge_once(&self) -> anyhow::Result<u64> {
        Ok(self.auth.purge_expired_sessions().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::account::NewUser;
    use crate::test_support::{InMemoryLinkRepository, PlainHasher};

    #[tokio::test]
    async fn removes_expired_sessions_only() {
        let store = Arc::new(InMemoryLinkRepository::new());
        // TTL 为 0：签发的会话立刻过期
        let service = Arc::new(AuthService::new(
            store.clone(),
            store.clone(),
            Arc::new(PlainHasher),
            Duration::ZERO,
        ));

        service
            .register(NewUser {
                email: "a@example.com".into(),
                password: "secret123".into(),
                display_name: None,
            })
            .await
            .expect("register should succeed");

        let purger = SessionPurger::new(service);
        assert_eq!(purger.purge_once().await.unwrap(), 1);
        assert_eq!(purger.purge_once().await.unwrap(), 0);
    }
}
