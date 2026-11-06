//! 内存仓储，仅测试用。

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{NaiveDateTime, Utc};

use super::{
    CreationHistory, LinkFilter, LinkOverview, LinkPage, LinkQuery, LinkRepository, OverviewWindow,
    RepoError, SessionRepository, UserPage, UserQuery, UserRepository, UserSummary,
};
use crate::domain::account::{Access, AccountStatus, NewUser, User};
use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::{LinkState, ShortLink};
use crate::types::BoxFuture;

struct Creation {
    code: String,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    links: HashMap<String, ShortLink>,
    /// 访客 → 创建记录，按时间倒序保存
    creations: HashMap<String, Vec<Creation>>,
    next_user_id: u64,
    users: Vec<User>,
    /// 令牌指纹 → (用户 id, 过期时刻)
    sessions: HashMap<String, (u64, NaiveDateTime)>,
}

#[derive(Default)]
pub struct InMemoryLinkRepository {
    inner: Mutex<Inner>,
}

impl InMemoryLinkRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_inner<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut inner = self.inner.lock().expect("in-memory repo mutex poisoned");
        f(&mut inner)
    }
}

impl LinkRepository for InMemoryLinkRepository {
    fn insert<'a>(
        &'a self,
        link: &'a NewShortLink,
        code: &'a str,
    ) -> BoxFuture<'a, Result<ShortLink, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                if inner.links.contains_key(code) {
                    return Err(RepoError::DuplicateCode);
                }
                inner.next_id += 1;
                let stored = ShortLink {
                    id: inner.next_id,
                    code: code.to_string(),
                    user_id: link.owner_id,
                    long_url: link.url.clone(),
                    created_at: Utc::now().naive_utc(),
                    expire_at: link.expire_at,
                    max_visits: link.max_visits,
                    visit_count: 0,
                    password: link.password.clone(),
                    ip_limit: link.ip_limit.clone(),
                    ua_limit: link.ua_limit.clone(),
                    interstitial: link.interstitial,
                };
                inner.links.insert(code.to_string(), stored.clone());
                Ok(stored)
            })
        })
    }

    fn find_by_code<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<Option<ShortLink>, RepoError>> {
        Box::pin(async move { self.with_inner(|inner| Ok(inner.links.get(code).cloned())) })
    }

    fn add_visit_count<'a>(
        &'a self,
        code: &'a str,
        delta: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| match inner.links.get_mut(code) {
                Some(link) => {
                    link.visit_count = link.visit_count.saturating_add(delta);
                    Ok(())
                }
                None => Err(RepoError::NotFound),
            })
        })
    }

    fn list<'a>(
        &'a self,
        query: &'a LinkQuery,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<LinkPage, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let keyword = query.keyword.as_deref().unwrap_or("").to_lowercase();

                let mut matched: Vec<&ShortLink> = inner
                    .links
                    .values()
                    .filter(|link| query.owner.is_none_or(|owner| link.user_id == Some(owner)))
                    .filter(|link| {
                        // 与 SQL 的模糊匹配保持一致：短码或长链接命中即可
                        keyword.is_empty()
                            || link.code.to_lowercase().contains(&keyword)
                            || link.long_url.to_lowercase().contains(&keyword)
                    })
                    .filter(|link| match query.filter {
                        LinkFilter::All => true,
                        LinkFilter::Active => link.state(now) == LinkState::Active,
                        LinkFilter::Expired => link.state(now) == LinkState::Expired,
                        LinkFilter::Exhausted => link.state(now) == LinkState::Exhausted,
                    })
                    .collect();

                // 与 SQL 的 `ORDER BY created_at DESC, id DESC` 同序
                matched.sort_by(|a, b| {
                    b.created_at
                        .cmp(&a.created_at)
                        .then_with(|| b.id.cmp(&a.id))
                });

                let total = matched.len() as u64;
                let start = (query.page.saturating_sub(1) as usize) * query.page_size as usize;
                let items = matched
                    .into_iter()
                    .skip(start)
                    .take(query.page_size as usize)
                    .cloned()
                    .collect();

                Ok(LinkPage { items, total })
            })
        })
    }

    fn overview(
        &self,
        window: OverviewWindow,
        owner: Option<u64>,
    ) -> BoxFuture<'_, Result<LinkOverview, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let mut overview = LinkOverview::default();

                for link in inner.links.values() {
                    if owner.is_some_and(|owner| link.user_id != Some(owner)) {
                        continue;
                    }

                    overview.total_links += 1;
                    overview.total_visits += u64::from(link.visit_count);

                    if link.created_at >= window.today_start {
                        overview.created_today += 1;
                    }
                    if link.expire_at.is_some_and(|expire_at| {
                        expire_at > window.now && expire_at <= window.expiring_before
                    }) {
                        overview.expiring_soon += 1;
                    }
                }

                Ok(overview)
            })
        })
    }

    fn delete_by_code<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move { self.with_inner(|inner| Ok(inner.links.remove(code).is_some())) })
    }

    fn list_by_owner(
        &self,
        owner_id: u64,
        limit: u32,
    ) -> BoxFuture<'_, Result<Vec<ShortLink>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let mut owned: Vec<&ShortLink> = inner
                    .links
                    .values()
                    .filter(|link| link.user_id == Some(owner_id))
                    .collect();
                // 与 SQL 的 `ORDER BY created_at DESC, id DESC` 同序
                owned.sort_by(|a, b| {
                    b.created_at
                        .cmp(&a.created_at)
                        .then_with(|| b.id.cmp(&a.id))
                });

                Ok(owned
                    .into_iter()
                    .take(limit as usize)
                    .cloned()
                    .collect::<Vec<_>>())
            })
        })
    }

    fn delete_owned<'a>(
        &'a self,
        owner_id: u64,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                // 归属不符与不存在返回同一个 false：调用方对两者都只能报 404
                match inner.links.get(code) {
                    Some(link) if link.user_id == Some(owner_id) => {
                        inner.links.remove(code);
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            })
        })
    }

    fn delete_all_owned(&self, owner_id: u64) -> BoxFuture<'_, Result<Vec<String>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let codes: Vec<String> = inner
                    .links
                    .values()
                    .filter(|link| link.user_id == Some(owner_id))
                    .map(|link| link.code.clone())
                    .collect();

                inner.links.retain(|_, link| link.user_id != Some(owner_id));

                Ok(codes)
            })
        })
    }
}

