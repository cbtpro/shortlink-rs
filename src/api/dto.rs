//! HTTP 请求 / 响应结构。
//!
//! 时间边界：接口上的 naive 时间按本地时间理解，存储与缓存一律 UTC。
//! 两个方向的换算只在本模块发生。

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::new_link::{NewShortLink, ValidationError};
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
    /// 用 `i64` 而不是 `u32`：负数得先能读进来，才谈得上「拒绝并说清原因」。
    /// 用 `u32` 的话 serde 在反序列化阶段就报错，客户端拿到的是解析器的原话。
    pub max_visits: Option<i64>,
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
    /// 跳转前先给一张中转确认页，默认关闭
    pub interstitial: Option<bool>,
}

/// 请求体 → 业务意图。
///
/// 这里是**归一化**的唯一位置：去首尾空格、空串归 `None`、负数字段拒绝。
/// 转换可能失败，所以是 `TryFrom` 而不是 `From` —— 静默丢掉非法值比报错危险得多。
impl TryFrom<CreateLinkRequest> for NewShortLink {
    type Error = ValidationError;

    fn try_from(request: CreateLinkRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            // 校验时会 trim，落库前同样 trim，避免把首尾空格带进 Location 头
            url: request.url.trim().to_string(),
            code: normalize_optional(request.custom_code),
            // 归属由 handler 按登录态补上，请求体说了不算
            owner_id: None,
            expire_at: request.expire_at.map(local_to_utc),
            max_visits: normalize_max_visits(request.max_visits)?,
            password: normalize_password(request.password),
            ip_limit: request.ip_limit,
            ua_limit: request.ua_limit,
            interstitial: request.interstitial.unwrap_or(false),
        })
    }
}

/// 去首尾空格，全空视为「没填」。
fn normalize_optional(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 密码只 trim，不把空串归 `None`。
///
/// 「留空」是没设密码，「填了几个空格」是想设密码却没填对 —— 后者必须报错，
/// 悄悄降级成「不设密码」会让人以为链接有保护。
fn normalize_password(raw: Option<String>) -> Option<String> {
    raw.as_deref().map(str::trim).map(str::to_string)
}

fn normalize_max_visits(raw: Option<i64>) -> Result<Option<u32>, ValidationError> {
    match raw {
        None => Ok(None),
        // 负数不能被当成「不限制」放过去
        Some(value) if value < 0 => Err(ValidationError::InvalidMaxVisits),
        Some(value) => u32::try_from(value)
            .map(Some)
            .map_err(|_| ValidationError::InvalidMaxVisits),
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
    /// 中转页「继续访问」带回来的票据。
    ///
    /// 只有在服务端签发过票据、并且这张票据还没被兑换时才算数 ——
    /// 它证明「刚刚通过鉴权的是同一个人」，而不是一个可以随手加的参数。
    pub confirm: Option<String>,
}

/// 密码表单。字段名与状态页里 `<input name>` 一致。
///
/// 用表单而不是把密码拼进 URL：URL 会进浏览器历史、进 Referer、进反代日志。
#[derive(Debug, Deserialize)]
pub struct RedirectForm {
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

    fn convert(extra: &str) -> NewShortLink {
        NewShortLink::try_from(request_json(extra)).expect("request should convert")
    }

    #[test]
    fn maps_custom_code_to_domain_field() {
        let new_link = convert(r#", "custom_code": "promo", "max_visits": 3"#);

        assert_eq!(new_link.code.as_deref(), Some("promo"));
        assert_eq!(new_link.max_visits, Some(3));
    }

    #[test]
    fn trims_surrounding_whitespace_of_url() {
        assert_eq!(convert("").url, "https://example.com/a");
    }

    #[test]
    fn trims_surrounding_whitespace_of_custom_code() {
        // 用户复制短码时常带空格，「 promo 」按「promo」处理，不要拿空格去报错
        assert_eq!(
            convert(r#", "custom_code": "  promo  ""#).code.as_deref(),
            Some("promo")
        );
    }

    #[test]
    fn blank_custom_code_means_generate_one() {
        assert!(convert(r#", "custom_code": "   ""#).code.is_none());
    }

    #[test]
    fn trims_surrounding_whitespace_of_password() {
        assert_eq!(
            convert(r#", "password": "  s3cret  ""#).password.as_deref(),
            Some("s3cret")
        );
    }

    #[test]
    fn blank_password_stays_empty_instead_of_becoming_none() {
        // 交给领域层报“空密码”，不能悄悄变成「不设密码」
        let new_link = convert(r#", "password": "   ""#);
        assert_eq!(new_link.password.as_deref(), Some(""));
        assert!(new_link.validate(10, at("2026-06-01 12:00:00")).is_err());
    }

    #[test]
    fn absent_optional_fields_stay_none() {
        let new_link = convert("");
        assert!(new_link.code.is_none());
        assert!(new_link.expire_at.is_none());
        assert!(new_link.max_visits.is_none());
        assert!(new_link.password.is_none());
        assert!(new_link.ip_limit.is_none());
        assert!(new_link.ua_limit.is_none());
        assert!(!new_link.interstitial, "没给就是不开中转页");
    }

    #[test]
    fn maps_interstitial_flag() {
        assert!(convert(r#", "interstitial": true"#).interstitial);

        // 显式 false 与缺省同义
        assert!(!convert(r#", "interstitial": false"#).interstitial);
    }

    #[test]
    fn converts_expire_at_to_utc() {
        let new_link = convert(r#", "expire_at": "2026-12-31T23:59:59""#);

        assert_eq!(
            new_link.expire_at,
            Some(local_to_utc(at("2026-12-31 23:59:59")))
        );
    }

    #[test]
    fn rejects_negative_max_visits() {
        let error = NewShortLink::try_from(request_json(r#", "max_visits": -1"#))
            .expect_err("负数必须被拒绝");
        assert_eq!(error, ValidationError::InvalidMaxVisits);
    }

    #[test]
    fn rejects_max_visits_beyond_u32() {
        let error = NewShortLink::try_from(request_json(r#", "max_visits": 4294967296"#))
            .expect_err("超出 u32 必须被拒绝");
        assert_eq!(error, ValidationError::InvalidMaxVisits);
    }

    #[test]
    fn accepts_max_visits_at_u32_ceiling() {
        // 类型上界能收下，业务上界由领域层的 validate 判定
        let new_link = convert(r#", "max_visits": 4294967295"#);
        assert_eq!(new_link.max_visits, Some(u32::MAX));
    }

    #[test]
    fn response_renders_created_at_as_local_wall_clock() {
        let utc = at("2026-01-01 00:00:00");
        let link = ShortLink {
            id: 1,
            code: "abc".into(),
            user_id: None,
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
