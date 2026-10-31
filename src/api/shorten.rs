//! `POST /shorten`：创建短链接。

use actix_web::{post, web, HttpRequest, HttpResponse};
use chrono::Utc;
use log::warn;

use crate::api::dto::{CreateLinkRequest, CreateLinkResponse};
use crate::api::session::MaybeAuthUser;
use crate::api::visitor;
use crate::domain::new_link::NewShortLink;
use crate::error::AppError;
use crate::state::AppState;

#[post("/shorten")]
pub async fn create_link(
    state: web::Data<AppState>,
    request: HttpRequest,
    viewer: MaybeAuthUser,
    payload: web::Json<CreateLinkRequest>,
) -> Result<HttpResponse, AppError> {
    // 归一化在 DTO 的 TryFrom 里，非法输入（负数次数等）在这一步就 400
    let mut new_link = NewShortLink::try_from(payload.into_inner())?;
    // 归属只由登录态决定：请求体里就算塞了 user_id 也进不来
    new_link.owner_id = viewer.0.as_ref().map(|user| user.id);

    // 时钟在这里读一次并往下传：校验「有效期不能是过去」与落库用的是同一个时刻
    let link = state
        .shorten
        .create(new_link, Utc::now().naive_utc())
        .await?;
    let short_url = state.shorten.short_url(&link.code);

    // 登录用户的链接已经挂在账号下，「最近创建」直接从归属查即可；
    // 匿名创建才需要记一笔访客记录（首次来访还会顺手发 cookie）
    let issued = match &viewer.0 {
        Some(_) => None,
        None => {
            let (visitor, cookie) = visitor::ensure(&request, state.cookie_secure);
            if let Err(err) = state.history.record(&visitor, &link.code).await {
                // 历史是附加功能，写失败不该让用户拿不到短链
                warn!("failed to record creation of `{}`: {err}", link.code);
            }
            cookie
        }
    };

    let mut builder = HttpResponse::Created();
    if let Some(cookie) = issued {
        builder.cookie(cookie);
    }

    Ok(builder.json(CreateLinkResponse::new(&link, short_url)))
}
