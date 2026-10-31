//! 首页的「最近创建」。

use actix_web::{delete, get, web, HttpRequest, HttpResponse};

use crate::api::dto::RecentLinkResponse;
use crate::api::visitor;
use crate::error::AppError;
use crate::state::AppState;

#[get("/recent")]
pub async fn list(
    state: web::Data<AppState>,
    request: HttpRequest,
) -> Result<HttpResponse, AppError> {
    // 首次访问没有 cookie：正常返回空列表，顺手把身份发下去
    let (visitor, issued) = visitor::ensure(&request, state.cookie_secure);
    let links = state.history.recent(&visitor).await?;

    let items: Vec<RecentLinkResponse> = links
        .iter()
        .map(|link| RecentLinkResponse::new(link, state.shorten.short_url(&link.code)))
        .collect();

    let mut builder = HttpResponse::Ok();
    if let Some(cookie) = issued {
        builder.cookie(cookie);
    }

    Ok(builder.json(serde_json::json!({ "items": items })))
}

#[delete("/recent/{code}")]
pub async fn forget(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    let Some(visitor) = visitor::read(&request) else {
        return Err(AppError::LinkNotFound);
    };

    if !state.history.forget(&visitor, &code).await? {
        return Err(AppError::LinkNotFound);
    }

    Ok(HttpResponse::NoContent().finish())
}

#[delete("/recent")]
pub async fn clear(
    state: web::Data<AppState>,
    request: HttpRequest,
) -> Result<HttpResponse, AppError> {
    // 没有身份就是没有记录，直接成功，不必为「本来就空」报错
    if let Some(visitor) = visitor::read(&request) {
        state.history.clear(&visitor).await?;
    }

    Ok(HttpResponse::NoContent().finish())
}
