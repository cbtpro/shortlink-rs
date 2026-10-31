//! 后台管理用例：概览、列表、删除。鉴权也在这里做，handler 只负责取凭证。

use std::sync::Arc;

use chrono::{DateTime, Utc};
use log::warn;

use crate::cache::LinkCache;
use crate::error::AppError;
use crate::repository::{LinkOverview, LinkPage, LinkQuery, LinkRepository, OverviewWindow};

/// 单页上限。后台表格是人工操作的地方，没人需要一次拉全表。
const MAX_PAGE_SIZE: u32 = 100;

/// 搜索词长度上限。
///
/// 短码最多 10 位、地址最多 2048，搜索词再长也没有意义；
/// 截断而不是报错 —— 搜索框里多贴了一串东西，给结果比给 400 有用。
const MAX_KEYWORD_LEN: usize = 100;

pub struct AdminService {
    repository: Arc<dyn LinkRepository>,
    cache: Arc<dyn LinkCache>,
    /// `None` 表示未启用后台，此时所有管理操作都返回 404
    token: Option<String>,
}

impl AdminService {
    pub fn new(
        repository: Arc<dyn LinkRepository>,
        cache: Arc<dyn LinkCache>,
        token: Option<String>,
    ) -> Self {
        Self {
            repository,
            cache,
            token,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.token.is_some()
    }

    /// 校验请求携带的令牌。
    pub fn authorize(&self, presented: Option<&str>) -> Result<(), AppError> {
        let Some(expected) = self.token.as_deref() else {
            return Err(AppError::AdminDisabled);
        };

        match presented {
            Some(value) if constant_time_eq(value.as_bytes(), expected.as_bytes()) => Ok(()),
            _ => Err(AppError::Unauthorized),
        }
    }

    pub async fn list(&self, query: LinkQuery, now: DateTime<Utc>) -> Result<LinkPage, AppError> {
        Ok(self.repository.list(&clamp(query), now.naive_utc()).await?)
    }

    pub async fn overview(&self, window: OverviewWindow) -> Result<LinkOverview, AppError> {
        Ok(self.repository.overview(window).await?)
    }

    pub async fn delete(&self, code: &str) -> Result<(), AppError> {
        if !self.repository.delete_by_code(code).await? {
            return Err(AppError::LinkNotFound);
        }

        // 先删库再清缓存。反过来的话，清缓存到删库之间进来的请求
        // 会把记录重新写回缓存，于是已删除的链接还能再跳一段时间。
        if let Err(err) = self.cache.evict(code).await {
            warn!("failed to evict cache for `{code}` after delete: {err}");
        }

        Ok(())
    }
}

/// 收敛分页参数：`page` 至少为 1，`page_size` 落在 `1..=MAX_PAGE_SIZE`。
///
/// 放在用例层而不是路由层，换协议（gRPC / CLI）时同样受保护。
fn clamp(query: LinkQuery) -> LinkQuery {
    LinkQuery {
        page: query.page.max(1),
        page_size: query.page_size.clamp(1, MAX_PAGE_SIZE),
        keyword: query
            .keyword
            .map(|keyword| keyword.trim().chars().take(MAX_KEYWORD_LEN).collect())
            .filter(|keyword: &String| !keyword.is_empty()),
        filter: query.filter,
    }
}

/// 常量时间比较，避免用响应耗时逐字节猜出令牌。
///
/// 长度不等会提前返回，这是有意的：长度本身不算秘密，
/// 逐个字节的比较才是需要抹平的信号。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};

    use crate::domain::new_link::NewShortLink;
    use crate::test_support::{BrokenRepository, InMemoryLinkRepository, RecordingCache};

    const TOKEN: &str = "s3cret-token";

    fn service(
        token: Option<&str>,
    ) -> (
        AdminService,
        Arc<InMemoryLinkRepository>,
        Arc<RecordingCache>,
    ) {
        let repository = Arc::new(InMemoryLinkRepository::new());
        let cache = Arc::new(RecordingCache::new());
        let service =
            AdminService::new(repository.clone(), cache.clone(), token.map(str::to_string));
        (service, repository, cache)
    }

