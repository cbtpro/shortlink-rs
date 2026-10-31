//! 注册、登录、注销与当前用户。

use actix_web::{get, post, web, HttpResponse};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::api::dto::utc_to_local;
use crate::api::session::{self, MaybeAuthUser};
use crate::domain::account::{NewUser, User};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// 对外的用户视图。这里没有 `password_hash` —— 字段列表本身就是白名单。
#[derive(Debug, Serialize)]
pub struct UserDto {
    pub id: u64,
    pub email: String,
    pub display_name: String,
    pub created_at: String,
}

impl From<&User> for UserDto {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            email: user.email.clone(),
            display_name: user.display_name.clone(),
            created_at: render_local(user.created_at),
        }
    }
}

/// `POST /auth/register`
#[post("/auth/register")]
pub async fn register(
    state: web::Data<AppState>,
    payload: web::Json<RegisterRequest>,
) -> Result<HttpResponse, AppError> {
    let request = payload.into_inner();
    let issued = state
        .auth
        .register(NewUser {
            email: request.email,
            password: request.password,
            display_name: request.display_name,
        })
        .await?;

    Ok(session_response(
        &state,
        HttpResponse::Created(),
        &issued.token,
        &issued.user,
    ))
}

/// `POST /auth/login`
#[post("/auth/login")]
pub async fn login(
    state: web::Data<AppState>,
    payload: web::Json<LoginRequest>,
) -> Result<HttpResponse, AppError> {
    let request = payload.into_inner();
    let issued = state.auth.login(&request.email, &request.password).await?;

    Ok(session_response(
        &state,
        HttpResponse::Ok(),
        &issued.token,
        &issued.user,
    ))
}

/// `POST /auth/logout`：清掉服务端会话，并让浏览器丢弃 cookie。
#[post("/auth/logout")]
pub async fn logout(
    state: web::Data<AppState>,
    request: actix_web::HttpRequest,
) -> Result<HttpResponse, AppError> {
    state
        .auth
        .logout(session::read(&request).as_deref())
        .await?;

    Ok(HttpResponse::NoContent()
        .cookie(session::clearing_cookie(state.cookie_secure))
        .finish())
}

/// `GET /auth/me`：未登录时返回 `{"user": null}` 而不是 401。
///
/// 页首的登录状态是最普通的展示需求，用 401 会让它每次都被当成错误上报；
/// 而「未登录就不能做」的判断留给具体接口自己给出，语义不混在一起。
#[get("/auth/me")]
pub async fn me(viewer: MaybeAuthUser) -> HttpResponse {
    let body = match viewer.0 {
        Some(user) => serde_json::json!({ "user": UserDto::from(&user) }),
        None => serde_json::json!({ "user": null }),
    };

    // 响应随 cookie 变化，任何缓存层存下来都会串号
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .json(body)
}

fn session_response(
    state: &AppState,
    mut builder: actix_web::HttpResponseBuilder,
    token: &str,
    user: &User,
) -> HttpResponse {
    let cookie = session::cookie(token, state.auth.session_ttl(), state.cookie_secure);

    builder
        // 会话相关的响应一律不缓存：中间层存下来会串号
        .insert_header((actix_web::http::header::CACHE_CONTROL, "no-store"))
        .cookie(cookie)
        .json(serde_json::json!({ "user": UserDto::from(user) }))
}

fn render_local(utc: NaiveDateTime) -> String {
    utc_to_local(utc).format("%Y-%m-%d %H:%M:%S").to_string()
}
