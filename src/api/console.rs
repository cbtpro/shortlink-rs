//! 控制台接口：短链列表 / 概览 / 删除，以及管理员的账号管理。
//!
//! 鉴权交给 [`crate::api::session::AuthUser`]：没登录 401，被禁用 403，
//! 临时口令未更换 403。这里只负责取当前用户，然后把「能看多少」交给用例层 ——
//! 「普通用户只能看自己」是一条业务规则，不该由每个 handler 各写一遍。

use actix_web::http::header;
use actix_web::{delete, get, post, web, HttpResponse};
use chrono::{DateTime, Duration, Local, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::dto::{local_to_utc, utc_to_local};
use crate::api::session::AuthUser;
use crate::domain::account::{AccountStatus, User};
use crate::domain::short_link::{LinkState, ShortLink};
use crate::error::AppError;
use crate::repository::{LinkFilter, LinkOverview, LinkQuery, OverviewWindow, UserQuery};
use crate::service::console::{ConsolePage, Owner};
use crate::state::AppState;

/// 默认每页条数
const DEFAULT_PAGE_SIZE: u32 = 20;
/// 「即将过期」的观察窗口长度
const EXPIRING_WINDOW_DAYS: i64 = 7;

/// 短链列表查询参数。
#[derive(Debug, Deserialize)]
pub struct ConsoleListQuery {
    pub page: Option<u32>,
    pub size: Option<u32>,
    /// 关键字，同时匹配短码与目标地址
    pub q: Option<String>,
    /// `all` / `active` / `expired` / `exhausted`，其他值按 `all` 处理
    pub status: Option<String>,
    /// 只看某人的链接。普通用户传了也无效，作用域由角色决定
    pub owner: Option<u64>,
}

impl ConsoleListQuery {
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
            owner: self.owner,
        }
    }
}

/// 用户列表查询参数。
#[derive(Debug, Deserialize)]
pub struct UserListQuery {
    pub page: Option<u32>,
    pub size: Option<u32>,
    /// 关键字，同时匹配邮箱与昵称
    pub q: Option<String>,
}

impl UserListQuery {
    fn into_query(self) -> UserQuery {
        UserQuery {
            page: self.page.unwrap_or(1),
            page_size: self.size.unwrap_or(DEFAULT_PAGE_SIZE),
            keyword: self.q,
        }
    }
}

/// 改状态请求体。显式给出目标状态而不是「toggle」：
/// 两个管理员同时点，toggle 会各自反转一次，最后是什么状态说不清。
#[derive(Debug, Deserialize)]
pub struct StatusRequest {
    pub status: String,
}

/// 列表行。时间已换算成本地时间，前端不用再处理时区。
#[derive(Debug, Serialize)]
pub struct ConsoleLinkDto {
    pub code: String,
    pub long_url: String,
    pub created_at: String,
    pub expire_at: Option<String>,
    pub max_visits: Option<u32>,
    pub visit_count: u32,
    pub has_password: bool,
    pub state: LinkState,
    /// 归属用户。匿名链接为 null；普通用户看自己的列表时也是 null
    pub owner: Option<OwnerDto>,
}

#[derive(Debug, Serialize)]
pub struct OwnerDto {
    pub id: u64,
    pub email: String,
    pub display_name: String,
}

impl From<&Owner> for OwnerDto {
    fn from(owner: &Owner) -> Self {
        Self {
            id: owner.id,
            email: owner.email.clone(),
            display_name: owner.display_name.clone(),
        }
    }
}

