//! 把 Redis 中的访问计数增量定期回刷到数据库。
//!
//! 按增量抵扣而不是整条覆盖，避免覆盖掉回刷间隙的并发访问。

use std::sync::Arc;
use std::time::Duration;

use log::{error, info, warn};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};

use crate::cache::VisitCounter;
use crate::repository::{LinkRepository, RepoError};

pub struct VisitFlusher {
    repository: Arc<dyn LinkRepository>,
    counter: Arc<dyn VisitCounter>,
}

impl VisitFlusher {
    pub fn new(repository: Arc<dyn LinkRepository>, counter: Arc<dyn VisitCounter>) -> Self {
        Self {
            repository,
            counter,
        }
    }

    /// 启动常驻任务。
    pub fn spawn(self, period: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = interval(period);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // interval 首次 tick 会立即返回，先消费掉，避免启动瞬间空跑一次
            ticker.tick().await;

            info!("visit flusher started, period = {period:?}");

            loop {
                ticker.tick().await;
                match self.flush_once().await {
                    Ok(written) => info!("visit flusher persisted {written} link(s)"),
                    Err(err) => error!("visit flush failed: {err}"),
                }
            }
        })
    }

    /// 执行一次回刷，返回成功写入的链接数量。
    pub async fn flush_once(&self) -> anyhow::Result<usize> {
        let pending = self.counter.scan_pending().await?;
        let mut written = 0;

        for visit in pending {
            match self
                .repository
                .add_visit_count(&visit.code, visit.count)
                .await
            {
                Ok(()) => {
                    // 只有落库成功才抵扣，失败则留到下一轮重试
                    if let Err(err) = self.counter.consume(visit).await {
                        warn!("failed to consume visit counter: {err}");
                    }
                    written += 1;
                }
                // 记录已被删除：增量没有归宿，丢弃比每轮重试更合理，
                // 否则后台会为一条不存在的短码持续刷告警直到 key 过期
                Err(RepoError::NotFound) => {
                    warn!(
                        "dropping {} pending visit(s) for `{}`: link no longer exists",
                        visit.count, visit.code
                    );
                    if let Err(err) = self.counter.consume(visit).await {
                        warn!("failed to consume visit counter: {err}");
                    }
                }
                Err(err) => warn!("failed to persist visits: {err}"),
            }
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_link::NewShortLink;
    use crate::test_support::{
        BrokenCounter, BrokenRepository, InMemoryLinkRepository, RecordingCounter,
    };

    fn link() -> NewShortLink {
        NewShortLink {
            url: "https://example.com".into(),
            ..Default::default()
        }
    }

    async fn repository_with(codes: &[&str]) -> Arc<InMemoryLinkRepository> {
        let repository = Arc::new(InMemoryLinkRepository::new());
        for code in codes {
            repository.insert(&link(), code).await.expect("seed insert");
        }
        repository
    }

    async fn visit_count_of(repository: &InMemoryLinkRepository, code: &str) -> u32 {
        repository
            .find_by_code(code)
            .await
            .expect("lookup should succeed")
            .expect("link should exist")
            .visit_count
    }

    #[tokio::test]
    async fn persists_pending_counts_and_clears_them() {
        let repository = repository_with(&["a", "b"]).await;
        let counter = Arc::new(RecordingCounter::new());
        counter.seed("a", 3).seed("b", 5);

        let flusher = VisitFlusher::new(repository.clone(), counter.clone());
        let written = flusher.flush_once().await.expect("flush should succeed");

        assert_eq!(written, 2);
        assert_eq!(visit_count_of(&repository, "a").await, 3);
        assert_eq!(visit_count_of(&repository, "b").await, 5);
        // 落库成功后必须抵扣，否则下一轮会重复累加
        assert_eq!(counter.pending_of_sync("a"), 0);
        assert_eq!(counter.pending_of_sync("b"), 0);
    }

    #[tokio::test]
    async fn accumulates_across_rounds_instead_of_overwriting() {
        // 这是「访问数被回滚」那个 bug 的回归测试：两轮增量必须累加
        let repository = repository_with(&["a"]).await;
        let counter = Arc::new(RecordingCounter::new());
        let flusher = VisitFlusher::new(repository.clone(), counter.clone());

        counter.seed("a", 2);
        flusher.flush_once().await.expect("first flush");
        counter.seed("a", 3);
        flusher.flush_once().await.expect("second flush");

        assert_eq!(visit_count_of(&repository, "a").await, 5);
    }

    #[tokio::test]
    async fn keeps_counter_when_database_write_fails() {
        // 存储不可用：必须保留增量，否则这段访问量会静默丢失
        let repository: Arc<dyn LinkRepository> = Arc::new(BrokenRepository);
        let counter = Arc::new(RecordingCounter::new());
        counter.seed("a", 2);

        let flusher = VisitFlusher::new(repository, counter.clone());
        let written = flusher.flush_once().await.expect("flush should not abort");

        assert_eq!(written, 0);
        assert_eq!(counter.pending_of_sync("a"), 2, "失败应留到下一轮重试");
    }

    #[tokio::test]
    async fn drops_counter_when_link_is_gone() {
        // 与「存储故障」相反：记录确实不存在时重试永远不会成功，应直接丢弃
        let repository: Arc<dyn LinkRepository> = Arc::new(InMemoryLinkRepository::new());
        let counter = Arc::new(RecordingCounter::new());
        counter.seed("ghost", 2);

        let flusher = VisitFlusher::new(repository, counter.clone());
        let written = flusher.flush_once().await.expect("flush should not abort");

        assert_eq!(written, 0);
        assert_eq!(
            counter.pending_of_sync("ghost"),
            0,
            "链接已删除，增量不该无限保留"
        );
    }

    #[tokio::test]
    async fn empty_counter_is_a_no_op() {
        let flusher = VisitFlusher::new(
            Arc::new(InMemoryLinkRepository::new()),
            Arc::new(RecordingCounter::new()),
        );

        assert_eq!(flusher.flush_once().await.expect("flush"), 0);
    }

    #[tokio::test]
    async fn reports_scan_failure() {
        let flusher = VisitFlusher::new(
            Arc::new(InMemoryLinkRepository::new()),
            Arc::new(BrokenCounter),
        );

        assert!(flusher.flush_once().await.is_err());
    }
}
