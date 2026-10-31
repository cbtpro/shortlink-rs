//! 接口层：HTTP 协议适配。只做参数解析、调用用例、序列化响应。

pub mod dto;
pub mod visitor;

mod admin;
mod assets;
mod history;
mod interstitial;
mod redirect;
mod shorten;

use actix_web::web;

/// 注册全部路由。`/{code}` 是兜底路由，必须最后注册。
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(assets::home_page);
    cfg.service(assets::admin_page);
    cfg.service(assets::asset);
    cfg.service(assets::favicon);

    cfg.service(shorten::create_link);
    cfg.service(history::list);
    cfg.service(history::forget);
    cfg.service(history::clear);

    cfg.service(admin::overview);
    cfg.service(admin::list);
    cfg.service(admin::delete_link);

    cfg.service(redirect::redirect);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use actix_web::http::{header, StatusCode};
    use actix_web::{test, web, App};

    use super::configure;

    use crate::cache::{LinkCache, VisitCounter};
    use crate::domain::code::RandomCodeGenerator;
    use crate::repository::{CreationHistory, LinkRepository};
    use crate::service::admin::AdminService;
    use crate::service::history::HistoryService;
    use crate::service::redirect::RedirectService;
    use crate::service::shorten::ShortenService;
    use crate::state::AppState;
    use crate::test_support::{InMemoryLinkRepository, NoOpCache, NoOpCounter, RecordingCache, RecordingCounter};

    const BASE_URL: &str = "http://short.test";
    const ADMIN_TOKEN: &str = "test-admin-token";

    /// 组装一个不依赖 MySQL / Redis 的应用实例。
    fn state_with(counter: Arc<dyn VisitCounter>, admin_token: Option<&str>) -> web::Data<AppState> {        state_with_cache(counter, admin_token, Arc::new(NoOpCache))
    }

    fn state_with_cache(
        counter: Arc<dyn VisitCounter>,
        admin_token: Option<&str>,
        cache: Arc<dyn LinkCache>,
    ) -> web::Data<AppState> {
        let store = Arc::new(InMemoryLinkRepository::new());
        let repository: Arc<dyn LinkRepository> = store.clone();
        let creations: Arc<dyn CreationHistory> = store;

        let shorten = Arc::new(ShortenService::new(
            repository.clone(),
            Arc::new(RandomCodeGenerator),
            7,
            10,
            BASE_URL,
        ));
        let redirect = Arc::new(RedirectService::new(
            repository.clone(),
            cache.clone(),
            counter,
            Duration::from_secs(60),
        ));
        let admin = Arc::new(AdminService::new(
            repository,
            cache,
            admin_token.map(str::to_string),
        ));
        let history = Arc::new(HistoryService::new(creations, 20));

        web::Data::new(AppState {
            shorten,
            redirect,
            admin,
            history,
            cookie_secure: false,
        })
    }

    fn state() -> web::Data<AppState> {
        state_with(Arc::new(NoOpCounter), Some(ADMIN_TOKEN))
    }

    /// 发送 POST /shorten。
    ///
    /// 用宏而不是函数，避免在签名里写出 `test::call_service` 要求的服务类型。
    macro_rules! post_shorten {
        ($app:expr, $payload:expr) => {{
            let request = test::TestRequest::post()
                .uri("/shorten")
                .set_json($payload)
                .to_request();
            test::call_service($app, request).await
        }};
    }

    /// 带访客 cookie 的 POST /shorten。
    macro_rules! post_shorten_as {
        ($app:expr, $cookie:expr, $payload:expr) => {{
            let request = test::TestRequest::post()
                .uri("/shorten")
                .insert_header((header::COOKIE, $cookie.to_string()))
                .set_json($payload)
                .to_request();
            test::call_service($app, request).await
        }};
    }

    fn get(uri: &str) -> test::TestRequest {
        test::TestRequest::get().uri(uri)
    }

    #[actix_web::test]
    async fn creates_link_then_redirects() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/x", "custom_code": "promo" })
        );

        assert_eq!(response.status(), StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["code"], "promo");
        assert_eq!(body["short_url"], "http://short.test/promo");
        assert_eq!(body["long_url"], "https://example.com/x");
        assert_eq!(body["created_at"].as_str().map(str::len), Some(19));

        let response = test::call_service(&app, get("/promo").to_request()).await;
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .expect("Location header"),
            "https://example.com/x"
        );
    }

    #[actix_web::test]
    async fn generates_code_when_custom_code_is_absent() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(&app, serde_json::json!({ "url": "https://example.com" }));

        assert_eq!(response.status(), StatusCode::CREATED);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["code"].as_str().map(str::len), Some(7));
    }

    #[actix_web::test]
    async fn rejects_invalid_url_with_400() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(&app, serde_json::json!({ "url": "javascript:alert(1)" }));

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["code"], 400, "响应体 code 必须与状态码一致");
        assert!(body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("http"));
    }

    #[actix_web::test]
    async fn reports_unknown_code_with_404() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/nope").to_request()).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn guard_password_with_403() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "custom_code": "prot", "password": "s3cret" })
        );

        let denied = test::call_service(&app, get("/prot").to_request()).await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let wrong = test::call_service(&app, get("/prot?password=nope").to_request()).await;
        assert_eq!(wrong.status(), StatusCode::FORBIDDEN);

        let allowed = test::call_service(&app, get("/prot?password=s3cret").to_request()).await;
        assert_eq!(allowed.status(), StatusCode::FOUND);
    }

    #[actix_web::test]
    async fn interstitial_page_stands_between_visitor_and_target() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/landing?a=1&b=2",
                "custom_code": "gate",
                "interstitial": true
            })
        );

        let response = test::call_service(&app, get("/gate").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );

        let body = test::read_body(response).await;
        let html = std::str::from_utf8(&body).expect("中转页应为 utf-8");
        assert!(html.contains("example.com"), "页面要显示目标站点");
        assert!(
            html.contains("https://example.com/landing?a=1&amp;b=2"),
            "目标地址转义后才进 href"
        );
        assert!(!html.contains("{{"), "占位符必须全部替换");
    }

    #[actix_web::test]
    async fn password_gate_comes_before_the_interstitial() {
        // 中转页不是绕过密码的理由
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/secret",
                "custom_code": "vault",
                "password": "s3cret",
                "interstitial": true
            })
        );

        let denied = test::call_service(&app, get("/vault").to_request()).await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let allowed = test::call_service(&app, get("/vault?password=s3cret").to_request()).await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn rejects_expired_link_with_410() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "custom_code": "old", "expire_at": "2000-01-01T00:00:00" })
        );

        let response = test::call_service(&app, get("/old").to_request()).await;
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[actix_web::test]
    async fn stops_after_max_visits_with_410() {
        // 用 RecordingCounter 让「未回刷增量」真实生效，库里的 visit_count 仍是 0
        let app = test::init_service(
            App::new()
                .app_data(state_with(Arc::new(RecordingCounter::new()), Some(ADMIN_TOKEN)))
                .configure(configure),
        )
        .await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "custom_code": "once", "max_visits": 1 })
        );

        let first = test::call_service(&app, get("/once").to_request()).await;
        assert_eq!(first.status(), StatusCode::FOUND);

        let second = test::call_service(&app, get("/once").to_request()).await;
        assert_eq!(second.status(), StatusCode::GONE);
    }

    #[actix_web::test]
    async fn rejects_duplicate_custom_code_with_409() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let payload = serde_json::json!({ "url": "https://example.com", "custom_code": "dup" });
        assert_eq!(post_shorten!(&app, payload.clone()).status(), StatusCode::CREATED);
        assert_eq!(post_shorten!(&app, payload).status(), StatusCode::CONFLICT);
    }

    fn content_type(response: &actix_web::dev::ServiceResponse) -> Option<&str> {
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
    }

    fn admin_get(uri: &str) -> actix_web::test::TestRequest {
        test::TestRequest::get()
            .uri(uri)
            .insert_header((header::AUTHORIZATION, format!("Bearer {ADMIN_TOKEN}")))
    }

    #[actix_web::test]
    async fn admin_page_is_served() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/admin").to_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));
    }

    #[actix_web::test]
    async fn admin_page_wins_over_the_catch_all_route() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/admin").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body = test::read_body(response).await;
        let html = std::str::from_utf8(&body).expect("后台页应为 utf-8");
        assert!(html.contains("admin.js"), "返回的应是后台页面");
    }

    #[actix_web::test]
    async fn admin_api_rejects_missing_or_wrong_token() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let missing = test::call_service(&app, get("/admin/api/links").to_request()).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/admin/api/links")
                .insert_header((header::AUTHORIZATION, "Bearer nope"))
                .to_request(),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        // 不是 Bearer 的方案同样不放行
        let basic = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/admin/api/links")
                .insert_header((header::AUTHORIZATION, "Basic dGVzdA=="))
                .to_request(),
        )
        .await;
        assert_eq!(basic.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn admin_api_is_404_when_disabled() {
        let app = test::init_service(
            App::new()
                .app_data(state_with(Arc::new(NoOpCounter), None))
                .configure(configure),
        )
        .await;

        let response = test::call_service(&app, admin_get("/admin/api/links").to_request()).await;

        // 未配置 ADMIN_TOKEN：连「存在后台」都不该暴露
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn admin_lists_creates_and_deletes() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/a", "custom_code": "keep" })
        );
        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/b", "custom_code": "drop" })
        );

        let listed = test::call_service(&app, admin_get("/admin/api/links").to_request()).await;
        assert_eq!(listed.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(listed).await;
        assert_eq!(body["total"], 2);
        assert_eq!(body["items"].as_array().map(Vec::len), Some(2));
        assert_eq!(body["page"], 1);
        assert!(body["items"][0]["state"] == "active");

        let deleted = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/admin/api/links/drop")
                .insert_header((header::AUTHORIZATION, format!("Bearer {ADMIN_TOKEN}")))
                .to_request(),
        )
        .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

        let after: serde_json::Value = test::read_body_json(
            test::call_service(&app, admin_get("/admin/api/links").to_request()).await,
        )
        .await;
        assert_eq!(after["total"], 1);
        assert_eq!(after["items"][0]["code"], "keep");

        // 删除后跳转应该 404，而不是继续 302
        let redirect = test::call_service(&app, get("/drop").to_request()).await;
        assert_eq!(redirect.status(), StatusCode::NOT_FOUND);
    }

    /// 创建接口下发的访客 cookie，取 `name=value` 部分。
    fn visitor_cookie(response: &actix_web::dev::ServiceResponse) -> String {
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .expect("创建接口应下发访客 cookie")
            .to_string()
    }

    fn recent_get(uri: &str, cookie: &str) -> actix_web::test::TestRequest {
        test::TestRequest::get()
            .uri(uri)
            .insert_header((header::COOKIE, cookie.to_string()))
    }

    fn recent_delete(uri: &str, cookie: &str) -> actix_web::test::TestRequest {
        test::TestRequest::delete()
            .uri(uri)
            .insert_header((header::COOKIE, cookie.to_string()))
    }

    #[actix_web::test]
    async fn creation_shows_up_in_recent() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let created = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/r", "custom_code": "recent1" })
        );
        assert_eq!(created.status(), StatusCode::CREATED);
        let cookie = visitor_cookie(&created);

        let listed = test::call_service(&app, recent_get("/recent", &cookie).to_request()).await;
        assert_eq!(listed.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(listed).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["items"][0]["code"], "recent1");
        assert_eq!(body["items"][0]["short_url"], "http://short.test/recent1");
        assert_eq!(body["items"][0]["has_password"], false);
        assert_eq!(body["items"][0]["interstitial"], false);
        assert_eq!(body["items"][0]["created_at"].as_str().map(str::len), Some(19));
    }

    #[actix_web::test]
    async fn recent_issues_identity_to_first_time_visitors() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/recent").to_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::SET_COOKIE).is_some(),
            "首次访问应签发访客 cookie"
        );
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0));
    }

    #[actix_web::test]
    async fn recent_does_not_leak_across_visitors() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let first = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/a", "custom_code": "mine" })
        ));
        let second = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/b", "custom_code": "yours" })
        ));
        assert_ne!(first, second);

        let body: serde_json::Value = test::read_body_json(
            test::call_service(&app, recent_get("/recent", &second).to_request()).await,
        )
        .await;

        assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["items"][0]["code"], "yours", "不该看到别人的短链");
    }

    #[actix_web::test]
    async fn visitor_can_forget_single_entry_and_clear_all() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let cookie = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/a", "custom_code": "keep" })
        ));
        // 第二次必须复用同一个 cookie，否则会被当成另一位访客
        post_shorten_as!(
            &app,
            cookie,
            serde_json::json!({ "url": "https://example.com/b", "custom_code": "drop" })
        );

        let removed = test::call_service(
            &app,
            recent_delete("/recent/drop", &cookie).to_request(),
        )
        .await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        let repeated = test::call_service(
            &app,
            recent_delete("/recent/drop", &cookie).to_request(),
        )
        .await;
        assert_eq!(repeated.status(), StatusCode::NOT_FOUND, "重复删除应 404");

        let cleared =
            test::call_service(&app, recent_delete("/recent", &cookie).to_request()).await;
        assert_eq!(cleared.status(), StatusCode::NO_CONTENT);

        let body: serde_json::Value = test::read_body_json(
            test::call_service(&app, recent_get("/recent", &cookie).to_request()).await,
        )
        .await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0));
    }

    #[actix_web::test]
    async fn recent_survives_the_cookie_but_not_the_link() {
        // 后台删掉短链后，历史里那条不能继续指向 404
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let cookie = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/x", "custom_code": "doomed" })
        ));

        test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/admin/api/links/doomed")
                .insert_header((header::AUTHORIZATION, format!("Bearer {ADMIN_TOKEN}")))
                .to_request(),
        )
        .await;

        let body: serde_json::Value = test::read_body_json(
            test::call_service(&app, recent_get("/recent", &cookie).to_request()).await,
        )
        .await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(0));
    }

    #[actix_web::test]
    async fn admin_delete_unknown_code_is_404() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/admin/api/links/ghost")
                .insert_header((header::AUTHORIZATION, format!("Bearer {ADMIN_TOKEN}")))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn admin_delete_evicts_cache() {
        let cache = Arc::new(RecordingCache::new());
        cache.preload(
            "cached",
            crate::domain::short_link::CachedLink {
                id: 1,
                long_url: "https://example.com/cached".into(),
                expire_at: None,
                max_visits: None,
                visit_count: 0,
                interstitial: false,
            },
        );

        let app = test::init_service(
            App::new()
                .app_data(state_with_cache(Arc::new(NoOpCounter), Some(ADMIN_TOKEN), cache.clone()))
                .configure(configure),
        )
        .await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/cached", "custom_code": "cached" })
        );

        test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/admin/api/links/cached")
                .insert_header((header::AUTHORIZATION, format!("Bearer {ADMIN_TOKEN}")))
                .to_request(),
        )
        .await;

        assert!(!cache.contains("cached"), "删链后必须清缓存");
    }

    #[actix_web::test]
    async fn admin_overview_reports_totals() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "custom_code": "one" })
        );
        // 访问两次，计数写进 Redis 替身，不影响库里的 visit_count
        test::call_service(&app, get("/one").to_request()).await;
        test::call_service(&app, get("/one").to_request()).await;

        let response = test::call_service(&app, admin_get("/admin/api/overview").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["total_links"], 1);
        assert_eq!(body["created_today"], 1);
        assert_eq!(body["total_visits"], 0, "计数尚未回刷，概览只反映库里的事实");
    }

    #[actix_web::test]
    async fn serves_index_at_root() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/").to_request()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));

        let body = test::read_body(response).await;
        let html = std::str::from_utf8(&body).expect("首页应为 utf-8");
        assert!(html.contains("<title>"), "首页内容缺失");
    }

    #[actix_web::test]
    async fn serves_assets_instead_of_treating_them_as_codes() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        for (uri, expected) in [
            ("/assets/css/base.css", "text/css; charset=utf-8"),
            ("/assets/js/pages/home.js", "text/javascript; charset=utf-8"),
        ] {
            let response = test::call_service(&app, get(uri).to_request()).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri} 被兜底路由截获了");
            assert_eq!(content_type(&response), Some(expected), "{uri}");
        }
    }

    #[actix_web::test]
    async fn unknown_asset_is_404() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/assets/js/nope.js").to_request()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn favicon_returns_no_content() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/favicon.ico").to_request()).await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