impl ConsoleLinkDto {
    fn new(link: &ShortLink, owner: Option<&Owner>, now: NaiveDateTime) -> Self {
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
            owner: owner.map(OwnerDto::from),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ConsolePageDto {
    pub items: Vec<ConsoleLinkDto>,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub total_pages: u64,
}

impl ConsolePageDto {
    fn new(page: ConsolePage, query: &LinkQuery, now: NaiveDateTime) -> Self {
        let page_size = query.page_size.max(1);
        let total_pages = page.total.div_ceil(u64::from(page_size));

        Self {
            items: page
                .items
                .iter()
                .map(|item| ConsoleLinkDto::new(&item.link, item.owner.as_ref(), now))
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

/// 用户列表行。
#[derive(Debug, Serialize)]
pub struct UserDto {
    pub id: u64,
    pub email: String,
    pub display_name: String,
    pub role: &'static str,
    pub status: &'static str,
    pub must_change_password: bool,
    pub link_count: u64,
    pub created_at: String,
}

impl UserDto {
    fn new(user: &User, link_count: u64) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            role: user.role.as_str(),
            status: user.status.as_str(),
            must_change_password: user.must_change_password,
            link_count,
            created_at: format_time(user.created_at),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UserPageDto {
    pub items: Vec<UserDto>,
    pub page: u32,
    pub page_size: u32,
    pub total: u64,
    pub total_pages: u64,
}

/// 重置密码的响应：临时口令只在这里出现一次。
#[derive(Debug, Serialize)]
pub struct TempPasswordDto {
    pub email: String,
    pub password: String,
}

/// 改状态后的响应。
#[derive(Debug, Serialize)]
pub struct StatusDto {
    pub id: u64,
    pub status: &'static str,
}

/// `GET /console/api/overview`
#[get("/console/api/overview")]
pub async fn overview(
    state: web::Data<AppState>,
    viewer: AuthUser,
) -> Result<HttpResponse, AppError> {
    let now = Utc::now();
    let summary = state
        .console
        .overview(&viewer.0, overview_window(now))
        .await?;

    Ok(HttpResponse::Ok().json(AdminOverviewDto::from(summary)))
}

/// `GET /console/api/links`
#[get("/console/api/links")]
pub async fn list(
    state: web::Data<AppState>,
    viewer: AuthUser,
    params: web::Query<ConsoleListQuery>,
) -> Result<HttpResponse, AppError> {
    let now = Utc::now();
    let query = params.into_inner().into_query();
    let page = state
        .console
        .list_links(&viewer.0, query.clone(), now)
        .await?;

    Ok(HttpResponse::Ok().json(ConsolePageDto::new(page, &query, now.naive_utc())))
}

/// `DELETE /console/api/links/{code}`
#[delete("/console/api/links/{code}")]
pub async fn delete_link(
    state: web::Data<AppState>,
    viewer: AuthUser,
    code: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    state
        .console
        .delete_link(&viewer.0, &code.into_inner())
        .await?;

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /admin/api/users`
#[get("/admin/api/users")]
pub async fn list_users(
    state: web::Data<AppState>,
    viewer: AuthUser,
    params: web::Query<UserListQuery>,
) -> Result<HttpResponse, AppError> {
    let query = params.into_inner().into_query();
    let page = state.console.list_users(&viewer.0, query.clone()).await?;
    let total_pages = page.total.div_ceil(u64::from(query.page_size.max(1)));

    Ok(HttpResponse::Ok().json(UserPageDto {
        items: page
            .items
            .iter()
            .map(|row| UserDto::new(&row.user, row.link_count))
            .collect(),
        page: query.page,
        page_size: query.page_size,
        total: page.total,
        total_pages,
    }))
}

/// `POST /admin/api/users/{id}/password`：重置为随机临时口令。
///
/// 响应里带明文口令，HTTP 层必须禁止缓存：这是唯一一次能拿到它的机会。
#[post("/admin/api/users/{id}/password")]
pub async fn reset_password(
    state: web::Data<AppState>,
    viewer: AuthUser,
    id: web::Path<u64>,
) -> Result<HttpResponse, AppError> {
    let credential = state
        .console
        .reset_password(&viewer.0, id.into_inner())
        .await?;

    Ok(HttpResponse::Ok()
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .json(TempPasswordDto {
            email: credential.email,
            password: credential.password,
        }))
}

/// `POST /admin/api/users/{id}/status`
#[post("/admin/api/users/{id}/status")]
pub async fn set_status(
    state: web::Data<AppState>,
    viewer: AuthUser,
    id: web::Path<u64>,
    payload: web::Json<StatusRequest>,
) -> Result<HttpResponse, AppError> {
    let status = match payload.status.trim().to_ascii_lowercase().as_str() {
        "active" => AccountStatus::Active,
        "disabled" => AccountStatus::Disabled,
        // 拼错的状态不能被当成「禁用」——那会让一次手滑写错就停掉别人的号
        _ => return Err(AppError::InvalidBody),
    };

    let id = id.into_inner();
    state.console.set_status(&viewer.0, id, status).await?;

    Ok(HttpResponse::Ok().json(StatusDto {
        id,
        status: status.as_str(),
    }))
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
        let query = ConsoleListQuery {
            page: None,
            size: None,
            q: None,
            status: Some("expired".into()),
            owner: None,
        }
        .into_query();

        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(query.filter, LinkFilter::Expired);
        assert_eq!(query.owner, None);
    }

    #[test]
    fn unknown_status_falls_back_to_all() {
        // 宁可放宽也不报错：前端拼错状态时仍能看到全量列表
        for status in [Some("weird"), None] {
            let query = ConsoleListQuery {
                page: Some(2),
                size: Some(5),
                q: Some("abc".into()),
                status: status.map(str::to_string),
                owner: Some(9),
            }
            .into_query();

            assert_eq!(query.filter, LinkFilter::All);
            assert_eq!(query.page, 2);
            assert_eq!(query.page_size, 5);
            assert_eq!(query.keyword.as_deref(), Some("abc"));
            assert_eq!(query.owner, Some(9));
        }
    }

    #[test]
    fn total_pages_rounds_up() {
        let query = LinkQuery {
            page: 1,
            page_size: 20,
            keyword: None,
            filter: LinkFilter::All,
            owner: None,
        };
        let page = ConsolePage {
            items: Vec::new(),
            total: 41,
        };

        let dto = ConsolePageDto::new(
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
