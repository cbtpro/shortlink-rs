//! `GET|POST /{code}`：直接跳转，或先给一张中转确认页。
//!
//! 跳不过去的时候（过期、次数用尽、要密码、短码不存在）返回的是一张状态页
//! 而不是 JSON —— 这一层最终面对的是浏览器里的人和分享出去的链接。

use actix_web::http::{header, StatusCode};
use actix_web::{get, post, web, HttpResponse};

use crate::api::dto::{RedirectForm, RedirectQuery};
use crate::api::interstitial;
use crate::api::notice::{self, Notice};
use crate::domain::short_link::AccessDenial;
use crate::error::AppError;
use crate::service::redirect::Landing;
use crate::state::AppState;

#[get("/{code}")]
pub async fn redirect(
    state: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<RedirectQuery>,
) -> Result<HttpResponse, AppError> {
    let code = path.into_inner();

    // 中转页的「继续访问」带着票据回到这里：这一跳才算一次访问
    if let Some(ticket) = query.confirm.as_deref() {
        if let Some(target) = state.redirect.confirm(&code, ticket).await? {
            return Ok(redirect_to(target, StatusCode::FOUND));
        }
        // 票据过期、已用过或不属于这个短码：落回完整鉴权流程，
        // 该出密码页就出密码页，该重新中转就重新中转
    }

    land(&state, &code, query.password.as_deref(), StatusCode::FOUND).await
}

/// 密码表单提交。密码在请求体里，不进 URL、不进历史记录、不进 Referer。
#[post("/{code}")]
pub async fn submit_password(
    state: web::Data<AppState>,
    path: web::Path<String>,
    form: web::Form<RedirectForm>,
) -> Result<HttpResponse, AppError> {
    let code = path.into_inner();
    // 空串等于没填：与「链接要密码、这次没带」是同一件事
    let password = form.password.as_deref().filter(|value| !value.is_empty());

    // 303 而不是 302：明确要求浏览器把 POST 换成 GET 再去目标站点，
    // 免得刷新时重发表单
    land(&state, &code, password, StatusCode::SEE_OTHER).await
}

/// 两个入口共用的一段：解析 + 变体到响应。
async fn land(
    state: &AppState,
    code: &str,
    password: Option<&str>,
    redirect_status: StatusCode,
) -> Result<HttpResponse, AppError> {
    match state.redirect.resolve(code, password).await {
        Ok(Landing::Direct(location)) => Ok(redirect_to(location, redirect_status)),
        Ok(Landing::Interstitial { target, ticket }) => Ok(interstitial::render(
            &state.assets,
            code,
            &target,
            &continue_url(code, ticket.as_deref(), &target),
        )),
        // 拒绝访问的原因各有各的页面，但都不是服务端故障：原样返回，别升级成 500
        Err(AppError::AccessDenied(reason)) => {
            Ok(notice::render(&state.assets, denial_page(reason), code))
        }
        Err(AppError::LinkNotFound) => Ok(notice::render(&state.assets, Notice::missing(), code)),
        Err(other) => Err(other),
    }
}

fn redirect_to(location: String, status: StatusCode) -> HttpResponse {
    HttpResponse::build(status)
        .append_header((header::LOCATION, location))
        .finish()
}

/// 「继续访问」去哪。
///
/// 有票据就回服务端换一次跳转 —— 只有经过服务端，这一跳才统计得到；
/// 发不出票据（Redis 不可用）时退回目标地址，页面照常可用，只是这一次不计入。
fn continue_url(code: &str, ticket: Option<&str>, target: &str) -> String {
    match ticket {
        Some(ticket) => format!("/{code}?confirm={ticket}"),
        None => target.to_string(),
    }
}

/// 拒绝原因到页面的映射。加新原因时编译器会在这里提醒。
fn denial_page(reason: AccessDenial) -> Notice {
    match reason {
        AccessDenial::Expired => Notice::expired(),
        AccessDenial::VisitLimitReached => Notice::exhausted(),
        AccessDenial::MissingPassword => Notice::password_required(),
        AccessDenial::InvalidPassword => Notice::password_rejected(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_denial_to_a_page() {
        let cases = [
            (AccessDenial::Expired, StatusCode::GONE),
            (AccessDenial::VisitLimitReached, StatusCode::GONE),
            (AccessDenial::MissingPassword, StatusCode::FORBIDDEN),
            (AccessDenial::InvalidPassword, StatusCode::FORBIDDEN),
        ];

        for (reason, expected) in cases {
            assert_eq!(
                denial_page(reason).status,
                expected,
                "unexpected status for {reason:?}"
            );
        }
    }

    #[test]
    fn continue_url_goes_through_the_server_when_a_ticket_exists() {
        // 带票据 = 回服务端换跳转，服务端才有机会计数
        assert_eq!(
            continue_url("abc", Some("deadbeef"), "https://example.com"),
            "/abc?confirm=deadbeef"
        );
        // 没有票据就只能直连目标，页面照样能用
        assert_eq!(
            continue_url("abc", None, "https://example.com"),
            "https://example.com"
        );
    }

    #[test]
    fn only_password_pages_ask_for_input() {
        assert!(denial_page(AccessDenial::MissingPassword).asks_password);
        assert!(denial_page(AccessDenial::InvalidPassword).asks_password);
        assert!(!denial_page(AccessDenial::Expired).asks_password);
        assert!(!denial_page(AccessDenial::VisitLimitReached).asks_password);
        assert!(!Notice::missing().asks_password);
    }
}
