//! 短链接实体与访问策略。
//!
//! `NaiveDateTime` 一律是 UTC，换算成本地时间只发生在 `api::dto`。

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

/// 短链接聚合根，对应 MySQL 表 `short_links`。
#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
pub struct ShortLink {
    pub id: u64,
    pub code: String,
    /// 归属用户。`None` 表示匿名创建（未登录时生成），只能靠访客 cookie 找回。
    pub user_id: Option<u64>,
    pub long_url: String,
    pub created_at: NaiveDateTime,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub visit_count: u32,
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
    /// 跳转前是否先给一张中转确认页
    pub interstitial: bool,
}

impl ShortLink {
    /// 带密码、限制或次数上限的链接不进缓存：
    /// 敏感字段不落 Redis，缓存里的旧计数也不会让上限与限制判定失真。
    pub fn is_cacheable(&self) -> bool {
        self.password.is_none()
            && self.ip_limit.is_none()
            && self.ua_limit.is_none()
            && self.max_visits.is_none()
    }

    /// 链接当前状态，供后台列表展示。判定条件须与 `AccessPolicy::check` 一致。
    pub fn state(&self, now: NaiveDateTime) -> LinkState {
        if self.expire_at.is_some_and(|expire_at| expire_at <= now) {
            return LinkState::Expired;
        }
        if self.max_visits.is_some_and(|max| self.visit_count >= max) {
            return LinkState::Exhausted;
        }
        LinkState::Active
    }
}

/// 链接的生命周期状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    Active,
    Expired,
    Exhausted,
}

/// 缓存投影：跳转判定所需的最小字段，不含密码。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedLink {
    pub id: u64,
    pub long_url: String,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub visit_count: u32,
    /// 缓存命中时同样要按中转页设置出响应，漏了它就会「第一次中转、之后直跳」
    pub interstitial: bool,
}

impl From<&ShortLink> for CachedLink {
    fn from(link: &ShortLink) -> Self {
        Self {
            id: link.id,
            long_url: link.long_url.clone(),
            expire_at: link.expire_at,
            max_visits: link.max_visits,
            visit_count: link.visit_count,
            interstitial: link.interstitial,
        }
    }
}

/// 一次访问请求的上下文。
#[derive(Clone, Debug)]
pub struct AccessRequest<'a> {
    pub now: NaiveDateTime,
    pub password: Option<&'a str>,
    /// 尚未回刷到数据库的访问增量
    pub pending_visits: u32,
}

/// 访问被拒绝的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessDenial {
    Expired,
    VisitLimitReached,
    MissingPassword,
    InvalidPassword,
}

/// 过期 / 密码 / 次数上限三条规则的唯一实现，实体与缓存投影共用。
#[derive(Clone, Debug)]
pub struct AccessPolicy {
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub visit_count: u32,
    pub password: Option<String>,
}

impl AccessPolicy {
    pub fn requires_visit_count(&self) -> bool {
        self.max_visits.is_some()
    }

    pub fn check(&self, request: &AccessRequest<'_>) -> Result<(), AccessDenial> {
        if self
            .expire_at
            .is_some_and(|expire_at| expire_at <= request.now)
        {
            return Err(AccessDenial::Expired);
        }

        if let Some(expected) = self.password.as_deref() {
            match request.password {
                None => return Err(AccessDenial::MissingPassword),
                Some(given) if given == expected => {}
                Some(_) => return Err(AccessDenial::InvalidPassword),
            }
        }

        // 上限按「已入库次数 + 尚未回刷的增量」计算，否则缓存期内计数会漏判
        let total_visits = self.visit_count.saturating_add(request.pending_visits);
        if self.max_visits.is_some_and(|max| total_visits >= max) {
            return Err(AccessDenial::VisitLimitReached);
        }

        Ok(())
    }
}

impl From<&ShortLink> for AccessPolicy {
    fn from(link: &ShortLink) -> Self {
        Self {
            expire_at: link.expire_at,
            max_visits: link.max_visits,
            visit_count: link.visit_count,
            password: link.password.clone(),
        }
    }
}

impl From<&CachedLink> for AccessPolicy {
    fn from(link: &CachedLink) -> Self {
        Self {
            expire_at: link.expire_at,
            max_visits: link.max_visits,
            visit_count: link.visit_count,
            password: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(hour: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap()
    }

    fn policy(expire_at: Option<NaiveDateTime>, max_visits: Option<u32>) -> AccessPolicy {
        AccessPolicy {
            expire_at,
            max_visits,
            visit_count: 0,
            password: None,
        }
    }

    fn request<'a>(now: NaiveDateTime, password: Option<&'a str>) -> AccessRequest<'a> {
        AccessRequest {
            now,
            password,
            pending_visits: 0,
        }
    }

    fn link_fixture() -> ShortLink {
        ShortLink {
            id: 1,
            code: "abc".into(),
            user_id: None,
            long_url: "https://example.com".into(),
            created_at: at(0),
            expire_at: None,
            max_visits: None,
            visit_count: 0,
            password: None,
            ip_limit: None,
            ua_limit: None,
            interstitial: false,
        }
    }

    #[test]
    fn rejects_expired_link() {
        let p = policy(Some(at(12)), None);
        assert_eq!(p.check(&request(at(13), None)), Err(AccessDenial::Expired));
    }

