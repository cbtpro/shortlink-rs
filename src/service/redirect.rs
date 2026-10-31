//! 跳转用例。缓存与计数器是可降级依赖，出错只记告警，数据库才是事实来源。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use log::warn;

use crate::cache::{LinkCache, VisitCounter};
use crate::domain::short_link::{AccessPolicy, AccessRequest, CachedLink};
use crate::error::AppError;
use crate::repository::LinkRepository;

/// 解析短码后的落地方式。
///
/// 由用例层决定「给不给中转页」，接口层只管把变体变成响应 ——
/// 这样中转页是一条业务规则，而不是散在 handler 里的 if。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// 直接 302 到目标
    Direct(String),
    /// 先渲染中转确认页，再由页面上的一次点击进入目标
    Interstitial(String),
}

/// 一次解析的结果：目标地址、是否中转、访问策略。
struct Resolved {
    long_url: String,
    interstitial: bool,
    policy: AccessPolicy,
}

pub struct RedirectService {
    repository: Arc<dyn LinkRepository>,
    cache: Arc<dyn LinkCache>,
    counter: Arc<dyn VisitCounter>,
    cache_ttl: Duration,
}

impl RedirectService {
    pub fn new(
        repository: Arc<dyn LinkRepository>,
        cache: Arc<dyn LinkCache>,
        counter: Arc<dyn VisitCounter>,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            repository,
            cache,
            counter,
            cache_ttl,
        }
    }

    /// 解析短码，成功时返回落地方式。规则不过关会返回 [`AppError::AccessDenied`]。
    pub async fn resolve(&self, code: &str, password: Option<&str>) -> Result<Landing, AppError> {
        let resolved = self.load(code).await?;

        // 只有设了次数上限才需要读增量计数，避免缓存命中时多一次 Redis 往返
        let pending_visits = if resolved.policy.requires_visit_count() {
            self.pending_visits(code).await
        } else {
            0
        };
        resolved.policy.check(&AccessRequest {
            // 与库中的 `expire_at` 同一基准：存储层一律 UTC
            now: Utc::now().naive_utc(),
            password,
            pending_visits,
        })?;

        // 中转页打开就算一次访问：用户已经到达，是否点「继续访问」不再计一次
        self.record_visit(code).await;

        Ok(if resolved.interstitial {
            Landing::Interstitial(resolved.long_url)
        } else {
            Landing::Direct(resolved.long_url)
        })
    }

    /// 先查缓存，未命中再回源并回填。
    async fn load(&self, code: &str) -> Result<Resolved, AppError> {
        match self.cache.get(code).await {
            Ok(Some(cached)) => {
                return Ok(Resolved {
                    long_url: cached.long_url.clone(),
                    interstitial: cached.interstitial,
                    policy: AccessPolicy::from(&cached),
                });
            }
            Ok(None) => {}
            Err(err) => warn!("cache lookup failed for `{code}`, falling back to database: {err}"),
        }

        let link = self
            .repository
            .find_by_code(code)
            .await?
            .ok_or(AppError::LinkNotFound)?;

        if link.is_cacheable() {
            let cached = CachedLink::from(&link);
            if let Err(err) = self.cache.put(code, &cached, self.cache_ttl).await {
                warn!("failed to cache `{code}`: {err}");
            }
        }

        Ok(Resolved {
            long_url: link.long_url.clone(),
            interstitial: link.interstitial,
            policy: AccessPolicy::from(&link),
        })
    }

    async fn pending_visits(&self, code: &str) -> u32 {
        match self.counter.pending_of(code).await {
            Ok(count) => count,
            Err(err) => {
                warn!("failed to read pending visits for `{code}`: {err}");
                0
            }
        }
    }

    /// 访问计数只写 Redis，由后台任务批量回刷，避免每次跳转都写库。
    async fn record_visit(&self, code: &str) {
        if let Err(err) = self.counter.increment(code, self.cache_ttl).await {
            warn!("failed to buffer visit for `{code}`: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::new_link::NewShortLink;
    use crate::domain::short_link::AccessDenial;
    use crate::test_support::{
        BrokenCache, BrokenCounter, InMemoryLinkRepository, NoOpCache, NoOpCounter, RecordingCache,
        RecordingCounter,
    };
    use chrono::NaiveDate;

    fn service_with(repository: Arc<InMemoryLinkRepository>) -> RedirectService {
        RedirectService::new(
            repository,
            Arc::new(NoOpCache),
            Arc::new(NoOpCounter),
            Duration::from_secs(60),
        )
    }

    fn new_link(expire_at: Option<chrono::NaiveDateTime>, password: Option<&str>) -> NewShortLink {
        NewShortLink {
            url: "https://example.com".into(),
            password: password.map(str::to_string),
            expire_at,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn returns_long_url() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, None), "abc")
            .await
            .unwrap();

        let url = service_with(repository).resolve("abc", None).await.unwrap();
        assert_eq!(direct(url), "https://example.com");
    }

    #[tokio::test]
    async fn reports_missing_code() {
        let result = service_with(Arc::new(InMemoryLinkRepository::new()))
            .resolve("nope", None)
            .await;
        assert!(matches!(result, Err(AppError::LinkNotFound)));
    }

    #[tokio::test]
    async fn rejects_expired_link() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        let past = NaiveDate::from_ymd_opt(2020, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        repository
            .insert(&new_link(Some(past), None), "old")
            .await
            .unwrap();

        let result = service_with(repository).resolve("old", None).await;
        assert!(matches!(
            result,
            Err(AppError::AccessDenied(AccessDenial::Expired))
        ));
    }

    #[tokio::test]
    async fn requires_password() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, Some("secret")), "prot")
            .await
            .unwrap();

        let service = service_with(repository);
        assert!(matches!(
            service.resolve("prot", None).await,
            Err(AppError::AccessDenied(AccessDenial::MissingPassword))
        ));
        assert!(service.resolve("prot", Some("secret")).await.is_ok());
    }

    fn service_with_deps(
        repository: Arc<InMemoryLinkRepository>,
        cache: Arc<dyn LinkCache>,
        counter: Arc<dyn VisitCounter>,
    ) -> RedirectService {
        RedirectService::new(repository, cache, counter, Duration::from_secs(60))
    }

    fn cached(long_url: &str) -> CachedLink {
        CachedLink {
            id: 1,
            long_url: long_url.to_string(),
            expire_at: None,
            max_visits: None,
            visit_count: 0,
            interstitial: false,
        }
    }

    /// 断言是直跳，并取出目标地址；中转等同于失败。
    fn direct(landing: Landing) -> String {
        match landing {
            Landing::Direct(url) => url,
            other => panic!("expected a direct redirect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn serves_from_cache_without_hitting_repository() {
        // 库是空的：能返回结果就证明走的是缓存
        let cache = Arc::new(RecordingCache::new());
        cache.preload("hit", cached("https://cached.example"));

        let service = service_with_deps(
            Arc::new(InMemoryLinkRepository::new()),
            cache,
            Arc::new(NoOpCounter),
        );

        assert_eq!(
            direct(service.resolve("hit", None).await.unwrap()),
            "https://cached.example"
        );
    }

    #[tokio::test]
    async fn falls_back_to_database_when_cache_fails() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, None), "abc")
            .await
            .unwrap();

        let service = service_with_deps(repository, Arc::new(BrokenCache), Arc::new(NoOpCounter));

        assert_eq!(
            direct(service.resolve("abc", None).await.unwrap()),
            "https://example.com"
        );
    }

    #[tokio::test]
    async fn caches_only_unrestricted_links() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, None), "plain")
            .await
            .unwrap();

        let mut restricted = new_link(None, None);
        restricted.max_visits = Some(5);
        repository.insert(&restricted, "limited").await.unwrap();

        let cache = Arc::new(RecordingCache::new());
        let service = service_with_deps(repository, cache.clone(), Arc::new(NoOpCounter));

        service.resolve("plain", None).await.unwrap();
        service.resolve("limited", None).await.unwrap();

        assert!(cache.contains("plain"), "普通链接应回填缓存");
        assert!(!cache.contains("limited"), "带限制的链接不能进缓存");
    }

    fn interstitial_link() -> NewShortLink {
        NewShortLink {
            url: "https://example.com".into(),
            interstitial: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn interstitial_links_land_on_the_confirmation_page() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&interstitial_link(), "gate")
            .await
            .unwrap();

        let counter = Arc::new(RecordingCounter::new());
        let service = service_with_deps(repository, Arc::new(NoOpCache), counter.clone());

        assert_eq!(
            service.resolve("gate", None).await.unwrap(),
            Landing::Interstitial("https://example.com".into())
        );
        // 打开中转页就算到达，点「继续访问」不再计一次
        assert_eq!(counter.increments_of("gate"), 1);
    }

    #[tokio::test]
    async fn cached_interstitial_keeps_its_setting() {
        // 缓存命中时丢掉标记，就会「第一次中转、刷新后直跳」
        let cache = Arc::new(RecordingCache::new());
        let mut entry = cached("https://cached.example");
        entry.interstitial = true;
        cache.preload("gate", entry);

        let service = service_with_deps(
            Arc::new(InMemoryLinkRepository::new()),
            cache,
            Arc::new(NoOpCounter),
        );

        assert_eq!(
            service.resolve("gate", None).await.unwrap(),
            Landing::Interstitial("https://cached.example".into())
        );
    }

    #[tokio::test]
    async fn write_back_cache_carries_the_setting() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&interstitial_link(), "gate")
            .await
            .unwrap();

        let cache = Arc::new(RecordingCache::new());
        let service = service_with_deps(repository, cache.clone(), Arc::new(NoOpCounter));
        service.resolve("gate", None).await.unwrap();

        let stored = cache.get("gate").await.unwrap().expect("普通链接应被回填");
        assert!(stored.interstitial);
    }

    #[tokio::test]
    async fn records_visit_only_after_rules_pass() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, None), "abc")
            .await
            .unwrap();

        let counter = Arc::new(RecordingCounter::new());
        let service = service_with_deps(repository, Arc::new(NoOpCache), counter.clone());

        service.resolve("abc", None).await.unwrap();
        assert_eq!(counter.increments_of("abc"), 1);

        // 不存在的短码不该计数
        assert!(service.resolve("missing", None).await.is_err());
        assert_eq!(counter.increments_of("missing"), 0);
    }

    #[tokio::test]
    async fn rejects_when_pending_visits_already_reach_limit() {
        // 数据库里 visit_count 还是 0，但 Redis 里已有 1 次未回刷；
        // 上限为 1 时必须立刻拒绝，否则缓存窗口内会多放行一次
        let repository = Arc::new(InMemoryLinkRepository::new());
        let mut limited = new_link(None, None);
        limited.max_visits = Some(1);
        repository.insert(&limited, "lim").await.unwrap();

        let service = service_with_deps(
            repository,
            Arc::new(NoOpCache),
            Arc::new(RecordingCounter::with_pending("lim", 1)),
        );

        assert!(matches!(
            service.resolve("lim", None).await,
            Err(AppError::AccessDenied(AccessDenial::VisitLimitReached))
        ));
    }

    #[tokio::test]
    async fn survives_broken_counter() {
        // 计数是附属能力：Redis 挂了也不该影响跳转
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&new_link(None, None), "abc")
            .await
            .unwrap();

        let service = service_with_deps(repository, Arc::new(NoOpCache), Arc::new(BrokenCounter));

        assert_eq!(
            direct(service.resolve("abc", None).await.unwrap()),
            "https://example.com"
        );
    }
}
