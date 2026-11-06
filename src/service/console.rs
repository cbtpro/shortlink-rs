//! 控制台用例：短链列表 / 概览 / 删除，以及管理员对账号的管理。
//!
//! 这里是「谁能看到什么」的唯一落点：
//! - 普通用户：所有查询都被强制加上 `owner = 自己`，删除走「归属匹配」的 SQL；
//! - 管理员：看全站，可以按归属筛选，也可以重置密码、启用禁用。
//!
//! 规则写在用例层而不是各个 handler 里 —— handler 只负责把当前用户传进来，
//! 少写一个 `if` 就是越权，而这种错不会在编译期被发现。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use log::warn;

use crate::cache::{LinkCache, VisitCounter};
use crate::domain::account::{AccountStatus, User};
use crate::domain::short_link::ShortLink;
use crate::error::AppError;
use crate::repository::{
    LinkOverview, LinkQuery, LinkRepository, OverviewWindow, SessionRepository, UserPage,
    UserQuery, UserRepository,
};
use crate::security::password::random_password;
use crate::security::PasswordHasher;
use crate::service::run_blocking;

/// 单页上限。后台表格是人工操作的地方，没人需要一次拉全表。
const MAX_PAGE_SIZE: u32 = 100;

/// 搜索词长度上限。
///
/// 短码最多 10 位、地址最多 2048，搜索词再长也没有意义；
/// 截断而不是报错 —— 搜索框里多贴了一串东西，给结果比给 400 有用。
const MAX_KEYWORD_LEN: usize = 100;

/// 一次性口令长度。16 位、60 个符号，约 94 bit，够撑到用户改密那一刻。
const TEMP_PASSWORD_LEN: usize = 16;

/// 列表里的一行：短链本身 + 归属（管理员看全站时才需要）。
pub struct ConsoleLink {
    pub link: ShortLink,
    pub owner: Option<Owner>,
}

pub struct Owner {
    pub id: u64,
    pub email: String,
    pub display_name: String,
}

pub struct ConsolePage {
    pub items: Vec<ConsoleLink>,
    pub total: u64,
}

/// 重置密码后回给管理员的一次性凭据。只在响应里出现一次，不落库。
pub struct TempCredential {
    pub email: String,
    pub password: String,
}

pub struct ConsoleService {
    links: Arc<dyn LinkRepository>,
    users: Arc<dyn UserRepository>,
    sessions: Arc<dyn SessionRepository>,
    cache: Arc<dyn LinkCache>,
    counter: Arc<dyn VisitCounter>,
    hasher: Arc<dyn PasswordHasher>,
}

impl ConsoleService {
    pub fn new(
        links: Arc<dyn LinkRepository>,
        users: Arc<dyn UserRepository>,
        sessions: Arc<dyn SessionRepository>,
        cache: Arc<dyn LinkCache>,
        counter: Arc<dyn VisitCounter>,
        hasher: Arc<dyn PasswordHasher>,
    ) -> Self {
        Self {
            links,
            users,
            sessions,
            cache,
            counter,
            hasher,
        }
    }

    pub async fn overview(
        &self,
        actor: &User,
        window: OverviewWindow,
    ) -> Result<LinkOverview, AppError> {
        Ok(self.links.overview(window, scope_of(actor)).await?)
    }

    /// 当前页的短链。普通用户只会拿到自己的。
    pub async fn list_links(
        &self,
        actor: &User,
        query: LinkQuery,
        now: DateTime<Utc>,
    ) -> Result<ConsolePage, AppError> {
        let query = clamp_link_query(query, actor);
        let page = self.links.list(&query, now.naive_utc()).await?;

        let mut items = self.annotate(actor, page.items).await?;
        self.merge_pending_visits(&mut items).await;

        Ok(ConsolePage {
            items,
            total: page.total,
        })
    }