    #[test]
    fn password_rules() {
        let mut p = policy(None, None);
        p.password = Some("secret".into());

        assert_eq!(
            p.check(&request(at(12), None)),
            Err(AccessDenial::MissingPassword)
        );
        assert_eq!(
            p.check(&request(at(12), Some("wrong"))),
            Err(AccessDenial::InvalidPassword)
        );
        assert!(p.check(&request(at(12), Some("secret"))).is_ok());

        // 没设密码时带不带都放行
        assert!(policy(None, None).check(&request(at(12), None)).is_ok());
    }

    #[test]
    fn visit_limit_counts_pending_increments() {
        let p = policy(None, Some(3));
        let mut request = request(at(12), None);
        request.pending_visits = 3;
        assert_eq!(p.check(&request), Err(AccessDenial::VisitLimitReached));
    }

    #[test]
    fn protected_links_are_not_cacheable() {
        let mut link = link_fixture();
        assert!(link.is_cacheable());

        // 中转页只是展示方式，不含敏感字段，不影响可缓存性
        link.interstitial = true;
        assert!(link.is_cacheable());
        link.interstitial = false;

        link.password = Some("secret".into());
        assert!(!link.is_cacheable());

        link.password = None;
        link.max_visits = Some(3);
        assert!(!link.is_cacheable());
    }

    #[test]
    fn rate_limits_disable_caching() {
        let mut link = link_fixture();
        link.ip_limit = Some(serde_json::json!({ "whitelist": ["1.2.3.4"] }));
        assert!(!link.is_cacheable());

        link.ip_limit = None;
        link.ua_limit = Some(serde_json::json!({ "blacklist": ["curl"] }));
        assert!(!link.is_cacheable());
    }

    #[test]
    fn expiry_boundary_is_inclusive() {
        // expire_at == now 就算过期，不留越权窗口
        assert_eq!(
            policy(Some(at(12)), None).check(&request(at(12), None)),
            Err(AccessDenial::Expired)
        );
        assert!(policy(Some(at(13)), None)
            .check(&request(at(12), None))
            .is_ok());
    }

    #[test]
    fn visit_limit_boundary_is_inclusive() {
        let mut reached = policy(None, Some(2));
        reached.visit_count = 2;
        assert_eq!(
            reached.check(&request(at(12), None)),
            Err(AccessDenial::VisitLimitReached)
        );

        let mut below = policy(None, Some(2));
        below.visit_count = 1;
        assert!(below.check(&request(at(12), None)).is_ok());
    }

    #[test]
    fn pending_visits_do_not_overflow() {
        let mut p = policy(None, Some(u32::MAX));
        p.visit_count = u32::MAX;

        let mut request = request(at(12), None);
        request.pending_visits = 10;

        // saturating_add 溢出时停在 u32::MAX，不 panic，仍判超限
        assert_eq!(p.check(&request), Err(AccessDenial::VisitLimitReached));
    }

    #[test]
    fn only_limited_links_need_visit_count() {
        assert!(!policy(None, None).requires_visit_count());
        assert!(policy(None, Some(1)).requires_visit_count());
    }

    #[test]
    fn cached_policy_carries_no_password() {
        let mut link = link_fixture();
        link.password = Some("secret".into());

        let cached = CachedLink::from(&link);
        assert!(AccessPolicy::from(&cached).password.is_none());
    }

    #[test]
    fn cached_payload_omits_sensitive_fields() {
        let mut link = link_fixture();
        link.password = Some("secret".into());

        let json = serde_json::to_string(&CachedLink::from(&link)).expect("should serialize");
        assert!(!json.contains("secret"));
        assert!(!json.contains("password"));
    }

    #[test]
    fn cached_payload_round_trips() {
        let mut link = link_fixture();
        link.expire_at = Some(at(13));
        link.max_visits = Some(9);
        link.visit_count = 4;
        link.interstitial = true;

        let json = serde_json::to_string(&CachedLink::from(&link)).expect("should serialize");
        let restored: CachedLink = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(restored.expire_at, Some(at(13)));
        assert_eq!(restored.max_visits, Some(9));
        assert_eq!(restored.visit_count, 4);
        assert!(restored.interstitial, "中转页设置必须随缓存一起往返");
        assert_eq!(restored.long_url, link.long_url);
    }

    #[test]
    fn state_matches_access_policy() {
        // 列表展示与跳转判定必须对同一链接给出同一结论
        let mut active = link_fixture();
        active.expire_at = Some(at(13));
        active.max_visits = Some(3);
        active.visit_count = 2;
        assert_eq!(active.state(at(12)), LinkState::Active);
        assert!(AccessPolicy::from(&active)
            .check(&request(at(12), None))
            .is_ok());

        let mut expired = link_fixture();
        expired.expire_at = Some(at(12));
        assert_eq!(expired.state(at(12)), LinkState::Expired);
        assert_eq!(
            AccessPolicy::from(&expired).check(&request(at(12), None)),
            Err(AccessDenial::Expired)
        );

        let mut exhausted = link_fixture();
        exhausted.max_visits = Some(2);
        exhausted.visit_count = 2;
        assert_eq!(exhausted.state(at(12)), LinkState::Exhausted);
        assert_eq!(
            AccessPolicy::from(&exhausted).check(&request(at(12), None)),
            Err(AccessDenial::VisitLimitReached)
        );
    }

    #[test]
    fn expiry_wins_over_exhaustion() {
        // 两种失效同时满足时按 check 的顺序先报过期
        let mut link = link_fixture();
        link.expire_at = Some(at(11));
        link.max_visits = Some(1);
        link.visit_count = 1;

        assert_eq!(link.state(at(12)), LinkState::Expired);
    }
}
