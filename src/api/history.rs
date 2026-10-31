//! 首页的「最近创建」。
//!
//! 同一个路径服务两种身份，语义明确写在这里：
//!
//! | 身份 | 列表来源 | 删除的含义 |
//! | --- | --- | --- |
//! | 已登录 | `short_links.user_id` 归属 | 短链本身失效 |
//! | 未登录 | 访客 cookie 的创建记录 | 只是移出清单，短链仍可用 |
//!
//! 两者不共用一条数据通路，因为「记录」和「归属」不是一回事：
//! 匿名记录是便利功能，用户归属是所有权。

use actix_web::{delete, get, web, HttpRequest, HttpResponse};

use crate::api::dto::RecentLinkResponse;
use crate::api::session::MaybeAuthUser;
use crate::api::visitor;
use crate::domain::short_link::ShortLink;
use crate::error::AppError;
use crate::state::AppState;

#[get("/recent")]
pub async fn list(
    state: web::Data<AppState>,
    request: HttpRequest,
    viewer: MaybeAuthUser,
) -> Result<HttpResponse, AppError> {
    let (links, issued) = match viewer.0 {
        Some(user) => (state.my_links.list(user.id).await?, None),
        None => {
            // 首次访问没有 cookie：正常返回空列表，顺手把身份发下去
            let (visitor, issued) = visitor::ensure(&request, state.cookie_secure);
            (state.history.recent(&visitor).await?, issued)
        }
    };

    let mut builder = HttpResponse::Ok();
    if let Some(cookie) = issued {
        builder.cookie(cookie);
    }

    Ok(builder.json(serde_json::json!({ "items": render(&state, &links) })))
}

#[delete("/recent/{code}")]
pub async fn forget(
    state: web::Data<AppState>,
    request: HttpRequest,
    viewer: MaybeAuthUser,
    code: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let code = code.into_inner();

    match viewer.0 {
        // 登录用户删的是短链本身
        Some(user) => state.my_links.delete(user.id, &code).await?,
        None => {
            let Some(visitor) = visitor::read(&request) else {
                return Err(AppError::LinkNotFound);
            };

            if !state.history.forget(&visitor, &code).await? {
                return Err(AppError::LinkNotFound);
            }
        }
    }

    Ok(HttpResponse::NoContent().finish())
}

#[delete("/recent")]
pub async fn clear(
    state: web::Data<AppState>,
    request: HttpRequest,
    viewer: MaybeAuthUser,
) -> Result<HttpResponse, AppError> {
    match viewer.0 {
        Some(user) => {
            state.my_links.clear(user.id).await?;
        }
        // 没有身份就是没有记录，直接成功，不必为「本来就空」报错
        None => {
            if let Some(visitor) = visitor::read(&request) {
                state.history.clear(&visitor).await?;
            }
        }
    }

    Ok(HttpResponse::NoContent().finish())
}

fn render(state: &AppState, links: &[ShortLink]) -> Vec<RecentLinkResponse> {
    links
        .iter()
        .map(|link| RecentLinkResponse::new(link, state.shorten.short_url(&link.code)))
        .collect()
}
