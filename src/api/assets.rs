//! 页面与静态资源路由。只做 HTTP 细节，内容一律来自 `crate::assets`。

use actix_web::http::header;
use actix_web::{get, web, HttpRequest, HttpResponse};

use crate::assets::Asset;
use crate::state::AppState;

const NO_STORE: &str = "no-store";
const REVALIDATE: &str = "no-cache";

#[get("/")]
pub async fn home_page(state: web::Data<AppState>) -> HttpResponse {
    page(&state, "index.html")
}

#[get("/admin")]
pub async fn admin_page(state: web::Data<AppState>) -> HttpResponse {
    page(&state, "admin.html")
}

/// 登录与注册共用一个页面：两者字段几乎一样，拆成两页只会让状态更碎。
#[get("/login")]
pub async fn login_page(state: web::Data<AppState>) -> HttpResponse {
    page(&state, "login.html")
}

/// 前端静态资源。找不到就 404，不会漏到 `/{code}` 上查库。
#[get("/assets/{path:.*}")]
pub async fn asset_file(
    state: web::Data<AppState>,
    path: web::Path<String>,
    request: HttpRequest,
) -> HttpResponse {
    // URL 的 `/assets/` 对应 `web/assets/`，资源键始终相对 `web/` 根
    let key = format!("assets/{}", path.into_inner());

    match state.assets.get(&key) {
        Some(file) => respond(&request, file),
        None => HttpResponse::NotFound().finish(),
    }
}

/// 浏览器会默认请求 favicon，显式 204，别让它落到兜底路由上白查一次库。
#[get("/favicon.ico")]
pub async fn favicon() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

fn page(state: &AppState, name: &str) -> HttpResponse {
    match state.assets.page(name) {
        Some(html) => HttpResponse::Ok()
            .insert_header((header::CACHE_CONTROL, cache_control(state)))
            .content_type("text/html; charset=utf-8")
            .body(html),
        // 启动时已校验页面齐全，走到这里说明资源被运行时删了
        None => {
            log::error!("page `{name}` is missing from the asset source");
            HttpResponse::InternalServerError().finish()
        }
    }
}

fn respond(request: &HttpRequest, file: Asset) -> HttpResponse {
    if is_fresh(request, &file.etag) {
        return HttpResponse::NotModified()
            .insert_header((header::ETAG, file.etag))
            .insert_header((header::CACHE_CONTROL, REVALIDATE))
            .finish();
    }

    HttpResponse::Ok()
        .insert_header((header::ETAG, file.etag))
        // 资源名不带指纹，必须让浏览器回源校验，否则改完会看到旧样式
        .insert_header((header::CACHE_CONTROL, REVALIDATE))
        .content_type(file.content_type)
        .body(file.body)
}

fn cache_control(state: &AppState) -> &'static str {
    if state.assets.live_reload() {
        NO_STORE
    } else {
        REVALIDATE
    }
}

fn is_fresh(request: &HttpRequest, etag: &str) -> bool {
    request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_etag_from_header_list() {
        let request = actix_web::test::TestRequest::default()
            .insert_header((header::IF_NONE_MATCH, "\"a\", \"b\""))
            .to_http_request();

        assert!(is_fresh(&request, "\"b\""));
        assert!(!is_fresh(&request, "\"c\""));
    }

    #[test]
    fn missing_etag_is_never_fresh() {
        let request = actix_web::test::TestRequest::default().to_http_request();
        assert!(!is_fresh(&request, "\"a\""));
    }
}
