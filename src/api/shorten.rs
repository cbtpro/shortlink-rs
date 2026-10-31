//! `POST /shorten`：创建短链接。

use actix_web::{post, web, HttpRequest, HttpResponse};
use log::warn;

use crate::api::dto::{CreateLinkRequest, CreateLinkResponse};
use crate::api::visitor;
use crate::error::AppError;
use crate::state::AppState;

#[post("/shorten")]
pub async fn create_link(
    state: web::Data<AppState>,
    request: HttpRequest,
    payload: web::Json<CreateLinkRequest>,
) -> Result<HttpResponse, AppError> {
    // 先取身份：创建成功后要立刻把它记进历史，首次来访时也顺手发 cookie
    let (visitor, issued) = visitor::ensure(&request, state.cookie_secure);

    let link = state.shorten.create(payload.into_inner().into()).await?;
    let short_url = state.shorten.short_url(&link.code);

    if let Err(err) = state.history.record(&visitor, &link.code).await {
        // 历史是附加功能，写失败不该让用户拿不到短链
        warn!("failed to record creation of `{}`: {err}", link.code);
    }

    let mut builder = HttpResponse::Created();
    if let Some(cookie) = issued {
        builder.cookie(cookie);
    }

    Ok(builder.json(CreateLinkResponse::new(&link, short_url)))
}