impl CreationHistory for InMemoryLinkRepository {
    fn record<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
        keep: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let entries = inner.creations.entry(visitor.to_string()).or_default();
                entries.retain(|entry| entry.code != code);
                entries.insert(
                    0,
                    Creation {
                        code: code.to_string(),
                    },
                );
                entries.truncate(keep.max(1) as usize);
                Ok(())
            })
        })
    }

    fn recent<'a>(
        &'a self,
        visitor: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<ShortLink>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                // 短链已被删掉的记录自然查不到，与 SQL 的 join 行为一致
                let codes: Vec<String> = inner
                    .creations
                    .get(visitor)
                    .map(|entries| entries.iter().map(|entry| entry.code.clone()).collect())
                    .unwrap_or_default();

                Ok(codes
                    .iter()
                    .filter_map(|code| inner.links.get(code).cloned())
                    .take(limit as usize)
                    .collect())
            })
        })
    }

    fn forget<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let Some(entries) = inner.creations.get_mut(visitor) else {
                    return Ok(false);
                };
                let before = entries.len();
                entries.retain(|entry| entry.code != code);
                Ok(entries.len() != before)
            })
        })
    }

    fn clear<'a>(&'a self, visitor: &'a str) -> BoxFuture<'a, Result<u64, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                Ok(inner
                    .creations
                    .remove(visitor)
                    .map(|entries| entries.len() as u64)
                    .unwrap_or(0))
            })
        })
    }
}