    /// 删除一条短链。普通用户只能删自己名下的，越权与不存在都是 404。
    pub async fn delete_link(&self, actor: &User, code: &str) -> Result<(), AppError> {
        let removed = if actor.is_admin() {
            self.links.delete_by_code(code).await?
        } else {
            self.links.delete_owned(actor.id, code).await?
        };

        if !removed {
            return Err(AppError::LinkNotFound);
        }

        // 先删库再清缓存：反过来的话，清缓存到删库之间进来的请求
        // 会把记录重新写回缓存，于是已删除的链接还能再跳一段时间。
        if let Err(err) = self.cache.evict(code).await {
            warn!("failed to evict cache for `{code}` after delete: {err}");
        }

        Ok(())
    }

    pub async fn list_users(&self, actor: &User, query: UserQuery) -> Result<UserPage, AppError> {
        require_admin(actor)?;
        Ok(self.users.list_users(&clamp_user_query(query)).await?)
    }

    /// 重置普通用户的密码。新口令随机生成、只返回一次，并要求对方下次登录改掉。
    pub async fn reset_password(
        &self,
        actor: &User,
        user_id: u64,
    ) -> Result<TempCredential, AppError> {
        require_admin(actor)?;

        let target = self
            .users
            .find_by_id(user_id)
            .await?
            .ok_or(AppError::UserNotFound)?;
        guard_target(actor, &target)?;

        let password = random_password(TEMP_PASSWORD_LEN);
        let hash = {
            // 散列是 CPU 密集的，别占住 worker 线程
            let hasher = self.hasher.clone();
            let password = password.clone();
            run_blocking(move || hasher.hash(&password)).await?
        };

        if !self.users.set_password(target.id, &hash, true).await? {
            return Err(AppError::UserNotFound);
        }

        // 口令已变，旧会话必须立刻作废，否则「重置」只是多了一个能用的密码
        self.sessions.delete_for_user(target.id).await?;

        Ok(TempCredential {
            email: target.email,
            password,
        })
    }

    /// 启用 / 禁用。禁用会同时踢掉该用户的全部会话。
    pub async fn set_status(
        &self,
        actor: &User,
        user_id: u64,
        status: AccountStatus,
    ) -> Result<(), AppError> {
        require_admin(actor)?;

        // 管理员把自己停用就再也进不来了，除非改库 —— 直接不允许
        if user_id == actor.id {
            return Err(AppError::Forbidden(
                "cannot change the status of your own account",
            ));
        }

        let target = self
            .users
            .find_by_id(user_id)
            .await?
            .ok_or(AppError::UserNotFound)?;
        guard_target(actor, &target)?;

        if !self.users.set_status(target.id, status).await? {
            return Err(AppError::UserNotFound);
        }

        if !status.is_active() {
            self.sessions.delete_for_user(target.id).await?;
        }

        Ok(())
    }

    /// 给列表补上归属。普通用户的列表全是自己的，不必多查一次。
    async fn annotate(
        &self,
        actor: &User,
        links: Vec<ShortLink>,
    ) -> Result<Vec<ConsoleLink>, AppError> {
        if !actor.is_admin() {
            return Ok(links
                .into_iter()
                .map(|link| ConsoleLink { link, owner: None })
                .collect());
        }

        let mut ids: Vec<u64> = links.iter().filter_map(|link| link.user_id).collect();
        ids.sort_unstable();
        ids.dedup();

        let owners: HashMap<u64, Owner> = if ids.is_empty() {
            HashMap::new()
        } else {
            self.users
                .find_by_ids(&ids)
                .await?
                .into_iter()
                .map(|user| {
                    (
                        user.id,
                        Owner {
                            id: user.id,
                            email: user.email,
                            display_name: user.display_name,
                        },
                    )
                })
                .collect()
        };

        Ok(links
            .into_iter()
            .map(|link| {
                let owner = link
                    .user_id
                    .and_then(|id| owners.get(&id))
                    .map(|owner| Owner {
                        id: owner.id,
                        email: owner.email.clone(),
                        display_name: owner.display_name.clone(),
                    });
                ConsoleLink { link, owner }
            })
            .collect())
    }

