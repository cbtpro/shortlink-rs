//! `GET /{code}`：直接跳转，或先给一张中转确认页。

use actix_web::{get, http::header, web, HttpResponse};

use crate::api::dto::RedirectQuery;
use crate::api::interstitial;
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

    match state
        .redirect
        .resolve(&code, query.password.as_deref())
        .await?
    {
        Landing::Direct(location) => Ok(HttpResponse::Found()
            .append_header((header::LOCATION, location))
            .finish()),
        Landing::Interstitial(target) => Ok(interstitial::render(&state.assets, &code, &target)),
    }
}
