//! 跳转用例。缓存与计数器是可降级依赖，出错只记告警，数据库才是事实来源。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use log::warn;

use crate::cache::{GateTicket, LinkCache, VisitCounter};
use crate::domain::short_link::{AccessPolicy, AccessRequest, CachedLink};
use crate::error::AppError;
use crate::repository::LinkRepository;

/// 中转页「继续访问」票据的有效期。
///
/// 用户可能在确认页上读一读、想一想，给足十分钟；票据是一次性的，
/// 过期的代价只是重新打开一次中转页。
const CONFIRM_TICKET_TTL: Duration = Duration::from_secs(10 * 60);

/// 解析短码后的落地方式。
///
/// 由用例层决定「给不给中转页」，接口层只管把变体变成响应 ——
/// 这样中转页是一条业务规则，而不是散在 handler 里的 if。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    /// 直接 302 到目标
    Direct(String),
    /// 先渲染中转确认页，再由页面上的一次点击进入目标。
    ///
    /// `ticket` 是「继续访问」的凭据：中转页只是确认页，不算一次访问，
    /// 点下去的那次跳转才算。`None` 表示票据仓库不可用（Redis 挂了），
    /// 页面只能直连目标 —— 这一跳就统计不到了，属于可接受的降级。
    Interstitial {
        target: String,
        ticket: Option<String>,
    },
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
    tickets: Arc<dyn GateTicket>,
    cache_ttl: Duration,
}