impl UserRepository for InMemoryLinkRepository {
    fn create<'a>(
        &'a self,
        new: &'a NewUser,
        password_hash: &'a str,
        access: Access,
    ) -> BoxFuture<'a, Result<User, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                if inner.users.iter().any(|user| user.email == new.email) {
                    return Err(RepoError::DuplicateEmail);
                }

                inner.next_user_id += 1;
                let user = User {
                    id: inner.next_user_id,
                    email: new.email.clone(),
                    display_name: new.resolve_display_name(),
                    password_hash: password_hash.to_string(),
                    role: access.role,
                    status: AccountStatus::Active,
                    must_change_password: access.must_change_password,
                    created_at: Utc::now().naive_utc(),
                };
                inner.users.push(user.clone());
                Ok(user)
            })
        })
    }

    fn find_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                Ok(inner.users.iter().find(|user| user.email == email).cloned())
            })
        })
    }

    fn find_by_id(&self, id: u64) -> BoxFuture<'_, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| Ok(inner.users.iter().find(|user| user.id == id).cloned()))
        })
    }

    fn find_by_ids<'a>(&'a self, ids: &'a [u64]) -> BoxFuture<'a, Result<Vec<User>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                Ok(inner
                    .users
                    .iter()
                    .filter(|user| ids.contains(&user.id))
                    .cloned()
                    .collect())
            })
        })
    }

    fn find_first_admin(&self) -> BoxFuture<'_, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                Ok(inner
                    .users
                    .iter()
                    .find(|user| user.role.is_admin())
                    .cloned())
            })
        })
    }

    fn list_users<'a>(
        &'a self,
        query: &'a UserQuery,
    ) -> BoxFuture<'a, Result<UserPage, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let keyword = query.keyword.as_deref().unwrap_or("").to_lowercase();

                let mut matched: Vec<&User> = inner
                    .users
                    .iter()
                    .filter(|user| {
                        keyword.is_empty()
                            || user.email.to_lowercase().contains(&keyword)
                            || user.display_name.to_lowercase().contains(&keyword)
                    })
                    .collect();
                // 与 SQL 的 `ORDER BY id DESC` 同序：新注册的排最前
                matched.sort_by_key(|link| std::cmp::Reverse(link.id));

                let total = matched.len() as u64;
                let start = (query.page.saturating_sub(1) as usize) * query.page_size as usize;
                let items = matched
                    .into_iter()
                    .skip(start)
                    .take(query.page_size as usize)
                    .map(|user| UserSummary {
                        link_count: inner
                            .links
                            .values()
                            .filter(|link| link.user_id == Some(user.id))
                            .count() as u64,
                        user: user.clone(),
                    })
                    .collect();

                Ok(UserPage { items, total })
            })
        })
    }

    fn set_password<'a>(
        &'a self,
        id: u64,
        password_hash: &'a str,
        must_change_password: bool,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async move {
            self.with_inner(
                |inner| match inner.users.iter_mut().find(|user| user.id == id) {
                    Some(user) => {
                        user.password_hash = password_hash.to_string();
                        user.must_change_password = must_change_password;
                        Ok(true)
                    }
                    None => Ok(false),
                },
            )
        })
    }

    fn set_status(&self, id: u64, status: AccountStatus) -> BoxFuture<'_, Result<bool, RepoError>> {
        Box::pin(async move {
            self.with_inner(
                |inner| match inner.users.iter_mut().find(|user| user.id == id) {
                    Some(user) => {
                        user.status = status;
                        Ok(true)
                    }
                    None => Ok(false),
                },
            )
        })
    }
}

