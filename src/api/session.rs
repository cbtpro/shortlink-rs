//! 登录会话：cookie 读写 + 请求提取器。
//!
//! 提取器是这里的关键设计：handler 只要在参数里声明 `MaybeAuthUser`，
//! 拿到的就是 `Option<User>` 而不是一个待解析的字符串。解析与校验只有一份，
//! 不会出现某个接口漏了读 cookie 或者忘了验过期的情况。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use actix_web::cookie::{time, Cookie, SameSite};
use actix_web::{web, FromRequest, HttpRequest};

use crate::domain::account::User;
use crate::error::AppError;
use crate::security::token;
use crate::state::AppState;

pub const COOKIE_NAME: &str = "sl_session";

/// 从请求里读令牌。格式不合法一律当没登录，避免任意字符串被拿去查库。
pub fn read(request: &HttpRequest) -> Option<String> {
    let cookie = request.cookie(COOKIE_NAME)?;
    let value = cookie.value();

    token::is_well_formed(value).then(|| value.to_string())
}

/// 要写回浏览器的会话 cookie。
///
/// `HttpOnly` 让脚本读不到令牌（XSS 也偷不走会话）；
/// `SameSite=Lax` 让跨站的表单提交带不上它，配合「所有写接口都是 JSON」
/// 就足以挡住 CSRF，不必再引入 token 双提交那一套。
pub fn cookie(token: &str, ttl: Duration, secure: bool) -> Cookie<'static> {
    let max_age = time::Duration::seconds(ttl.as_secs().min(i64::MAX as u64) as i64);

    build(token.to_string(), max_age, secure)
}

/// 注销用的空 cookie：同名同路径，`Max-Age=0` 让浏览器立刻丢弃。
pub fn clearing_cookie(secure: bool) -> Cookie<'static> {
    build(String::new(), time::Duration::ZERO, secure)
}

fn build(value: String, max_age: time::Duration, secure: bool) -> Cookie<'static> {
    let mut builder = Cookie::build(COOKIE_NAME, value)
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(max_age);

    if secure {
        builder = builder.secure(true);
    }

    builder.finish()
}

/// 「登录了就是用户，没登录就是游客」。
pub struct MaybeAuthUser(pub Option<User>);

type AuthFuture<T> = Pin<Box<dyn Future<Output = Result<T, AppError>>>>;

impl FromRequest for MaybeAuthUser {
    type Error = AppError;
    type Future = AuthFuture<Self>;

    fn from_request(request: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let state = request.app_data::<web::Data<AppState>>().cloned();
        let presented = read(request);

        Box::pin(async move {
            let (Some(state), Some(presented)) = (state, presented) else {
                return Ok(MaybeAuthUser(None));
            };

            // 存储故障要如实报 500：静默当游客会把「服务坏了」伪装成「没登录」，
            // 用户会以为自己掉线了，反复重新登录。
            let user = state.auth.authenticate(&presented).await?;
            Ok(MaybeAuthUser(user))
        })
    }
}

/// 「必须是可用的已登录用户」。
///
/// 与 [`MaybeAuthUser`] 的分工：那是「登录了就是用户，没登录就是游客」的展示需求，
/// 这是「没登录就不许做」的准入判断。另外两类账号在这里一次性挡掉：
/// - 被禁用的账号（会话查询已经过滤，这里再兜一层）；
/// - 临时口令尚未更换的账号 —— 除了改密接口，其余一律 403。
pub struct AuthUser(pub User);

impl FromRequest for AuthUser {
    type Error = AppError;
    type Future = AuthFuture<Self>;

    fn from_request(request: &HttpRequest, _payload: &mut actix_web::dev::Payload) -> Self::Future {
        let state = request.app_data::<web::Data<AppState>>().cloned();
        let presented = read(request);

        Box::pin(async move {
            let (Some(state), Some(presented)) = (state, presented) else {
                return Err(AppError::Unauthorized);
            };

            let user = state
                .auth
                .authenticate(&presented)
                .await?
                .ok_or(AppError::Unauthorized)?;

            if !user.is_active() {
                return Err(AppError::AccountDisabled);
            }
            // 临时口令只能用来改密码：否则管理员第一次登录后可以一直用文件里那串口令
            if user.must_change_password {
                return Err(AppError::PasswordChangeRequired);
            }

            Ok(AuthUser(user))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;
    #[test]
    fn reads_well_formed_tokens_only() {
        let good = token::mint();
        let with_cookie = TestRequest::default()
            .insert_header((
                actix_web::http::header::COOKIE,
                format!("{COOKIE_NAME}={good}"),
            ))
            .to_http_request();
        assert_eq!(read(&with_cookie).as_deref(), Some(good.as_str()));

        let tampered = TestRequest::default()
            .insert_header((
                actix_web::http::header::COOKIE,
                format!("{COOKIE_NAME}=abc"),
            ))
            .to_http_request();
        assert!(read(&tampered).is_none());

        let absent = TestRequest::default().to_http_request();
        assert!(read(&absent).is_none());
    }

    #[test]
    fn session_cookie_is_hard_to_read_and_scoped() {
        let cookie = cookie("abc", Duration::from_secs(3600), true);

        assert_eq!(cookie.name(), COOKIE_NAME);
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
        assert_eq!(cookie.max_age(), Some(time::Duration::hours(1)));
    }

    #[test]
    fn plain_http_cookie_is_not_secure() {
        // 本地 http 调试加上 Secure 会让浏览器不回传，登录直接「不生效」
        assert_eq!(cookie("abc", Duration::from_secs(60), false).secure(), None);
    }

    #[test]
    fn clearing_cookie_expires_immediately() {
        let cookie = clearing_cookie(false);

        assert_eq!(cookie.value(), "");
        assert_eq!(cookie.max_age(), Some(time::Duration::ZERO));
        assert_eq!(cookie.path(), Some("/"));
    }
}
