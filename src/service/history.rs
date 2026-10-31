//! 访客的「最近创建」。按匿名访客隔离，不跨浏览器共享。

use std::sync::Arc;

use crate::domain::short_link::ShortLink;
use crate::error::AppError;
use crate::repository::CreationHistory;

/// 单页上限。历史是给用户找回刚建的链接用的，不是审计日志。
const MAX_HISTORY_LIMIT: u32 = 50;

pub struct HistoryService {
    history: Arc<dyn CreationHistory>,
    limit: u32,
}

impl HistoryService {
    pub fn new(history: Arc<dyn CreationHistory>, limit: u32) -> Self {
        Self {
            history,
            limit: limit.clamp(1, MAX_HISTORY_LIMIT),
        }
    }

    /// 记一笔创建。调用方应当把它当尽力而为：写失败只影响历史，不该让创建失败。
    pub async fn record(&self, visitor: &str, code: &str) -> Result<(), AppError> {
        Ok(self.history.record(visitor, code, self.limit).await?)
    }

    pub async fn recent(&self, visitor: &str) -> Result<Vec<ShortLink>, AppError> {
        Ok(self.history.recent(visitor, self.limit).await?)
    }

    pub async fn forget(&self, visitor: &str, code: &str) -> Result<bool, AppError> {
        Ok(self.history.forget(visitor, code).await?)
    }

    pub async fn clear(&self, visitor: &str) -> Result<u64, AppError> {
        Ok(self.history.clear(visitor).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_link::NewShortLink;
    use crate::repository::memory::InMemoryLinkRepository;
    use crate::repository::LinkRepository;

    const VISITOR: &str = "0123456789abcdef0123456789abcdef";

    async fn setup(limit: u32) -> (HistoryService, Arc<InMemoryLinkRepository>) {
        let repository = Arc::new(InMemoryLinkRepository::new());
        let service = HistoryService::new(repository.clone(), limit);
        (service, repository)
    }

    async fn create(repository: &InMemoryLinkRepository, code: &str) {
        repository
            .insert(
                &NewShortLink {
                    url: format!("https://example.com/{code}"),
                    ..Default::default()
                },
                code,
            )
            .await
            .expect("insert should succeed");
    }

    #[tokio::test]
    async fn keeps_newest_first() {
        let (service, repository) = setup(10).await;
        for code in ["a", "b", "c"] {
            create(&repository, code).await;
            service.record(VISITOR, code).await.unwrap();
        }

        let codes: Vec<_> = service
            .recent(VISITOR)
            .await
            .unwrap()
            .into_iter()
            .map(|link| link.code)
            .collect();
        assert_eq!(codes, vec!["c", "b", "a"]);
    }

    #[tokio::test]
    async fn trims_beyond_the_limit() {
        let (service, repository) = setup(2).await;
        for code in ["a", "b", "c"] {
            create(&repository, code).await;
            service.record(VISITOR, code).await.unwrap();
        }

        let codes: Vec<_> = service
            .recent(VISITOR)
            .await
            .unwrap()
            .into_iter()
            .map(|link| link.code)
            .collect();
        assert_eq!(codes, vec!["c", "b"], "写到上限后应淘汰最旧的");
    }

    #[tokio::test]
    async fn records_are_isolated_per_visitor() {
        let (service, repository) = setup(10).await;
        create(&repository, "mine").await;
        create(&repository, "yours").await;

        service.record(VISITOR, "mine").await.unwrap();
        service.record("ffffffffffffffffffffffffffffffff", "yours")
            .await
            .unwrap();

        assert_eq!(service.recent(VISITOR).await.unwrap().len(), 1);
        assert_eq!(
            service.recent("ffffffffffffffffffffffffffffffff").await.unwrap()[0].code,
            "yours"
        );
    }

    #[tokio::test]
    async fn hides_links_deleted_from_storage() {
        let (service, repository) = setup(10).await;
        create(&repository, "gone").await;
        service.record(VISITOR, "gone").await.unwrap();

        repository.delete_by_code("gone").await.unwrap();

        assert!(service.recent(VISITOR).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_and_clear_are_scoped_to_the_visitor() {
        let (service, repository) = setup(10).await;
        for code in ["a", "b"] {
            create(&repository, code).await;
            service.record(VISITOR, code).await.unwrap();
        }

        assert!(service.forget(VISITOR, "a").await.unwrap());
        assert!(!service.forget(VISITOR, "a").await.unwrap(), "重复删除返回 false");

        assert_eq!(service.clear(VISITOR).await.unwrap(), 1);
        assert!(service.recent(VISITOR).await.unwrap().is_empty());
        assert_eq!(service.clear(VISITOR).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn clamps_limit_to_a_sane_range() {
        // limit 来自配置，写错成 0 会让「刚创建就查不到」
        let (service, repository) = setup(0).await;
        create(&repository, "a").await;
        service.record(VISITOR, "a").await.unwrap();

        assert_eq!(service.recent(VISITOR).await.unwrap().len(), 1);
    }
}
