//! 后台管理接口，鉴权用 `Authorization: Bearer <ADMIN_TOKEN>`。

use actix_web::http::header;
use actix_web::{delete, get, web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Local, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::dto::{local_to_utc, utc_to_local};
use crate::domain::short_link::{LinkState, ShortLink};
use crate::error::AppError;
use crate::repository::{LinkFilter, LinkOverview, LinkPage, LinkQuery, OverviewWindow};
use crate::state::AppState;

/// 默认每页条数
const DEFAULT_PAGE_SIZE: u32 = 20;
/// 「即将过期」的观察窗口长度
const EXPIRING_WINDOW_DAYS: i64 = 7;

/// 列表查询参数。
#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    pub page: Option<u32>,
    pub size: Option<u32>,
    /// 关键字，同时匹配短码与目标地址
    pub q: Option<String>,
    /// `all` / `active` / `expired` / `exhausted`，其他值按 `all` 处理
    pub status: Option<String>,
}

impl AdminListQuery {
    fn into_query(self) -> LinkQuery {
        let filter = match self.status.as_deref() {
            Some("active") => LinkFilter::Active,
            Some("expired") => LinkFilter::Expired,
            Some("exhausted") => LinkFilter::Exhausted,
            _ => LinkFilter::All,
        };

        LinkQuery {
            page: self.page.unwrap_or(1),
            page_size: self.size.unwrap_or(DEFAULT_PAGE_SIZE),
            keyword: self.q,
            filter,
        }
    }
}

/// 列表行。时间已换算成本地时间，前端不用再处理时区。
#[derive(Debug, Serialize)]
pub struct AdminLinkDto {
    pub code: String,
    pub long_url: String,
    pub created_at: String,
    pub expire_at: Option<String>,
    pub max_visits: Option<u32>,
    pub visit_count: u32,
    pub has_password: bool,
    pub state: LinkState,
}

impl AdminLinkDto {
    fn new(link: &ShortLink, now: NaiveDateTime) -> Self {
        Self {
            code: link.code.clone(),
            long_url: link.long_url.clone(),
            created_at: format_time(link.created_at),
            expire_at: link.expire_at.map(format_time),
            max_visits: link.max_visits,
            visit_count: link.visit_count,
            // 只暴露「有没有密码」，凭据本身不出库
            has_password: link.password.is_some(),
            state: link.state(now),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AdminPageDto {
    pub items: Vec<AdminLinkDto>,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub total_pages: u64,
}

impl AdminPageDto {
    fn new(page: LinkPage, query: &LinkQuery, now: NaiveDateTime) -> Self {
        let page_size = query.page_size.max(1);
        let total_pages = page.total.div_ceil(u64::from(page_size));

        Self {
            items: page
                .items
                .iter()
                .map(|link| AdminLinkDto::new(link, now))
                .collect(),
            page: query.page,
            page_size,
            total: page.total,
            total_pages,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AdminOverviewDto {
    pub total_links: u64,
    pub total_visits: u64,
    pub created_today: u64,
    pub expiring_soon: u64,
}

impl From<LinkOverview> for AdminOverviewDto {
    // 参数不能叫 `overview`：会与同名 handler 冲突，被当成常量模式
    fn from(summary: LinkOverview) -> Self {
        Self {
            total_links: summary.total_links,
            total_visits: summary.total_visits,
            created_today: summary.created_today,
            expiring_soon: summary.expiring_soon,
        }
    }
}

/// `GET /admin/api/overview`
#[get("/admin/api/overview")]
pub async fn overview(
    state: web::Data<AppState>,
    request: HttpRequest,
) -> Result<HttpResponse, AppError> {
    state.admin.authorize(bearer_token(&request))?;

    let now = Utc::now();
    let summary = state.admin.overview(overview_window(now)).await?;

    Ok(HttpResponse::Ok().json(AdminOverviewDto::from(summary)))
}

/// `GET /admin/api/links`
#[get("/admin/api/links")]
pub async fn list(
    state: web::Data<AppState>,
    request: HttpRequest,
    params: web::Query<AdminListQuery>,
) -> Result<HttpResponse, AppError> {
    state.admin.authorize(bearer_token(&request))?;

    let now = Utc::now();
    let query = params.into_inner().into_query();
    let page = state.admin.list(query.clone(), now).await?;

    Ok(HttpResponse::Ok().json(AdminPageDto::new(page, &query, now.naive_utc())))
}

/// `DELETE /admin/api/links/{code}`
#[delete("/admin/api/links/{code}")]
pub async fn delete_link(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    state.admin.authorize(bearer_token(&request))?;
    state.admin.delete(&code.into_inner()).await?;

    Ok(HttpResponse::NoContent().finish())
}

/// 从 `Authorization: Bearer xxx` 中取出令牌。
///
/// scheme 按 RFC 7235 大小写不敏感；格式不对一律视为「未提供」，
/// 由用例层统一判 401，避免把「格式错误」和「未启用」混为一谈。
fn bearer_token(request: &HttpRequest) -> Option<&str> {
    let value = request.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;

    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }

    let token = token.trim();
    (!token.is_empty()).then_some(token)
}

/// 概览的时间窗口。
///
/// 「今天」按本地日历划分再折回 UTC —— 时区换算只发生在 API 边界，
/// 领域与存储层看到的始终是 UTC。
fn overview_window(now: DateTime<Utc>) -> OverviewWindow {
    let today_start = now
        .with_timezone(&Local)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always a valid time");

    OverviewWindow {
        now: now.naive_utc(),
        today_start: local_to_utc(today_start),
        expiring_before: (now + Duration::days(EXPIRING_WINDOW_DAYS)).naive_utc(),
    }
}

fn format_time(utc: NaiveDateTime) -> String {
    utc_to_local(utc).format("%Y-%m-%d %H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn query_defaults_and_status_mapping() {
        let query = AdminListQuery {
            page: None,
            size: None,
            q: None,
            status: Some("expired".into()),
        }
        .into_query();

        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(query.filter, LinkFilter::Expired);
    }

    #[test]
    fn unknown_status_falls_back_to_all() {
        // 宁可放宽也不报错：前端拼错状态时仍能看到全量列表
        for status in [Some("weird"), None] {
            let query = AdminListQuery {
                page: Some(2),
                size: Some(5),
                q: Some("abc".into()),
                status: status.map(str::to_string),
            }
            .into_query();

            assert_eq!(query.filter, LinkFilter::All);
            assert_eq!(query.page, 2);
            assert_eq!(query.page_size, 5);
            assert_eq!(query.keyword.as_deref(), Some("abc"));
        }
    }

    #[test]
    fn total_pages_rounds_up() {
        let query = LinkQuery {
            page: 1,
            page_size: 20,
            keyword: None,
            filter: LinkFilter::All,
        };
        let page = LinkPage {
            items: Vec::new(),
            total: 41,
        };

        let dto = AdminPageDto::new(
            page,
            &query,
            NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        );
        assert_eq!(dto.total_pages, 3);
    }

    #[test]
    fn window_starts_at_local_midnight() {
        let now = Utc::now();
        let window = overview_window(now);

        // 本地零点折回 UTC 后应恰好落在本地日界上
        assert_eq!(
            utc_to_local(window.today_start).time(),
            chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
        );
        assert!(window.expiring_before > window.now);
    }
}
