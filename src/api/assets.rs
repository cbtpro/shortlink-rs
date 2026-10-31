//! 内嵌的前端资源，改完 web/ 下的文件需要重新构建。

use actix_web::{get, web, HttpResponse};
use actix_web::http::header;

const HTML: &str = "text/html; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";

const INDEX_HTML: &str = include_str!("../../web/index.html");
const ADMIN_HTML: &str = include_str!("../../web/admin.html");
/// 中转页模板，由 `api::interstitial` 填占位符后再返回。
pub(super) const INTERSTITIAL_HTML: &str = include_str!("../../web/interstitial.html");

/// 相对 `/assets/` 的路径、Content-Type、内容。加前端文件只需在这里补一行。
const ASSETS: &[(&str, &str, &str)] = &[
    ("css/base.css", CSS, include_str!("../../web/assets/css/base.css")),
    ("css/home.css", CSS, include_str!("../../web/assets/css/home.css")),
    ("css/admin.css", CSS, include_str!("../../web/assets/css/admin.css")),
    (
        "css/interstitial.css",
        CSS,
        include_str!("../../web/assets/css/interstitial.css"),
    ),
    ("js/core/dom.js", JAVASCRIPT, include_str!("../../web/assets/js/core/dom.js")),
    (
        "js/core/api-base.js",
        JAVASCRIPT,
        include_str!("../../web/assets/js/core/api-base.js"),
    ),
    ("js/core/http.js", JAVASCRIPT, include_str!("../../web/assets/js/core/http.js")),
    ("js/core/format.js", JAVASCRIPT, include_str!("../../web/assets/js/core/format.js")),
    ("js/core/toast.js", JAVASCRIPT, include_str!("../../web/assets/js/core/toast.js")),
    ("js/core/clipboard.js", JAVASCRIPT, include_str!("../../web/assets/js/core/clipboard.js")),
    (
        "js/features/admin-api.js",
        JAVASCRIPT,
        include_str!("../../web/assets/js/features/admin-api.js"),
    ),
    (
        "js/features/recent-links.js",
        JAVASCRIPT,
        include_str!("../../web/assets/js/features/recent-links.js"),
    ),
    ("js/pages/home.js", JAVASCRIPT, include_str!("../../web/assets/js/pages/home.js")),
    ("js/pages/admin.js", JAVASCRIPT, include_str!("../../web/assets/js/pages/admin.js")),
];

#[get("/")]
pub async fn home_page() -> HttpResponse {
    html(INDEX_HTML)
}

#[get("/admin")]
pub async fn admin_page() -> HttpResponse {
    html(ADMIN_HTML)
}

/// 前端静态资源。找不到就 404，不会漏到 `/{code}` 上查库。
#[get("/assets/{path:.*}")]
pub async fn asset(path: web::Path<String>) -> HttpResponse {
    let path = path.into_inner();
    match ASSETS.iter().find(|(name, _, _)| *name == path.as_str()) {
        Some((_, content_type, body)) => respond(body, content_type),
        None => HttpResponse::NotFound().finish(),
    }
}

/// 浏览器会默认请求 favicon，显式 204，别让它落到兜底路由上白查一次库。
#[get("/favicon.ico")]
pub async fn favicon() -> HttpResponse {
    HttpResponse::NoContent().finish()
}

fn html(body: &'static str) -> HttpResponse {
    respond(body, HTML)
}

fn respond(body: &'static str, content_type: &'static str) -> HttpResponse {
    HttpResponse::Ok()
        // 资源名不带指纹，构建后必须让浏览器回源校验，否则改完会看到旧样式
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .content_type(content_type)
        .body(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资源必须真的打进二进制，不能是运行时读文件。
    #[test]
    fn bundles_non_empty_assets() {
        assert!(INDEX_HTML.contains("<title>"));
        assert!(ADMIN_HTML.contains("<title>"));
        assert!(INTERSTITIAL_HTML.contains("<title>"));

        for (name, content_type, body) in ASSETS {
            assert!(!body.trim().is_empty(), "{name} 内容为空");
            assert!(
                *content_type == CSS || *content_type == JAVASCRIPT,
                "{name} 的类型不对"
            );
        }
    }

    #[test]
    fn asset_paths_are_unique() {
        for (position, (name, _, _)) in ASSETS.iter().enumerate() {
            assert!(
                !ASSETS[position + 1..].iter().any(|(other, _, _)| other == name),
                "{name} 重复注册"
            );
        }
    }

    /// 页面里引用的每个 `assets/...` 都必须有对应的路由，否则会静默 404。
    #[test]
    fn pages_reference_served_assets() {
        for page in [INDEX_HTML, ADMIN_HTML, INTERSTITIAL_HTML] {
            for path in referenced_assets(page) {
                let key = path.trim_start_matches("assets/");
                assert!(
                    ASSETS.iter().any(|(name, _, _)| *name == key),
                    "页面引用了未注册的资源：{path}"
                );
            }
        }
    }

    fn referenced_assets(html: &'static str) -> Vec<&'static str> {
        html.match_indices("assets/")
            .filter_map(|(start, _)| {
                let rest = &html[start..];
                let end = rest.find(|c: char| c == '"' || c == '?')?;
                Some(&rest[..end])
            })
            .collect()
    }
}