impl SessionRepository for InMemoryLinkRepository {
    fn create<'a>(
        &'a self,
        user_id: u64,
        token_hash: &'a str,
        expires_at: NaiveDateTime,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                inner
                    .sessions
                    .insert(token_hash.to_string(), (user_id, expires_at));
                Ok(())
            })
        })
    }

    fn find_user<'a>(
        &'a self,
        token_hash: &'a str,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let Some((user_id, expires_at)) = inner.sessions.get(token_hash) else {
                    return Ok(None);
                };
                if *expires_at <= now {
                    return Ok(None);
                }

                // 与 SQL 一致：被禁用的账号即使会话还没过期也查不出用户
                Ok(inner
                    .users
                    .iter()
                    .find(|user| user.id == *user_id && user.is_active())
                    .cloned())
            })
        })
    }

    fn delete<'a>(&'a self, token_hash: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(
            async move { self.with_inner(|inner| Ok(inner.sessions.remove(token_hash).is_some())) },
        )
    }

    fn delete_for_user(&self, user_id: u64) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let before = inner.sessions.len();
                inner.sessions.retain(|_, (owner, _)| *owner != user_id);
                Ok((before - inner.sessions.len()) as u64)
            })
        })
    }

    fn purge_expired(&self, now: NaiveDateTime) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let before = inner.sessions.len();
                inner
                    .sessions
                    .retain(|_, (_, expires_at)| *expires_at > now);
                Ok((before - inner.sessions.len()) as u64)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str) -> NewShortLink {
        NewShortLink {
            url: url.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn assigns_increasing_ids() {
        let repository = InMemoryLinkRepository::new();
        let first = repository
            .insert(&link("https://a.example"), "a")
            .await
            .unwrap();
        let second = repository
            .insert(&link("https://b.example"), "b")
            .await
            .unwrap();

        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
        assert_eq!(first.visit_count, 0);
    }

    #[tokio::test]
    async fn rejects_duplicate_code() {
        let repository = InMemoryLinkRepository::new();
        repository
            .insert(&link("https://a.example"), "dup")
            .await
            .unwrap();

        let result = repository.insert(&link("https://b.example"), "dup").await;
        assert!(matches!(result, Err(RepoError::DuplicateCode)));
    }

    #[tokio::test]
    async fn returns_none_for_unknown_code() {
        let repository = InMemoryLinkRepository::new();
        assert!(repository.find_by_code("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn accumulates_visit_counts() {
        let repository = InMemoryLinkRepository::new();
        repository
            .insert(&link("https://a.example"), "a")
            .await
            .unwrap();

        repository.add_visit_count("a", 2).await.unwrap();
        repository.add_visit_count("a", 3).await.unwrap();

        let stored = repository.find_by_code("a").await.unwrap().unwrap();
        assert_eq!(stored.visit_count, 5);
    }

    #[tokio::test]
    async fn reports_missing_link_when_adding_visits() {
        let repository = InMemoryLinkRepository::new();
        assert!(matches!(
            repository.add_visit_count("ghost", 1).await,
            Err(RepoError::NotFound)
        ));
    }

    #[tokio::test]
    async fn saturates_instead_of_overflowing() {
        let repository = InMemoryLinkRepository::new();
        repository
            .insert(&link("https://a.example"), "a")
            .await
            .unwrap();

        repository.add_visit_count("a", u32::MAX).await.unwrap();
        repository.add_visit_count("a", 1).await.unwrap();

        let stored = repository.find_by_code("a").await.unwrap().unwrap();
        assert_eq!(stored.visit_count, u32::MAX);
    }

    fn query(filter: LinkFilter) -> LinkQuery {
        LinkQuery {
            page: 1,
            page_size: 10,
            keyword: None,
            filter,
            owner: None,
        }
    }

    fn now() -> NaiveDateTime {
        Utc::now().naive_utc()
    }

    #[tokio::test]
    async fn lists_newest_first_with_pagination() {
        let repository = InMemoryLinkRepository::new();
        for code in ["a", "b", "c"] {
            repository
                .insert(&link("https://example.com"), code)
                .await
                .unwrap();
        }

        let first = repository
            .list(
                &LinkQuery {
                    page: 1,
                    page_size: 2,
                    keyword: None,
                    filter: LinkFilter::All,
                    owner: None,
                },
                now(),
            )
            .await
            .unwrap();

        assert_eq!(first.total, 3, "total 不受分页影响");
        assert_eq!(first.items.len(), 2);
        // 同 created_at 时按 id 倒序，最后写入的排最前
        assert_eq!(first.items[0].code, "c");
        assert_eq!(first.items[1].code, "b");

        let second = repository
            .list(
                &LinkQuery {
                    page: 2,
                    page_size: 2,
                    keyword: None,
                    filter: LinkFilter::All,
                    owner: None,
                },
                now(),
            )
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].code, "a");
    }

    #[tokio::test]
    async fn filters_by_state() {
        let repository = InMemoryLinkRepository::new();
        let now = now();

        repository
            .insert(&link("https://active.example"), "active")
            .await
            .unwrap();

        repository
            .insert(
                &NewShortLink {
                    expire_at: Some(now - chrono::Duration::hours(1)),
                    ..link("https://expired.example")
                },
                "expired",
            )
            .await
            .unwrap();

        repository
            .insert(
                &NewShortLink {
                    max_visits: Some(1),
                    ..link("https://capped.example")
                },
                "capped",
            )
            .await
            .unwrap();
        repository.add_visit_count("capped", 1).await.unwrap();

        let codes = |page: crate::repository::LinkPage| {
            page.items
                .into_iter()
                .map(|item| item.code)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            codes(repository.list(&query(LinkFilter::All), now).await.unwrap()).len(),
            3
        );
        assert_eq!(
            codes(
                repository
                    .list(&query(LinkFilter::Active), now)
                    .await
                    .unwrap()
            ),
            vec!["active"]
        );
        assert_eq!(
            codes(
                repository
                    .list(&query(LinkFilter::Expired), now)
                    .await
                    .unwrap()
            ),
            vec!["expired"]
        );
        assert_eq!(
            codes(
                repository
                    .list(&query(LinkFilter::Exhausted), now)
                    .await
                    .unwrap()
            ),
            vec!["capped"]
        );
    }

    #[tokio::test]
    async fn searches_code_and_url_ignoring_case() {
        let repository = InMemoryLinkRepository::new();
        repository
            .insert(&link("https://example.com/Docs"), "Promo")
            .await
            .unwrap();
        repository
            .insert(&link("https://other.example"), "blog")
            .await
            .unwrap();

        let by_code = repository
            .list(
                &LinkQuery {
                    keyword: Some("promo".into()),
                    ..query(LinkFilter::All)
                },
                now(),
            )
            .await
            .unwrap();
        assert_eq!(by_code.total, 1);

        let by_url = repository
            .list(
                &LinkQuery {
                    keyword: Some("docs".into()),
                    ..query(LinkFilter::All)
                },
                now(),
            )
            .await
            .unwrap();
        assert_eq!(by_url.total, 1);
        assert_eq!(by_url.items[0].code, "Promo");
    }

    #[tokio::test]
    async fn overview_counts_links_visits_and_expiring() {
        let repository = InMemoryLinkRepository::new();
        let now = now();

        repository
            .insert(&link("https://a.example"), "a")
            .await
            .unwrap();
        repository
            .insert(&link("https://b.example"), "b")
            .await
            .unwrap();
        repository.add_visit_count("a", 4).await.unwrap();
        repository
            .insert(
                &NewShortLink {
                    expire_at: Some(now + chrono::Duration::days(1)),
                    ..link("https://soon.example")
                },
                "soon",
            )
            .await
            .unwrap();

        let overview = repository
            .overview(
                OverviewWindow {
                    now,
                    today_start: now - chrono::Duration::hours(1),
                    expiring_before: now + chrono::Duration::days(7),
                },
                None,
            )
            .await
            .unwrap();

        assert_eq!(overview.total_links, 3);
        assert_eq!(overview.total_visits, 4);
        assert_eq!(overview.created_today, 3, "刚才写入的三条都落在窗口内");
        assert_eq!(overview.expiring_soon, 1);
    }

    #[tokio::test]
    async fn deletes_existing_and_reports_missing() {
        let repository = InMemoryLinkRepository::new();
        repository
            .insert(&link("https://a.example"), "gone")
            .await
            .unwrap();

        assert!(repository.delete_by_code("gone").await.unwrap());
        assert!(repository.find_by_code("gone").await.unwrap().is_none());
        assert!(
            !repository.delete_by_code("gone").await.unwrap(),
            "重复删除返回 false"
        );
    }

    /* ---------- 用户与会话 ----------
     * 这个实现是所有服务层用例的地基：它错了，上层测出来的「通过」都是假的。
     * 所以这里单独盯住「作用域」和「降级」两件事，而不是只跑一遍顺利路径。
     */

    async fn add_user(
        repository: &InMemoryLinkRepository,
        email: &str,
        display_name: &str,
        access: Access,
    ) -> User {
        <InMemoryLinkRepository as UserRepository>::create(
            repository,
            &NewUser {
                email: email.into(),
                password: "password123".into(),
                display_name: Some(display_name.into()),
            },
            "plain$password123",
            access,
        )
        .await
        .expect("user should be created")
    }

    async fn add_link(repository: &InMemoryLinkRepository, code: &str, owner: Option<u64>) {
        let new = NewShortLink {
            owner_id: owner,
            ..link(&format!("https://{code}.example"))
        };
        repository.insert(&new, code).await.unwrap();
    }

    #[tokio::test]
    async fn user_ids_increase_and_emails_are_unique() {
        let repository = InMemoryLinkRepository::new();
        let first = add_user(&repository, "a@example.com", "甲", Access::USER).await;
        let second = add_user(&repository, "b@example.com", "乙", Access::USER).await;

        assert_eq!(first.id, 1);
        assert_eq!(second.id, 2);
        // 自助注册出来的账号必须是「启用 + 普通用户」，否则注册完就进不去
        assert!(first.is_active());
        assert!(!first.is_admin());
        assert!(!first.must_change_password);
        assert_eq!(first.display_name, "甲");

        let duplicate = <InMemoryLinkRepository as UserRepository>::create(
            &repository,
            &NewUser {
                email: "a@example.com".into(),
                password: "password123".into(),
                display_name: None,
            },
            "plain$x",
            Access::USER,
        )
        .await;
        assert!(matches!(duplicate, Err(RepoError::DuplicateEmail)));
    }

    #[tokio::test]
    async fn batch_lookup_skips_missing_ids_and_empty_input() {
        let repository = InMemoryLinkRepository::new();
        let a = add_user(&repository, "a@example.com", "甲", Access::USER).await;
        let b = add_user(&repository, "b@example.com", "乙", Access::USER).await;

        let found = repository.find_by_ids(&[b.id, 999, a.id]).await.unwrap();
        let mut ids: Vec<u64> = found.iter().map(|user| user.id).collect();
        // 顺序不作承诺（SQL 版也没写 ORDER BY），只要求人齐且不多
        ids.sort_unstable();
        assert_eq!(ids, vec![a.id, b.id]);

        // 空切片不能退化成「查全表」—— 那会把整张用户表吐给调用方
        assert!(repository.find_by_ids(&[]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn first_admin_ignores_regular_users() {
        let repository = InMemoryLinkRepository::new();
        assert!(repository.find_first_admin().await.unwrap().is_none());

        add_user(&repository, "a@example.com", "甲", Access::USER).await;
        assert!(
            repository.find_first_admin().await.unwrap().is_none(),
            "只有普通用户时不该返回任何人"
        );

        let admin = add_user(
            &repository,
            "root@example.com",
            "管理员",
            Access::ADMIN_WITH_TEMP_PASSWORD,
        )
        .await;
        let found = repository
            .find_first_admin()
            .await
            .unwrap()
            .expect("应当找到管理员");
        assert_eq!(found.id, admin.id);
        assert!(found.must_change_password, "初始化管理员要先改密");
    }

    #[tokio::test]
    async fn user_list_is_newest_first_and_counts_links() {
        let repository = InMemoryLinkRepository::new();
        let alice = add_user(&repository, "alice@example.com", "爱丽丝", Access::USER).await;
        let bob = add_user(&repository, "bob@example.com", "鲍勃", Access::USER).await;

        // 两条挂在 alice 名下，一条匿名 —— 列表里的数字只算前两条
        add_link(&repository, "a1", Some(alice.id)).await;
        add_link(&repository, "a2", Some(alice.id)).await;
        add_link(&repository, "loose", None).await;

        let page = repository
            .list_users(&UserQuery {
                page: 1,
                page_size: 10,
                keyword: None,
            })
            .await
            .unwrap();

        assert_eq!(page.total, 2);
        // 新注册的排最前，与 SQL 的 ORDER BY id DESC 一致
        assert_eq!(page.items[0].user.id, bob.id);
        assert_eq!(page.items[1].user.id, alice.id);
        assert_eq!(page.items[0].link_count, 0, "bob 名下一个链接都没有");
        assert_eq!(page.items[1].link_count, 2, "匿名创建的不能算到任何人头上");

        let second = repository
            .list_users(&UserQuery {
                page: 2,
                page_size: 1,
                keyword: None,
            })
            .await
            .unwrap();
        assert_eq!(second.total, 2, "总数不随分页变化");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].user.id, alice.id);
    }

    #[tokio::test]
    async fn user_search_matches_email_or_display_name_ignoring_case() {
        let repository = InMemoryLinkRepository::new();
        add_user(&repository, "alice@example.com", "爱丽丝", Access::USER).await;
        add_user(&repository, "bob@corp.example", "鲍勃", Access::USER).await;

        let by_email = repository
            .list_users(&UserQuery {
                page: 1,
                page_size: 10,
                keyword: Some("ALICE@".into()),
            })
            .await
            .unwrap();
        assert_eq!(by_email.total, 1);
        assert_eq!(by_email.items[0].user.display_name, "爱丽丝");

        let by_name = repository
            .list_users(&UserQuery {
                page: 1,
                page_size: 10,
                keyword: Some("鲍勃".into()),
            })
            .await
            .unwrap();
        assert_eq!(by_name.total, 1);
        assert_eq!(by_name.items[0].user.email, "bob@corp.example");

        let none = repository
            .list_users(&UserQuery {
                page: 1,
                page_size: 10,
                keyword: Some("查无此人".into()),
            })
            .await
            .unwrap();
        assert_eq!(none.total, 0);
        assert!(none.items.is_empty());
    }

    #[tokio::test]
    async fn writes_report_missing_ids_instead_of_silently_succeeding() {
        let repository = InMemoryLinkRepository::new();
        let user = add_user(&repository, "a@example.com", "甲", Access::USER).await;

        assert!(repository
            .set_password(user.id, "plain$y", true)
            .await
            .unwrap());
        assert!(repository
            .set_status(user.id, AccountStatus::Disabled)
            .await
            .unwrap());

        // 目标不存在时返回 false：调用方据此给出 404，而不是假装改成功了
        assert!(!repository.set_password(999, "plain$y", true).await.unwrap());
        assert!(!repository
            .set_status(999, AccountStatus::Disabled)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn deleting_sessions_only_touches_the_named_user() {
        let repository = InMemoryLinkRepository::new();
        let alice = add_user(&repository, "alice@example.com", "甲", Access::USER).await;
        let bob = add_user(&repository, "bob@example.com", "乙", Access::USER).await;
        let now = Utc::now().naive_utc();
        let later = now + chrono::Duration::hours(1);

        <InMemoryLinkRepository as SessionRepository>::create(
            &repository,
            alice.id,
            "token-a1",
            later,
        )
        .await
        .unwrap();
        <InMemoryLinkRepository as SessionRepository>::create(
            &repository,
            alice.id,
            "token-a2",
            later,
        )
        .await
        .unwrap();
        <InMemoryLinkRepository as SessionRepository>::create(
            &repository,
            bob.id,
            "token-b1",
            later,
        )
        .await
        .unwrap();

        // 改密只该踢掉本人的其他设备
        assert_eq!(repository.delete_for_user(alice.id).await.unwrap(), 2);
        assert!(repository
            .find_user("token-a1", now)
            .await
            .unwrap()
            .is_none());
        assert!(repository
            .find_user("token-b1", now)
            .await
            .unwrap()
            .is_some());

        assert_eq!(
            repository.delete_for_user(alice.id).await.unwrap(),
            0,
            "已经没得删了"
        );
    }

    #[tokio::test]
    async fn sessions_of_disabled_users_stop_resolving() {
        let repository = InMemoryLinkRepository::new();
        let user = add_user(&repository, "a@example.com", "甲", Access::USER).await;
        let now = Utc::now().naive_utc();
        let later = now + chrono::Duration::hours(1);
        <InMemoryLinkRepository as SessionRepository>::create(&repository, user.id, "token", later)
            .await
            .unwrap();

        assert!(repository.find_user("token", now).await.unwrap().is_some());

        // 与 SQL 的 `AND u.status = 'active'` 对齐：禁用即刻失效，不必等会话过期
        repository
            .set_status(user.id, AccountStatus::Disabled)
            .await
            .unwrap();
        assert!(repository.find_user("token", now).await.unwrap().is_none());

        // 重新启用后令牌仍在有效期内，因此又活了 —— 这是「禁用」而不是「注销」
        repository
            .set_status(user.id, AccountStatus::Active)
            .await
            .unwrap();
        assert!(repository.find_user("token", now).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn expired_sessions_are_not_resolved() {
        let repository = InMemoryLinkRepository::new();
        let user = add_user(&repository, "a@example.com", "甲", Access::USER).await;
        let now = Utc::now().naive_utc();

        <InMemoryLinkRepository as SessionRepository>::create(&repository, user.id, "token", now)
            .await
            .unwrap();

        // 到期时刻与现在相等即视为过期：边界上宁可让人重登一次
        assert!(repository.find_user("token", now).await.unwrap().is_none());
    }
}
