//! HTTP 请求 / 响应结构。
//!
//! 时间边界：接口上的 naive 时间按本地时间理解，存储与缓存一律 UTC。
//! 两个方向的换算只在本模块发生。

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::ShortLink;

/// 本地时间 → UTC，供写库使用。
pub(crate) fn local_to_utc(local: NaiveDateTime) -> NaiveDateTime {
    Local
        .from_local_datetime(&local)
        .earliest()
        .map(|dt| dt.naive_utc())
        // 夏令时跳变中被跳过的时间不存在，退回字面量，不 panic
        .unwrap_or(local)
}

/// UTC → 本地时间，供响应输出使用。
pub(crate) fn utc_to_local(utc: NaiveDateTime) -> NaiveDateTime {
    Local.from_utc_datetime(&utc).naive_local()
}

#[derive(Debug, Deserialize)]
pub struct CreateLinkRequest {
    pub url: String,
    /// 留空则由服务端生成
    pub custom_code: Option<String>,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
    /// 跳转前先给一张中转确认页，默认关闭
    pub interstitial: Option<bool>,
}

impl From<CreateLinkRequest> for NewShortLink {
    fn from(request: CreateLinkRequest) -> Self {
        Self {
            // 校验时会 trim，落库前同样 trim，避免把首尾空格带进 Location 头
            url: request.url.trim().to_string(),
            code: request.custom_code,
            expire_at: request.expire_at.map(local_to_utc),
            max_visits: request.max_visits,
            password: request.password,
            ip_limit: request.ip_limit,
            ua_limit: request.ua_limit,
            interstitial: request.interstitial.unwrap_or(false),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CreateLinkResponse {
    pub short_url: String,
    pub code: String,
    pub long_url: String,
    pub created_at: String,
    pub interstitial: bool,
}

impl CreateLinkResponse {
    pub fn new(link: &ShortLink, short_url: String) -> Self {
        Self {
            short_url,
            code: link.code.clone(),
            long_url: link.long_url.clone(),
            created_at: render_local(link.created_at),
            interstitial: link.interstitial,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RedirectQuery {
    pub password: Option<String>,
}

/// 「最近创建」列表项。
///
/// 只给出展示所需字段：密码在这里只是一个布尔，明文永远不出服务端。
#[derive(Debug, Serialize)]
pub struct RecentLinkResponse {
    pub code: String,
    pub short_url: String,
    pub long_url: String,
    pub created_at: String,
    pub expire_at: Option<String>,
    pub max_visits: Option<u32>,
    pub has_password: bool,
    pub interstitial: bool,
}

impl RecentLinkResponse {
    pub fn new(link: &ShortLink, short_url: String) -> Self {
        Self {
            code: link.code.clone(),
            short_url,
            long_url: link.long_url.clone(),
            created_at: render_local(link.created_at),
            expire_at: link.expire_at.map(render_local),
            max_visits: link.max_visits,
            has_password: link.password.is_some(),
            interstitial: link.interstitial,
        }
    }
}

fn render_local(utc: NaiveDateTime) -> String {
    utc_to_local(utc).format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("valid datetime")
    }

    #[test]
    fn expire_at_round_trips_through_utc() {
        let input = at("2026-12-31 23:59:59");
        assert_eq!(utc_to_local(local_to_utc(input)), input);
    }

    #[test]
    fn local_time_is_shifted_by_current_offset() {
        let input = at("2026-12-31 23:59:59");
        let offset = Local::now().offset().local_minus_utc();
        assert_eq!((input - local_to_utc(input)).num_seconds(), offset as i64);
    }

    fn request_json(extra: &str) -> CreateLinkRequest {
        let raw = format!(r#"{{"url":"  https://example.com/a  "{extra}}}"#);
        serde_json::from_str(&raw).expect("request should deserialize")
    }

    #[test]
    fn maps_custom_code_to_domain_field() {
        let request = request_json(r#", "custom_code": "promo", "max_visits": 3"#);
        let new_link: NewShortLink = request.into();

        assert_eq!(new_link.code.as_deref(), Some("promo"));
        assert_eq!(new_link.max_visits, Some(3));
    }

    #[test]
    fn trims_surrounding_whitespace_of_url() {
        let new_link: NewShortLink = request_json("").into();
        assert_eq!(new_link.url, "https://example.com/a");
    }

    #[test]
    fn absent_optional_fields_stay_none() {
        let new_link: NewShortLink = request_json("").into();
        assert!(new_link.code.is_none());
        assert!(new_link.expire_at.is_none());
        assert!(new_link.password.is_none());
        assert!(new_link.ip_limit.is_none());
        assert!(new_link.ua_limit.is_none());
        assert!(!new_link.interstitial, "没给就是不开中转页");
    }

    #[test]
    fn maps_interstitial_flag() {
        let enabled: NewShortLink = request_json(r#", "interstitial": true"#).into();
        assert!(enabled.interstitial);

        // 显式 false 与缺省同义
        let disabled: NewShortLink = request_json(r#", "interstitial": false"#).into();
        assert!(!disabled.interstitial);
    }

    #[test]
    fn converts_expire_at_to_utc() {
        let request = request_json(r#", "expire_at": "2026-12-31T23:59:59""#);
        let new_link: NewShortLink = request.into();

        assert_eq!(
            new_link.expire_at,
            Some(local_to_utc(at("2026-12-31 23:59:59")))
        );
    }

    #[test]
    fn response_renders_created_at_as_local_wall_clock() {
        let utc = at("2026-01-01 00:00:00");
        let link = ShortLink {
            id: 1,
            code: "abc".into(),
            long_url: "https://example.com".into(),
            created_at: utc,
            expire_at: None,
            max_visits: None,
            visit_count: 0,
            password: None,
            ip_limit: None,
            ua_limit: None,
            interstitial: false,
        };

        let response = CreateLinkResponse::new(&link, "https://s.example.com/abc".to_string());
        let expected = utc_to_local(utc).format("%Y-%m-%d %H:%M:%S").to_string();

        assert_eq!(response.created_at, expected);
        assert_eq!(response.created_at.len(), 19);
        assert_eq!(response.short_url, "https://s.example.com/abc");
    }
}