    fn query() -> LinkQuery {
        LinkQuery {
            page: 1,
            page_size: 10,
            keyword: None,
            filter: Default::default(),
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn disabled_admin_reports_404_even_with_token() {
        let (service, _, _) = service(None);
        assert!(!service.is_enabled());
        // 未启用时，就算令牌碰巧对了也不能放进来说明有后台
        assert!(matches!(
            service.authorize(Some(TOKEN)),
            Err(AppError::AdminDisabled)
        ));
    }

    #[test]
    fn accepts_only_exact_token() {
        let (service, _, _) = service(Some(TOKEN));

        assert!(service.authorize(Some(TOKEN)).is_ok());
        assert!(matches!(
            service.authorize(None),
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            service.authorize(Some("s3cret-toke")),
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            service.authorize(Some("s3cret-token2")),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn constant_time_compare_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[tokio::test]
    async fn clamps_pagination_and_trims_keyword() {
        let (service, repository, _) = service(Some(TOKEN));
        for code in ["a", "b", "c"] {
            repository
                .insert(
                    &NewShortLink {
                        url: "https://example.com".into(),
                        ..Default::default()
                    },
                    code,
                )
                .await
                .unwrap();
        }

        // page=0 会被当成 1；page_size 超过上限时按上限截断
        let page = service
            .list(
                LinkQuery {
                    page: 0,
                    page_size: 1000,
                    keyword: Some("   ".into()),
                    filter: Default::default(),
                },
                at(12),
            )
            .await
            .unwrap();

        assert_eq!(page.total, 3);
        assert_eq!(page.items.len(), 3, "空关键字不应过滤掉任何记录");
    }

    #[test]
    fn truncates_overlong_keyword() {
        let clamped = clamp(LinkQuery {
            page: 1,
            page_size: 20,
            keyword: Some("a".repeat(MAX_KEYWORD_LEN + 50)),
            filter: Default::default(),
        });

        assert_eq!(
            clamped.keyword.as_deref().map(str::len),
            Some(MAX_KEYWORD_LEN),
            "超长搜索词应被截断而不是原样带进 LIKE"
        );
    }

    #[tokio::test]
    async fn delete_removes_link_and_evicts_cache() {
        let (service, repository, cache) = service(Some(TOKEN));
        repository
            .insert(
                &NewShortLink {
                    url: "https://example.com".into(),
                    ..Default::default()
                },
                "gone",
            )
            .await
            .unwrap();
        cache.preload(
            "gone",
            crate::domain::short_link::CachedLink {
                id: 1,
                long_url: "https://example.com".into(),
                expire_at: None,
                max_visits: None,
                visit_count: 0,
                interstitial: false,
            },
        );

        service.delete("gone").await.unwrap();

        assert!(repository.find_by_code("gone").await.unwrap().is_none());
        assert!(
            !cache.contains("gone"),
            "删链后必须清缓存，否则还能跳到 TTL 结束"
        );
    }

    #[tokio::test]
    async fn delete_reports_missing_link() {
        let (service, _, _) = service(Some(TOKEN));
        assert!(matches!(
            service.delete("ghost").await,
            Err(AppError::LinkNotFound)
        ));
    }

    #[tokio::test]
    async fn delete_survives_cache_failure() {
        // 缓存清不掉不该让删除失败：数据库已经是事实来源
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(
                &NewShortLink {
                    url: "https://example.com".into(),
                    ..Default::default()
                },
                "gone",
            )
            .await
            .unwrap();

        let service = AdminService::new(
            repository.clone(),
            Arc::new(crate::test_support::BrokenCache),
            Some(TOKEN.to_string()),
        );

        assert!(service.delete("gone").await.is_ok());
        assert!(repository.find_by_code("gone").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn storage_failure_becomes_server_error() {
        let service = AdminService::new(
            Arc::new(BrokenRepository),
            Arc::new(RecordingCache::new()),
            Some(TOKEN.to_string()),
        );

        assert!(matches!(
            service.list(query(), at(12)).await,
            Err(AppError::Storage(_))
        ));
        assert!(matches!(
            service.delete("a").await,
            Err(AppError::Storage(_))
        ));
        assert!(matches!(
            service
                .overview(OverviewWindow {
                    now: at(12).naive_utc(),
                    today_start: at(0).naive_utc(),
                    expiring_before: (at(12) + Duration::days(7)).naive_utc(),
                })
                .await,
            Err(AppError::Storage(_))
        ));
    }

    #[tokio::test]
    async fn list_uses_the_supplied_clock() {
        // 状态筛选必须基于调用方传入的时刻，用例层不能自己偷偷取时钟
        let (service, repository, _) = service(Some(TOKEN));
        let base = at(12);
        repository
            .insert(
                &NewShortLink {
                    url: "https://example.com".into(),
                    expire_at: Some((base + Duration::hours(1)).naive_utc()),
                    ..Default::default()
                },
                "later",
            )
            .await
            .unwrap();

        let expired_now = service
            .list(
                LinkQuery {
                    filter: crate::repository::LinkFilter::Expired,
                    ..query()
                },
                base,
            )
            .await
            .unwrap();
        assert_eq!(expired_now.total, 0, "此刻还没到期");

        let expired_later = service
            .list(
                LinkQuery {
                    filter: crate::repository::LinkFilter::Expired,
                    ..query()
                },
                base + Duration::hours(2),
            )
            .await
            .unwrap();
        assert_eq!(expired_later.total, 1, "两小时后应判为已过期");
    }
}
