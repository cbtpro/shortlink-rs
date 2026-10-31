//! 「我的链接」：登录用户名下的短链。
//!
//! 与匿名访客的「最近创建」不是同一件事：
//! - 匿名记录只是本浏览器的清单，删掉记录不影响短链；
//! - 用户名下的链接是归属关系，删除就是真的让短链失效。
//!
//! 所以两者的删除语义刻意分开，不共用同一个 handler 分支。

use std::sync::Arc;

use log::warn;

use crate::cache::LinkCache;
use crate::domain::short_link::ShortLink;
use crate::error::AppError;
use crate::repository::LinkRepository;

/// 列表上限。和匿名历史同量级：这是给人找回刚建的链接用的，不是导出全量。
const MAX_LIST_LIMIT: u32 = 100;

pub struct MyLinksService {
    repository: Arc<dyn LinkRepository>,
    cache: Arc<dyn LinkCache>,
    limit: u32,
}

impl MyLinksService {
    pub fn new(repository: Arc<dyn LinkRepository>, cache: Arc<dyn LinkCache>, limit: u32) -> Self {
        Self {
            repository,
            cache,
            limit: limit.clamp(1, MAX_LIST_LIMIT),
        }
    }

    pub async fn list(&self, user_id: u64) -> Result<Vec<ShortLink>, AppError> {
        Ok(self.repository.list_by_owner(user_id, self.limit).await?)
    }

    /// 删除自己的一条链接。
    ///
    /// 「不存在」与「不是你的」都返回 404：后者若回 403，等于告诉别人这个短码存在。
    pub async fn delete(&self, user_id: u64, code: &str) -> Result<(), AppError> {
        if !self.repository.delete_owned(user_id, code).await? {
            return Err(AppError::LinkNotFound);
        }

        // 先删库再清缓存：反过来的话，清缓存到删库之间进来的请求会把记录写回缓存
        self.evict(code).await;
        Ok(())
    }

    /// 删除自己名下的全部链接，返回条数。
    pub async fn clear(&self, user_id: u64) -> Result<u64, AppError> {
        let codes = self.repository.delete_all_owned(user_id).await?;

        for code in &codes {
            self.evict(code).await;
        }

        Ok(codes.len() as u64)
    }

    /// 缓存失效失败只记日志：数据库已经是事实来源，删链不该因为 Redis 抖动而失败。
    async fn evict(&self, code: &str) {
        if let Err(err) = self.cache.evict(code).await {
            warn!("failed to evict cache for `{code}` after delete: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_link::NewShortLink;
    use crate::domain::short_link::CachedLink;
    use crate::test_support::{InMemoryLinkRepository, RecordingCache};

    struct Harness {
        service: MyLinksService,
        store: Arc<InMemoryLinkRepository>,
        cache: Arc<RecordingCache>,
    }

    fn harness() -> Harness {
        let store = Arc::new(InMemoryLinkRepository::new());
        let cache = Arc::new(RecordingCache::new());
        let service = MyLinksService::new(store.clone(), cache.clone(), 20);
        Harness {
            service,
            store,
            cache,
        }
    }

    async fn create(store: &InMemoryLinkRepository, code: &str, owner: Option<u64>) {
        store
            .insert(
                &NewShortLink {
                    url: format!("https://example.com/{code}"),
                    owner_id: owner,
                    ..Default::default()
                },
                code,
            )
            .await
            .expect("insert should succeed");
    }

    fn cached() -> CachedLink {
        CachedLink {
            id: 1,
            long_url: "https://example.com".into(),
            expire_at: None,
            max_visits: None,
            visit_count: 0,
            interstitial: false,
        }
    }

    #[tokio::test]
    async fn lists_only_own_links() {
        let h = harness();
        create(&h.store, "mine", Some(1)).await;
        create(&h.store, "theirs", Some(2)).await;
        create(&h.store, "anon", None).await;

        let codes: Vec<String> = h
            .service
            .list(1)
            .await
            .unwrap()
            .into_iter()
            .map(|link| link.code)
            .collect();

        assert_eq!(codes, vec!["mine"]);
    }

    #[tokio::test]
    async fn lists_newest_first_and_respects_the_limit() {
        let h = harness();
        for code in ["a", "b", "c", "d"] {
            create(&h.store, code, Some(1)).await;
        }

        let service = MyLinksService::new(h.store.clone(), h.cache.clone(), 2);
        let codes: Vec<String> = service
            .list(1)
            .await
            .unwrap()
            .into_iter()
            .map(|link| link.code)
            .collect();

        assert_eq!(codes, vec!["d", "c"]);
    }

    #[tokio::test]
    async fn delete_removes_the_link_and_its_cache_entry() {
        let h = harness();
        create(&h.store, "gone", Some(1)).await;
        h.cache.preload("gone", cached());

        h.service.delete(1, "gone").await.unwrap();

        assert!(h.store.find_by_code("gone").await.unwrap().is_none());
        assert!(!h.cache.contains("gone"), "删链后必须清缓存");
    }

    #[tokio::test]
    async fn delete_refuses_links_owned_by_someone_else() {
        let h = harness();
        create(&h.store, "theirs", Some(2)).await;

        let result = h.service.delete(1, "theirs").await;

        assert!(matches!(result, Err(AppError::LinkNotFound)));
        assert!(
            h.store.find_by_code("theirs").await.unwrap().is_some(),
            "别人的链接不能被顺带删掉"
        );
    }

    #[tokio::test]
    async fn delete_refuses_anonymous_links() {
        // 匿名链接没有归属，任何人都不能凭账号删它
        let h = harness();
        create(&h.store, "anon", None).await;

        assert!(matches!(
            h.service.delete(1, "anon").await,
            Err(AppError::LinkNotFound)
        ));
        assert!(h.store.find_by_code("anon").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_reports_missing_code() {
        let h = harness();
        assert!(matches!(
            h.service.delete(1, "ghost").await,
            Err(AppError::LinkNotFound)
        ));
    }

    #[tokio::test]
    async fn clear_deletes_only_own_links() {
        let h = harness();
        create(&h.store, "a", Some(1)).await;
        create(&h.store, "b", Some(1)).await;
        create(&h.store, "theirs", Some(2)).await;
        h.cache.preload("a", cached());
        h.cache.preload("b", cached());

        assert_eq!(h.service.clear(1).await.unwrap(), 2);

        assert!(h.store.find_by_code("a").await.unwrap().is_none());
        assert!(h.store.find_by_code("b").await.unwrap().is_none());
        assert!(h.store.find_by_code("theirs").await.unwrap().is_some());
        assert!(!h.cache.contains("a") && !h.cache.contains("b"));
        assert_eq!(h.service.clear(1).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn clamps_limit_to_a_sane_range() {
        let h = harness();
        create(&h.store, "a", Some(1)).await;

        let service = MyLinksService::new(h.store.clone(), h.cache.clone(), 0);
        assert_eq!(
            service.list(1).await.unwrap().len(),
            1,
            "0 会让列表永远是空的"
        );
    }
}