impl RedirectService {
    pub fn new(
        repository: Arc<dyn LinkRepository>,
        cache: Arc<dyn LinkCache>,
        counter: Arc<dyn VisitCounter>,
        tickets: Arc<dyn GateTicket>,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            repository,
            cache,
            counter,
            tickets,
            cache_ttl,
        }
    }

    /// 解析短码，成功时返回落地方式。规则不过关会返回 [`AppError::AccessDenied`]。
    pub async fn resolve(&self, code: &str, password: Option<&str>) -> Result<Landing, AppError> {
        let resolved = self.load(code).await?;

        resolved.policy.check(&AccessRequest {
            // 与库中的 `expire_at` 同一基准：存储层一律 UTC
            now: Utc::now().naive_utc(),
            password,
            pending_visits: self.pending_visits(code, &resolved.policy).await,
            authenticated: false,
        })?;

        if resolved.interstitial {
            // 中转页只是确认页：到这一步还不算「到达」。
            // 发一张票据，等访问者真的点「继续访问」再计数。
            return Ok(Landing::Interstitial {
                target: resolved.long_url,
                ticket: self.issue_ticket(code).await,
            });
        }

        self.record_visit(code).await;
        Ok(Landing::Direct(resolved.long_url))
    }

    /// 中转页点「继续访问」：兑换票据、复检规则、计数、给出目标地址。
    ///
    /// 返回 `Ok(None)` 表示票据无效（过期、已用过、不是给这个短码签的），
    /// 调用方应当回到完整的鉴权流程 —— 重新出密码页或中转页。
    ///
    /// 密码不再校验：票据只发给已经通过鉴权的请求。过期与次数上限仍要复检，
    /// 因为访问者在确认页停留期间，链接可能刚好失效或名额被别人用完。
    pub async fn confirm(&self, code: &str, ticket: &str) -> Result<Option<String>, AppError> {
        if self.redeem_ticket(ticket).await.as_deref() != Some(code) {
            return Ok(None);
        }

        let resolved = self.load(code).await?;
        resolved.policy.check(&AccessRequest {
            now: Utc::now().naive_utc(),
            password: None,
            pending_visits: self.pending_visits(code, &resolved.policy).await,
            authenticated: true,
        })?;

        self.record_visit(code).await;
        Ok(Some(resolved.long_url))
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

    /// 只有设了次数上限才需要读增量计数，避免缓存命中时多一次 Redis 往返。
    async fn pending_visits(&self, code: &str, policy: &AccessPolicy) -> u32 {
        if !policy.requires_visit_count() {
            return 0;
        }

        match self.counter.pending_of(code).await {
            Ok(count) => count,
            Err(err) => {
                warn!("failed to read pending visits for `{code}`: {err}");
                0
            }
        }
    }

    /// 发一张「继续访问」票据。拿不到就只能让页面直连目标，跳转不受影响。
    async fn issue_ticket(&self, code: &str) -> Option<String> {
        match self.tickets.issue(code, CONFIRM_TICKET_TTL).await {
            Ok(ticket) => Some(ticket),
            Err(err) => {
                warn!("failed to issue a confirm ticket for `{code}`, the page will link straight to the target: {err}");
                None
            }
        }
    }

    /// 兑换票据。仓库故障时当作「票据无效」，回落到完整鉴权流程。
    async fn redeem_ticket(&self, ticket: &str) -> Option<String> {
        match self.tickets.redeem(ticket).await {
            Ok(code) => code,
            Err(err) => {
                warn!("failed to redeem a confirm ticket, falling back to the full check: {err}");
                None
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
        BrokenCache, BrokenCounter, BrokenGateTicket, InMemoryGateTicket, InMemoryLinkRepository,
        NoOpCache, NoOpCounter, RecordingCache, RecordingCounter,
    };
    use chrono::NaiveDate;

    fn service_with(repository: Arc<InMemoryLinkRepository>) -> RedirectService {
        RedirectService::new(
            repository,
            Arc::new(NoOpCache),
            Arc::new(NoOpCounter),
            Arc::new(InMemoryGateTicket::new()),
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
        service_with_tickets(
            repository,
            cache,
            counter,
            Arc::new(InMemoryGateTicket::new()),
        )
    }

    fn service_with_tickets(
        repository: Arc<InMemoryLinkRepository>,
        cache: Arc<dyn LinkCache>,
        counter: Arc<dyn VisitCounter>,
        tickets: Arc<dyn GateTicket>,
    ) -> RedirectService {
        RedirectService::new(repository, cache, counter, tickets, Duration::from_secs(60))
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

    /// 断言是中转，并取出目标地址与「继续访问」票据。
    fn interstitial(landing: Landing) -> (String, Option<String>) {
        match landing {
            Landing::Interstitial { target, ticket } => (target, ticket),
            other => panic!("expected an interstitial landing, got {other:?}"),
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
        let tickets = Arc::new(InMemoryGateTicket::new());
        let service = service_with_tickets(
            repository,
            Arc::new(NoOpCache),
            counter.clone(),
            tickets.clone(),
        );

        let (target, ticket) = interstitial(service.resolve("gate", None).await.unwrap());
        assert_eq!(target, "https://example.com");
        let ticket = ticket.expect("中转页要带上一张「继续访问」的票据");
        assert_eq!(tickets.outstanding(), 1);

        // 只打开确认页不算一次访问
        assert_eq!(counter.increments_of("gate"), 0, "中转页不该消耗访问次数");

        // 点了「继续访问」才计，且只计一次
        assert_eq!(
            service.confirm("gate", &ticket).await.unwrap().as_deref(),
            Some("https://example.com")
        );
        assert_eq!(counter.increments_of("gate"), 1);
        assert_eq!(tickets.outstanding(), 0, "票据兑换后即失效");
    }

    #[tokio::test]
    async fn tickets_are_single_use() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&interstitial_link(), "gate")
            .await
            .unwrap();

        let counter = Arc::new(RecordingCounter::new());
        let service = service_with_deps(repository, Arc::new(NoOpCache), counter.clone());

        let (_, ticket) = interstitial(service.resolve("gate", None).await.unwrap());
        let ticket = ticket.unwrap();

        assert!(service.confirm("gate", &ticket).await.unwrap().is_some());
        assert!(
            service.confirm("gate", &ticket).await.unwrap().is_none(),
            "同一张票据不能重复兑换"
        );
        assert_eq!(counter.increments_of("gate"), 1);
    }

    #[tokio::test]
    async fn tickets_are_bound_to_their_code() {
        // 拿 A 的票据去跳 B，等于绕过 B 的鉴权
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&interstitial_link(), "gate")
            .await
            .unwrap();
        repository
            .insert(&interstitial_link(), "other")
            .await
            .unwrap();

        let counter = Arc::new(RecordingCounter::new());
        let service = service_with_deps(repository, Arc::new(NoOpCache), counter.clone());

        let (_, ticket) = interstitial(service.resolve("gate", None).await.unwrap());

        assert!(service
            .confirm("other", &ticket.unwrap())
            .await
            .unwrap()
            .is_none());
        assert_eq!(counter.increments_of("other"), 0);
    }

    #[tokio::test]
    async fn confirm_skips_the_password_but_rechecks_the_rules() {
        let repository = Arc::new(InMemoryLinkRepository::new());
        let mut guarded = interstitial_link();
        guarded.password = Some("secret".into());
        guarded.max_visits = Some(2);
        repository.insert(&guarded, "gate").await.unwrap();

        let counter = Arc::new(RecordingCounter::new());
        let service = service_with_deps(repository, Arc::new(NoOpCache), counter.clone());

        // 输对密码才出中转页
        let (_, ticket) = interstitial(
            service
                .resolve("gate", Some("secret"))
                .await
                .expect("correct password should pass"),
        );

        // 点「继续访问」时不再要密码：票据本身就是「已验过」的证明
        assert!(service
            .confirm("gate", &ticket.unwrap())
            .await
            .unwrap()
            .is_some());
        assert_eq!(counter.increments_of("gate"), 1);

        // 但次数上限照旧：中转页停留期间名额可能被用完了
        let exhausted = Arc::new(InMemoryLinkRepository::new());
        let mut capped = interstitial_link();
        capped.max_visits = Some(1);
        exhausted.insert(&capped, "cap").await.unwrap();

        let service = service_with_tickets(
            exhausted,
            Arc::new(NoOpCache),
            Arc::new(RecordingCounter::with_pending("cap", 1)),
            Arc::new(InMemoryGateTicket::new()),
        );
        assert!(matches!(
            service.resolve("cap", None).await,
            Err(AppError::AccessDenied(AccessDenial::VisitLimitReached))
        ));
    }

    #[tokio::test]
    async fn falls_back_to_a_direct_link_when_tickets_fail() {
        // 票据仓库（Redis）挂了：中转页照样出，只是「继续访问」只能直连目标，
        // 这一跳统计不到 —— 与「计数本身就在 Redis 里」的降级语义一致
        let repository = Arc::new(InMemoryLinkRepository::new());
        repository
            .insert(&interstitial_link(), "gate")
            .await
            .unwrap();

        let service = service_with_tickets(
            repository,
            Arc::new(NoOpCache),
            Arc::new(NoOpCounter),
            Arc::new(BrokenGateTicket),
        );

        let (target, ticket) = interstitial(service.resolve("gate", None).await.unwrap());
        assert_eq!(target, "https://example.com");
        assert!(ticket.is_none(), "发不出票据时页面应退回直连");
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

        let (target, _) = interstitial(service.resolve("gate", None).await.unwrap());
        assert_eq!(target, "https://cached.example");
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
