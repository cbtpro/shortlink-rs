//! 内存仓储，仅测试用。

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{NaiveDateTime, Utc};

use super::{
    CreationHistory, LinkFilter, LinkOverview, LinkPage, LinkQuery, LinkRepository, OverviewWindow,
    RepoError, SessionRepository, UserRepository,
};
use crate::domain::account::{NewUser, User};
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

    fn overview(&self, window: OverviewWindow) -> BoxFuture<'_, Result<LinkOverview, RepoError>> {
        Box::pin(async move {
            self.with_inner(|inner| {
                let mut overview = LinkOverview::default();

                for link in inner.links.values() {
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

                Ok(inner.users.iter().find(|user| user.id == *user_id).cloned())
            })
        })
    }

    fn delete<'a>(&'a self, token_hash: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(
            async move { self.with_inner(|inner| Ok(inner.sessions.remove(token_hash).is_some())) },
        )
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
            .overview(OverviewWindow {
                now,
                today_start: now - chrono::Duration::hours(1),
                expiring_before: now + chrono::Duration::days(7),
            })
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
}
