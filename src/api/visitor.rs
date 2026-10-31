//! 匿名访客身份。
//!
//! 服务没有账号体系，「最近创建」只能按浏览器区分。这里签发一个随机 id 放进
//! HttpOnly cookie：前端读不到，也就没法伪造别人的 id 去翻别人的历史。

use actix_web::cookie::{time, Cookie, SameSite};
use actix_web::HttpRequest;
use rand::Rng;

pub const COOKIE_NAME: &str = "sl_visitor";

/// 32 位十六进制，128 位熵，碰撞可以忽略。
const ID_LEN: usize = 32;
/// cookie 有效期。历史只是便利功能，一年足够覆盖「过几周回来再找」的场景。
const COOKIE_DAYS: i64 = 365;

/// 从请求里读访客 id。缺失或格式不合法都当作「没有」，
/// 避免任意字符串被当成 id 写进库。
pub fn read(request: &HttpRequest) -> Option<String> {
    let cookie = request.cookie(COOKIE_NAME)?;
    let value = cookie.value();
    is_well_formed(value).then(|| value.to_string())
}

/// 取访客 id；没有合法 cookie 时新签一个，并返回要写回的 Set-Cookie。
pub fn ensure(request: &HttpRequest, secure: bool) -> (String, Option<Cookie<'static>>) {
    match read(request) {
        Some(id) => (id, None),
        None => {
            let id = mint();
            let cookie = cookie_for(&id, secure);
            (id, Some(cookie))
        }
    }
}

pub fn mint() -> String {
    let mut rng = rand::rng();
    (0..ID_LEN)
        .map(|_| char::from_digit(rng.random_range(0..16), 16).unwrap_or('0'))
        .collect()
}

fn cookie_for(id: &str, secure: bool) -> Cookie<'static> {
    let mut builder = Cookie::build(COOKIE_NAME, id.to_string())
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time::Duration::days(COOKIE_DAYS));

    if secure {
        builder = builder.secure(true);
    }

    builder.finish()
}

fn is_well_formed(value: &str) -> bool {
    value.len() == ID_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_lowercase_hex_of_fixed_length() {
        let id = mint();
        assert_eq!(id.len(), ID_LEN);
        assert!(is_well_formed(&id));
        assert_ne!(id, mint());
    }

    #[test]
    fn rejects_tampered_values() {
        assert!(!is_well_formed(""));
        assert!(!is_well_formed("0123456789ABCDEF0123456789abcdef"), "大写不接受");
        assert!(!is_well_formed("0123456789abcdef0123456789abcde"), "长度不足");
        assert!(!is_well_formed("0123456789abcdef0123456789abcdeg"), "非十六进制");
    }

    #[test]
    fn cookie_is_scoped_and_hard_to_read() {
        let cookie = cookie_for("0123456789abcdef0123456789abcdef", true);
        assert_eq!(cookie.path(), Some("/"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Lax));
    }

    #[test]
    fn plain_http_cookie_is_not_secure() {
        // 本地 http 调试时带上 Secure 会导致浏览器不回传，历史功能直接失效
        let cookie = cookie_for("0123456789abcdef0123456789abcdef", false);
        assert_eq!(cookie.secure(), None);
    }
}