    /// 把「库里已回刷的」与「Redis 里尚未回刷的」合成真实访问量。
    ///
    /// 列表是给人看的：刚点完链接就回后台，看到的必须是刚加的那一次，
    /// 而不是等到下一次回刷（默认一分钟）才变。读 cache 失败就退回库里的值 ——
    /// 计数是附属能力，不该让后台打不开。
    ///
    /// 顺序是先读库、后读增量：反过来（先读增量再读库）如果中间正好跑了一次回刷，
    /// 同一段增量会被两边各算一次，宁可短暂少算也不能多算。
    async fn merge_pending_visits(&self, items: &mut [ConsoleLink]) {
        if items.is_empty() {
            return;
        }

        let codes: Vec<String> = items.iter().map(|item| item.link.code.clone()).collect();
        match self.counter.pending_of_many(&codes).await {
            Ok(pending) => {
                for item in items.iter_mut() {
                    if let Some(extra) = pending.get(&item.link.code) {
                        item.link.visit_count = item.link.visit_count.saturating_add(*extra);
                    }
                }
            }
            Err(err) => warn!("failed to read pending visits for console list: {err}"),
        }
    }
}

/// 普通用户的数据边界：永远只有自己；管理员默认不限。
fn scope_of(actor: &User) -> Option<u64> {
    if actor.is_admin() {
        None
    } else {
        Some(actor.id)
    }
}

fn require_admin(actor: &User) -> Result<(), AppError> {
    if actor.is_admin() {
        Ok(())
    } else {
        Err(AppError::AdminRequired)
    }
}

/// 管理员不能动的账号：另一个管理员。
///
/// 没有「提升为管理员」的入口，所以管理员账号一旦被停用或被改密，
/// 就只能改数据库救回来了。与其留个坑，不如让这类操作走「本人改密」。
fn guard_target(actor: &User, target: &User) -> Result<(), AppError> {
    if target.is_admin() {
        return Err(AppError::Forbidden(
            "administrator accounts can only be managed by themselves",
        ));
    }

    if target.id == actor.id {
        return Err(AppError::Forbidden(
            "cannot manage your own account from the console",
        ));
    }

    Ok(())
}

/// 收敛分页与搜索词，并强行套上归属边界。
///
/// 放在用例层而不是路由层：换协议（gRPC / CLI）时同样受保护。
fn clamp_link_query(mut query: LinkQuery, actor: &User) -> LinkQuery {
    query.page = query.page.max(1);
    query.page_size = query.page_size.clamp(1, MAX_PAGE_SIZE);
    query.keyword = query
        .keyword
        .map(|keyword| keyword.trim().chars().take(MAX_KEYWORD_LEN).collect())
        .filter(|keyword: &String| !keyword.is_empty());
    // 管理员可以按归属筛；普通用户的 owner 参数一律被覆盖
    query.owner = scope_of(actor).or(query.owner);
    query
}

fn clamp_user_query(mut query: UserQuery) -> UserQuery {
    query.page = query.page.max(1);
    query.page_size = query.page_size.clamp(1, MAX_PAGE_SIZE);
    query.keyword = query
        .keyword
        .map(|keyword| keyword.trim().chars().take(MAX_KEYWORD_LEN).collect())
        .filter(|keyword: &String| !keyword.is_empty());
    query
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate, NaiveDateTime};

    use crate::domain::account::{Access, NewUser};
    use crate::domain::new_link::NewShortLink;
    use crate::domain::short_link::CachedLink;
    use crate::repository::{LinkFilter, RepoError};
    use crate::test_support::{
        InMemoryLinkRepository, NoOpCounter, PlainHasher, RecordingCache, RecordingCounter,
    };

    struct Harness {
        service: ConsoleService,
        store: Arc<InMemoryLinkRepository>,
        cache: Arc<RecordingCache>,
        admin: User,
        alice: User,
        bob: User,
    }

    async fn harness_with_counter(counter: Arc<dyn VisitCounter>) -> Harness {
        let store = Arc::new(InMemoryLinkRepository::new());
        let cache = Arc::new(RecordingCache::new());

        let admin = create_user(
            &store,
            "admin@example.com",
            Access::ADMIN_WITH_TEMP_PASSWORD,
        )
        .await;
        let alice = create_user(&store, "alice@example.com", Access::USER).await;
        let bob = create_user(&store, "bob@example.com", Access::USER).await;

        let service = ConsoleService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            cache.clone(),
            counter,
            Arc::new(PlainHasher),
        );

        Harness {
            service,
            store,
            cache,
            admin,
            alice,
            bob,
        }
    }

    async fn harness() -> Harness {
        harness_with_counter(Arc::new(NoOpCounter)).await
    }

    async fn create_user(store: &InMemoryLinkRepository, email: &str, access: Access) -> User {
        <InMemoryLinkRepository as UserRepository>::create(
            store,
            &NewUser {
                email: email.to_string(),
                password: "secret123".into(),
                display_name: None,
            },
            "plain$secret123",
            access,
        )
        .await
        .expect("user should be created")
    }

    async fn create_link(store: &InMemoryLinkRepository, code: &str, owner: Option<u64>) {
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
            .expect("link should be inserted");
    }

    fn query() -> LinkQuery {
        LinkQuery {
            page: 1,
            page_size: 10,
            keyword: None,
            filter: LinkFilter::All,
            owner: None,
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
    }

    fn codes(page: ConsolePage) -> Vec<String> {
        page.items.into_iter().map(|item| item.link.code).collect()
    }

    #[tokio::test]
    async fn normal_users_only_see_their_own_links() {
        let h = harness().await;
        create_link(&h.store, "mine", Some(h.alice.id)).await;
        create_link(&h.store, "theirs", Some(h.bob.id)).await;
        create_link(&h.store, "anon", None).await;

        let page = h
            .service
            .list_links(&h.alice, query(), at(12))
            .await
            .unwrap();

        assert_eq!(codes(page), vec!["mine"]);
    }

    #[tokio::test]
    async fn owner_parameter_cannot_widen_a_normal_users_scope() {
        // 前端传 owner=<别人> 也一样，作用域由角色决定
        let h = harness().await;
        create_link(&h.store, "theirs", Some(h.bob.id)).await;

        let page = h
            .service
            .list_links(
                &h.alice,
                LinkQuery {
                    owner: Some(h.bob.id),
                    ..query()
                },
                at(12),
            )
            .await
            .unwrap();

        assert!(page.items.is_empty(), "不能靠参数越权看别人的链接");
    }

    #[tokio::test]
    async fn admins_see_everything_and_can_filter_by_owner() {
        let h = harness().await;
        create_link(&h.store, "a", Some(h.alice.id)).await;
        create_link(&h.store, "b", Some(h.bob.id)).await;
        create_link(&h.store, "anon", None).await;

        let all = h
            .service
            .list_links(&h.admin, query(), at(12))
            .await
            .unwrap();
        assert_eq!(all.total, 3);

        let only_bob = h
            .service
            .list_links(
                &h.admin,
                LinkQuery {
                    owner: Some(h.bob.id),
                    ..query()
                },
                at(12),
            )
            .await
            .unwrap();
        assert_eq!(codes(only_bob), vec!["b"]);
    }

    #[tokio::test]
    async fn admin_rows_carry_the_owner_email() {
        let h = harness().await;
        create_link(&h.store, "a", Some(h.alice.id)).await;
        create_link(&h.store, "anon", None).await;

        let page = h
            .service
            .list_links(&h.admin, query(), at(12))
            .await
            .unwrap();
        let owner_of = |code: &str| {
            page.items
                .iter()
                .find(|item| item.link.code == code)
                .and_then(|item| item.owner.as_ref())
                .map(|owner| owner.email.clone())
        };

        assert_eq!(owner_of("a").as_deref(), Some("alice@example.com"));
        assert_eq!(owner_of("anon"), None, "匿名链接没有归属");
    }

    #[tokio::test]
    async fn normal_users_cannot_delete_someone_elses_link() {
        let h = harness().await;
        create_link(&h.store, "theirs", Some(h.bob.id)).await;
        h.cache.preload(
            "theirs",
            CachedLink {
                id: 1,
                long_url: "https://example.com".into(),
                expire_at: None,
                max_visits: None,
                visit_count: 0,
                interstitial: false,
            },
        );

        assert!(matches!(
            h.service.delete_link(&h.alice, "theirs").await,
            Err(AppError::LinkNotFound)
        ));
        assert!(h.store.find_by_code("theirs").await.unwrap().is_some());
        assert!(h.cache.contains("theirs"), "删失败不该清缓存");
    }

    #[tokio::test]
    async fn admins_can_delete_any_link_and_cache_is_evicted() {
        let h = harness().await;
        create_link(&h.store, "any", Some(h.bob.id)).await;
        h.cache.preload(
            "any",
            CachedLink {
                id: 1,
                long_url: "https://example.com".into(),
                expire_at: None,
                max_visits: None,
                visit_count: 0,
                interstitial: false,
            },
        );

        h.service.delete_link(&h.admin, "any").await.unwrap();

        assert!(h.store.find_by_code("any").await.unwrap().is_none());
        assert!(!h.cache.contains("any"));
    }

    #[tokio::test]
    async fn overview_is_scoped_for_normal_users() {
        let h = harness().await;
        create_link(&h.store, "mine", Some(h.alice.id)).await;
        create_link(&h.store, "theirs", Some(h.bob.id)).await;

        let window = OverviewWindow {
            now: at(12).naive_utc(),
            today_start: NaiveDateTime::default(),
            expiring_before: (at(12) + Duration::days(7)).naive_utc(),
        };

        assert_eq!(
            h.service
                .overview(&h.alice, window)
                .await
                .unwrap()
                .total_links,
            1
        );
        assert_eq!(
            h.service
                .overview(&h.admin, window)
                .await
                .unwrap()
                .total_links,
            2
        );
    }

    #[tokio::test]
    async fn pending_visits_are_added_on_top_of_stored_counts() {
        let counter = Arc::new(RecordingCounter::with_pending("hot", 3));
        let h = harness_with_counter(counter).await;

        create_link(&h.store, "hot", Some(h.alice.id)).await;
        h.store.add_visit_count("hot", 2).await.unwrap();

        let page = h
            .service
            .list_links(&h.alice, query(), at(12))
            .await
            .unwrap();

        assert_eq!(page.items[0].link.visit_count, 5, "2 次已落库 + 3 次待回刷");
    }

    #[tokio::test]
    async fn normal_users_cannot_list_users() {
        let h = harness().await;
        let query = UserQuery {
            page: 1,
            page_size: 20,
            keyword: None,
        };

        assert!(matches!(
            h.service.list_users(&h.alice, query).await,
            Err(AppError::AdminRequired)
        ));
    }

    #[tokio::test]
    async fn admin_sees_link_counts_per_user() {
        let h = harness().await;
        create_link(&h.store, "a", Some(h.alice.id)).await;
        create_link(&h.store, "b", Some(h.alice.id)).await;

        let page = h
            .service
            .list_users(
                &h.admin,
                UserQuery {
                    page: 1,
                    page_size: 20,
                    keyword: None,
                },
            )
            .await
            .unwrap();

        let alice = page
            .items
            .iter()
            .find(|row| row.user.id == h.alice.id)
            .expect("alice should be listed");
        assert_eq!(alice.link_count, 2);
    }

    #[tokio::test]
    async fn admin_can_search_users_by_email() {
        let h = harness().await;

        let page = h
            .service
            .list_users(
                &h.admin,
                UserQuery {
                    page: 1,
                    page_size: 20,
                    keyword: Some("alice".into()),
                },
            )
            .await
            .unwrap();

        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].user.email, "alice@example.com");
    }

    #[tokio::test]
    async fn reset_password_returns_a_working_temporary_password() {
        let h = harness().await;
        // 先造一个老会话，验证重置后会被踢掉
        <InMemoryLinkRepository as SessionRepository>::create(
            h.store.as_ref(),
            h.alice.id,
            "fingerprint",
            at(12).naive_utc(),
        )
        .await
        .unwrap();

        let credential = h
            .service
            .reset_password(&h.admin, h.alice.id)
            .await
            .unwrap();

        assert_eq!(credential.email, "alice@example.com");
        assert_eq!(credential.password.chars().count(), TEMP_PASSWORD_LEN);
        assert!(
            h.store
                .find_user("fingerprint", at(12).naive_utc())
                .await
                .unwrap()
                .is_none(),
            "重置密码后旧会话必须失效"
        );

        let alice = h.store.find_by_id(h.alice.id).await.unwrap().unwrap();
        assert!(alice.must_change_password, "重置出来的口令必须先改");
        assert_eq!(
            alice.password_hash,
            format!("plain${}", credential.password)
        );
    }

    #[tokio::test]
    async fn admins_cannot_reset_another_admin() {
        let store = Arc::new(InMemoryLinkRepository::new());
        let first = create_user(&store, "one@example.com", Access::ADMIN_WITH_TEMP_PASSWORD).await;
        let second = create_user(&store, "two@example.com", Access::ADMIN_WITH_TEMP_PASSWORD).await;
        let service = ConsoleService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            Arc::new(RecordingCache::new()),
            Arc::new(NoOpCounter),
            Arc::new(PlainHasher),
        );

        assert!(matches!(
            service.reset_password(&first, second.id).await,
            Err(AppError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn normal_users_cannot_reset_passwords() {
        let h = harness().await;
        assert!(matches!(
            h.service.reset_password(&h.alice, h.bob.id).await,
            Err(AppError::AdminRequired)
        ));
    }

    #[tokio::test]
    async fn disabling_a_user_kills_their_sessions() {
        let h = harness().await;
        <InMemoryLinkRepository as SessionRepository>::create(
            h.store.as_ref(),
            h.bob.id,
            "fingerprint",
            at(12).naive_utc(),
        )
        .await
        .unwrap();

        h.service
            .set_status(&h.admin, h.bob.id, AccountStatus::Disabled)
            .await
            .unwrap();

        assert!(h
            .store
            .find_user("fingerprint", at(12).naive_utc())
            .await
            .unwrap()
            .is_none());
        assert!(!h
            .store
            .find_by_id(h.bob.id)
            .await
            .unwrap()
            .unwrap()
            .is_active());
    }

    #[tokio::test]
    async fn admins_cannot_disable_another_admin() {
        // 没有「提升为管理员」的入口，所以也不能通过停用把另一个管理员关掉：
        // 那样一来谁都可能把唯一的另一个管理员锁在门外。
        let store = Arc::new(InMemoryLinkRepository::new());
        let first = create_user(&store, "one@example.com", Access::ADMIN_WITH_TEMP_PASSWORD).await;
        let second = create_user(&store, "two@example.com", Access::ADMIN_WITH_TEMP_PASSWORD).await;
        let service = ConsoleService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            Arc::new(RecordingCache::new()),
            Arc::new(NoOpCounter),
            Arc::new(PlainHasher),
        );

        assert!(matches!(
            service
                .set_status(&first, second.id, AccountStatus::Disabled)
                .await,
            Err(AppError::Forbidden(_))
        ));
        assert!(
            store
                .find_by_id(second.id)
                .await
                .unwrap()
                .unwrap()
                .is_active(),
            "被拒绝的操作不能落到库上"
        );
    }

    #[tokio::test]
    async fn enabling_a_disabled_user_puts_them_back_to_work() {
        let h = harness().await;
        h.service
            .set_status(&h.admin, h.bob.id, AccountStatus::Disabled)
            .await
            .unwrap();
        h.service
            .set_status(&h.admin, h.bob.id, AccountStatus::Active)
            .await
            .unwrap();

        assert!(h
            .store
            .find_by_id(h.bob.id)
            .await
            .unwrap()
            .unwrap()
            .is_active());
    }

    #[tokio::test]
    async fn keyword_never_escapes_a_normal_users_scope() {
        // 关键词与归属是 AND：搜得再准也只能落在自己名下
        let h = harness().await;
        create_link(&h.store, "alice-promo", Some(h.alice.id)).await;
        create_link(&h.store, "bob-promo", Some(h.bob.id)).await;

        let page = h
            .service
            .list_links(
                &h.alice,
                LinkQuery {
                    keyword: Some("promo".into()),
                    ..query()
                },
                at(12),
            )
            .await
            .unwrap();

        assert_eq!(codes(page), vec!["alice-promo"]);
    }

    #[tokio::test]
    async fn admin_cannot_disable_themselves() {
        let h = harness().await;
        assert!(matches!(
            h.service
                .set_status(&h.admin, h.admin.id, AccountStatus::Disabled)
                .await,
            Err(AppError::Forbidden(_))
        ));
        assert!(h
            .store
            .find_by_id(h.admin.id)
            .await
            .unwrap()
            .unwrap()
            .is_active());
    }

    #[tokio::test]
    async fn missing_user_is_reported_as_not_found() {
        let h = harness().await;
        assert!(matches!(
            h.service.reset_password(&h.admin, 999).await,
            Err(AppError::UserNotFound)
        ));
        assert!(matches!(
            h.service
                .set_status(&h.admin, 999, AccountStatus::Disabled)
                .await,
            Err(AppError::UserNotFound)
        ));
    }

    #[tokio::test]
    async fn clamps_pagination_and_trims_keyword() {
        let h = harness().await;
        for code in ["a", "b", "c"] {
            create_link(&h.store, code, Some(h.alice.id)).await;
        }

        let page = h
            .service
            .list_links(
                &h.alice,
                LinkQuery {
                    page: 0,
                    page_size: 1000,
                    keyword: Some("   ".into()),
                    filter: LinkFilter::All,
                    owner: None,
                },
                at(12),
            )
            .await
            .unwrap();

        assert_eq!(page.total, 3, "空关键字不应过滤掉任何记录");
        assert_eq!(page.items.len(), 3);
    }

    #[tokio::test]
    async fn storage_failure_becomes_server_error() {
        use crate::test_support::BrokenRepository;

        let service = ConsoleService::new(
            Arc::new(BrokenRepository),
            Arc::new(BrokenRepository),
            Arc::new(BrokenRepository),
            Arc::new(RecordingCache::new()),
            Arc::new(NoOpCounter),
            Arc::new(PlainHasher),
        );
        let admin = User {
            id: 1,
            email: "a@example.com".into(),
            display_name: "a".into(),
            password_hash: String::new(),
            role: crate::domain::account::Role::Admin,
            status: AccountStatus::Active,
            must_change_password: false,
            created_at: NaiveDateTime::default(),
        };

        assert!(matches!(
            service.list_links(&admin, query(), at(12)).await,
            Err(AppError::Storage(_))
        ));
        assert!(matches!(
            service.delete_link(&admin, "a").await,
            Err(AppError::Storage(_))
        ));
        assert!(matches!(
            service
                .list_users(
                    &admin,
                    UserQuery {
                        page: 1,
                        page_size: 20,
                        keyword: None,
                    }
                )
                .await,
            Err(AppError::Storage(_))
        ));
    }

    #[test]
    fn user_query_is_clamped() {
        let clamped = clamp_user_query(UserQuery {
            page: 0,
            page_size: 1000,
            keyword: Some(format!("  {}  ", "a".repeat(MAX_KEYWORD_LEN + 20))),
        });

        assert_eq!(clamped.page, 1);
        assert_eq!(clamped.page_size, MAX_PAGE_SIZE);
        assert_eq!(
            clamped.keyword.as_deref().map(str::len),
            Some(MAX_KEYWORD_LEN)
        );
    }

    #[test]
    fn repo_error_maps_to_storage() {
        // 保证 BrokenRepository 真的在报存储错误，而不是碰巧走到别的分支
        let error: AppError = RepoError::Backend("boom".into()).into();
        assert!(matches!(error, AppError::Storage(_)));
    }
}
