//! 接口层：HTTP 协议适配。只做参数解析、调用用例、序列化响应。

pub mod dto;
pub mod session;
pub mod visitor;

mod admin;
mod app_config;
mod assets;
mod auth;
mod dev;
mod health;
mod history;
mod interstitial;
mod redirect;
mod shorten;

use actix_web::web;

use crate::error::AppError;

/// 注册全部路由。`/{code}` 是兜底路由，必须最后注册。
pub fn configure(cfg: &mut web::ServiceConfig) {
    // 请求体解析失败也要返回统一的 JSON 错误体。
    //
    // 默认行为是 actix 自己的纯文本 400，前端 `response.json()` 解析失败后只能显示
    // 「请求失败（HTTP 400）」；更糟的是解析器的原话里带着 Rust 类型名。
    cfg.app_data(web::JsonConfig::default().error_handler(|error, _| {
        log::debug!("rejected request body: {error}");
        AppError::InvalidBody.into()
    }));

    // 页面与资源
    cfg.service(assets::home_page);
    cfg.service(assets::admin_page);
    cfg.service(assets::login_page);
    cfg.service(assets::asset_file);
    cfg.service(assets::favicon);
    cfg.service(app_config::app_config);

    // 运维端点
    cfg.service(health::healthz);
    cfg.service(health::readyz);

    // 开发期热刷新。未开启时这两个 handler 自己返回 404
    cfg.service(dev::revision);
    cfg.service(dev::live_reload_script);

    cfg.service(auth::register);
    cfg.service(auth::login);
    cfg.service(auth::logout);
    cfg.service(auth::me);

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
    use chrono::Utc;

    use super::configure;

    use crate::cache::{LinkCache, VisitCounter};
    use crate::domain::code::RandomCodeGenerator;
    use crate::repository::{CreationHistory, LinkRepository, SessionRepository, UserRepository};
    use crate::service::admin::AdminService;
    use crate::service::auth::AuthService;
    use crate::service::history::HistoryService;
    use crate::service::my_links::MyLinksService;
    use crate::service::redirect::RedirectService;
    use crate::service::shorten::ShortenService;
    use crate::settings::{AssetsMode, Profile};
    use crate::state::AppState;
    use crate::test_support::{
        InMemoryLinkRepository, NoOpCache, NoOpCounter, PlainHasher, RecordingCache,
        RecordingCounter,
    };

    const BASE_URL: &str = "http://short.test";
    const ADMIN_TOKEN: &str = "test-admin-token";

    /// 组装一个不依赖 MySQL / Redis 的应用实例。
    fn state_with(
        counter: Arc<dyn VisitCounter>,
        admin_token: Option<&str>,
    ) -> web::Data<AppState> {
        state_with_cache(counter, admin_token, Arc::new(NoOpCache))
    }

    fn state_with_cache(
        counter: Arc<dyn VisitCounter>,
        admin_token: Option<&str>,
        cache: Arc<dyn LinkCache>,
    ) -> web::Data<AppState> {
        build_state(counter, admin_token, cache, test_assets())
    }

    fn build_state(
        counter: Arc<dyn VisitCounter>,
        admin_token: Option<&str>,
        cache: Arc<dyn LinkCache>,
        assets: Arc<crate::assets::AssetServer>,
    ) -> web::Data<AppState> {
        build_state_with(
            Arc::new(InMemoryLinkRepository::new()),
            counter,
            admin_token,
            cache,
            assets,
        )
    }

    /// 同一个组合根，但仓储由调用方给 —— 需要预置数据（比如「已过期的链接」）时用。
    fn build_state_with(
        store: Arc<InMemoryLinkRepository>,
        counter: Arc<dyn VisitCounter>,
        admin_token: Option<&str>,
        cache: Arc<dyn LinkCache>,
        assets: Arc<crate::assets::AssetServer>,
    ) -> web::Data<AppState> {
        let repository: Arc<dyn LinkRepository> = store.clone();
        let creations: Arc<dyn CreationHistory> = store.clone();
        let users: Arc<dyn UserRepository> = store.clone();
        let sessions: Arc<dyn SessionRepository> = store;

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
            repository.clone(),
            cache.clone(),
            admin_token.map(str::to_string),
        ));
        let history = Arc::new(HistoryService::new(creations, 20));
        let my_links = Arc::new(MyLinksService::new(repository, cache, 20));
        // 测试里用不做散列的替身：PBKDF2 自己的正确性另有专门用例
        let auth = Arc::new(AuthService::new(
            users,
            sessions,
            Arc::new(PlainHasher),
            Duration::from_secs(30 * 24 * 60 * 60),
        ));

        web::Data::new(AppState {
            shorten,
            redirect,
            admin,
            auth,
            my_links,
            history,
            assets,
            readiness: None,
            profile: Profile::Test,
            site_name: "shortlink".to_string(),
            api_base: String::new(),
            cookie_secure: false,
        })
    }

    fn test_assets() -> Arc<crate::assets::AssetServer> {
        Arc::new(
            crate::assets::AssetServer::new(
                AssetsMode::Embedded,
                std::path::PathBuf::from("web"),
                false,
            )
            .expect("内嵌资源必须可用"),
        )
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

    /// 读出响应体并解析成 JSON。
    async fn body_json<B>(response: actix_web::dev::ServiceResponse<B>) -> serde_json::Value
    where
        B: actix_web::body::MessageBody,
        B::Error: std::fmt::Debug,
    {
        test::read_body_json(response).await
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
        // 直接往仓储里塞一条已过期的链接：创建接口现在会拒绝过去的有效期，
        // 但「建好之后才过期」的链接仍要走 410 这条路。
        let store = Arc::new(InMemoryLinkRepository::new());
        store
            .insert(
                &crate::domain::new_link::NewShortLink {
                    url: "https://example.com".into(),
                    expire_at: Some(Utc::now().naive_utc() - chrono::Duration::days(1)),
                    ..Default::default()
                },
                "old",
            )
            .await
            .expect("seed insert should succeed");

        let app = test::init_service(
            App::new()
                .app_data(build_state_with(
                    store,
                    Arc::new(NoOpCounter),
                    Some(ADMIN_TOKEN),
                    Arc::new(NoOpCache),
                    test_assets(),
                ))
                .configure(configure),
        )
        .await;

        let response = test::call_service(&app, get("/old").to_request()).await;
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[actix_web::test]
    async fn rejects_expiry_before_now_with_400() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "expire_at": "2000-01-01T00:00:00" })
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["message"],
            "expire_at must be in the future"
        );
    }

    #[actix_web::test]
    async fn rejects_negative_max_visits_with_400() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "max_visits": -5 })
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["message"],
            "max_visits must be between 1 and 100000000"
        );
    }

    #[actix_web::test]
    async fn rejects_password_with_space_with_400() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "password": "a b" })
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["message"],
            "password must not contain whitespace"
        );
    }

    #[actix_web::test]
    async fn rejects_unparsable_body_with_the_same_error_shape() {
        // 类型不对（max_visits 传字符串）走的是反序列化失败，
        // 但客户端拿到的必须是和业务错误一样的 JSON 结构
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "max_visits": "many" })
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["code"], 400);
        assert_eq!(body["message"], "request body is not valid");
    }

    #[actix_web::test]
    async fn stops_after_max_visits_with_410() {
        // 用 RecordingCounter 让「未回刷增量」真实生效，库里的 visit_count 仍是 0
        let app = test::init_service(
            App::new()
                .app_data(state_with(
                    Arc::new(RecordingCounter::new()),
                    Some(ADMIN_TOKEN),
                ))
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
        assert_eq!(
            post_shorten!(&app, payload.clone()).status(),
            StatusCode::CREATED
        );
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
        assert_eq!(
            body["items"][0]["created_at"].as_str().map(str::len),
            Some(19)
        );
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

        let removed =
            test::call_service(&app, recent_delete("/recent/drop", &cookie).to_request()).await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        let repeated =
            test::call_service(&app, recent_delete("/recent/drop", &cookie).to_request()).await;
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
                .app_data(state_with_cache(
                    Arc::new(NoOpCounter),
                    Some(ADMIN_TOKEN),
                    cache.clone(),
                ))
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

        let response =
            test::call_service(&app, admin_get("/admin/api/overview").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["total_links"], 1);
        assert_eq!(body["created_today"], 1);
        assert_eq!(
            body["total_visits"], 0,
            "计数尚未回刷，概览只反映库里的事实"
        );
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

    #[actix_web::test]
    async fn reports_health_and_runtime_config() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let health = test::call_service(&app, get("/healthz").to_request()).await;
        assert_eq!(health.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(health).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["profile"], "test");
        assert_eq!(body["assets"], "embedded");

        // 测试替身没有真实依赖，不能谎报就绪
        let ready = test::call_service(&app, get("/readyz").to_request()).await;
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);

        let config = test::call_service(&app, get("/app-config.js").to_request()).await;
        assert_eq!(config.status(), StatusCode::OK);
        assert_eq!(
            content_type(&config),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(
            config
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );

        let script = String::from_utf8(test::read_body(config).await.to_vec()).unwrap();
        assert!(script.contains("window.__APP_CONFIG__"));
        assert!(script.contains("\"env\":\"test\""));
        assert!(script.contains("\"liveReload\":false"));
    }

    #[actix_web::test]
    async fn dev_endpoints_are_hidden_when_live_reload_is_off() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        for uri in ["/__dev/revision", "/__dev/live-reload.js"] {
            let response = test::call_service(&app, get(uri).to_request()).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{uri} 不该在生产环境暴露"
            );
        }
    }

    #[actix_web::test]
    async fn serves_live_reload_protocol_in_disk_mode() {
        let root = std::env::temp_dir().join(format!("shortlink-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("css")).unwrap();
        for page in [
            "index.html",
            "admin.html",
            "login.html",
            "interstitial.html",
        ] {
            std::fs::write(root.join(page), "<html><body>page</body></html>").unwrap();
        }
        std::fs::write(root.join("css/base.css"), "body{}").unwrap();

        let assets = Arc::new(
            crate::assets::AssetServer::new(AssetsMode::Disk, root.clone(), true)
                .expect("临时目录已备好页面"),
        );
        let app = test::init_service(
            App::new()
                .app_data(build_state(
                    Arc::new(NoOpCounter),
                    Some(ADMIN_TOKEN),
                    Arc::new(NoOpCache),
                    assets,
                ))
                .configure(configure),
        )
        .await;

        let page = test::call_service(&app, get("/").to_request()).await;
        assert_eq!(page.status(), StatusCode::OK);
        assert_eq!(
            page.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "开发模式页面不能被缓存，否则改完看不到"
        );
        let html = String::from_utf8(test::read_body(page).await.to_vec()).unwrap();
        assert!(
            html.contains("/__dev/live-reload.js"),
            "开发模式要注入刷新脚本"
        );

        let revision: serde_json::Value = test::read_body_json(
            test::call_service(&app, get("/__dev/revision").to_request()).await,
        )
        .await;
        let before = revision["revision"].as_str().unwrap().to_string();

        std::fs::write(root.join("css/base.css"), "body{color:red}").unwrap();
        let after: serde_json::Value = test::read_body_json(
            test::call_service(&app, get("/__dev/revision").to_request()).await,
        )
        .await;
        assert_ne!(
            after["revision"].as_str().unwrap(),
            before,
            "改文件后版本号必须变化"
        );

        let script = test::call_service(&app, get("/__dev/live-reload.js").to_request()).await;
        assert_eq!(script.status(), StatusCode::OK);

        std::fs::remove_dir_all(&root).ok();
    }
    /* ---------- 账号与会话 ---------- */

    /// 发起注册，返回的响应里带会话 cookie。
    macro_rules! register {
        ($app:expr, $email:expr) => {{
            let request = test::TestRequest::post()
                .uri("/auth/register")
                .set_json(serde_json::json!({
                    "email": $email,
                    "password": "secret123",
                }))
                .to_request();
            test::call_service($app, request).await
        }};
    }

    /// 从响应里取出会话 cookie 的 `name=value` 部分。
    fn session_cookie(response: &actix_web::dev::ServiceResponse) -> String {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("sl_session="))
            .and_then(|value| value.split(';').next())
            .expect("注册 / 登录接口应下发会话 cookie")
            .to_string()
    }

    fn with_cookie(method: &str, uri: &str, cookie: &str) -> test::TestRequest {
        let request = match method {
            "POST" => test::TestRequest::post(),
            "DELETE" => test::TestRequest::delete(),
            _ => test::TestRequest::get(),
        };

        request
            .uri(uri)
            .insert_header((header::COOKIE, cookie.to_string()))
    }

    #[actix_web::test]
    async fn register_creates_a_session_cookie() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = register!(&app, "Alice@Example.com");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(
            session_cookie(&response).starts_with("sl_session="),
            "cookie 名要能被前端与后续请求识别"
        );

        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["user"]["email"], "alice@example.com", "邮箱统一小写");
        assert_eq!(body["user"]["display_name"], "alice", "默认取邮箱前缀");
        assert!(
            body["user"]["password_hash"].is_null(),
            "散列绝不能出现在响应里"
        );
    }

    #[actix_web::test]
    async fn register_rejects_duplicate_email_with_409() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        assert_eq!(
            register!(&app, "dup@example.com").status(),
            StatusCode::CREATED
        );

        let again = register!(&app, "DUP@example.com");
        assert_eq!(again.status(), StatusCode::CONFLICT);
        let body: serde_json::Value = test::read_body_json(again).await;
        assert_eq!(body["message"], "email is already registered");
    }

    #[actix_web::test]
    async fn register_validates_email_and_password() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let bad_email = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/register")
                .set_json(serde_json::json!({ "email": "not-an-email", "password": "secret123" }))
                .to_request(),
        )
        .await;
        assert_eq!(bad_email.status(), StatusCode::BAD_REQUEST);

        let short = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/register")
                .set_json(serde_json::json!({ "email": "a@example.com", "password": "short" }))
                .to_request(),
        )
        .await;
        assert_eq!(short.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn login_accepts_the_right_password_and_rejects_the_rest() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;
        register!(&app, "bob@example.com");

        let ok = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(
                    serde_json::json!({ "email": "bob@example.com", "password": "secret123" }),
                )
                .to_request(),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        let cookie = session_cookie(&ok);

        let body: serde_json::Value = test::read_body_json(
            test::call_service(&app, with_cookie("GET", "/auth/me", &cookie).to_request()).await,
        )
        .await;
        assert_eq!(body["user"]["email"], "bob@example.com");

        for payload in [
            serde_json::json!({ "email": "bob@example.com", "password": "wrong-one" }),
            serde_json::json!({ "email": "nobody@example.com", "password": "secret123" }),
        ] {
            let denied = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/auth/login")
                    .set_json(payload)
                    .to_request(),
            )
            .await;
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[actix_web::test]
    async fn me_is_anonymous_for_strangers_and_logout_ends_the_session() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let anonymous: serde_json::Value =
            test::read_body_json(test::call_service(&app, get("/auth/me").to_request()).await)
                .await;
        assert!(anonymous["user"].is_null(), "未登录返回 null 而不是 401");

        let cookie = session_cookie(&register!(&app, "carol@example.com"));
        let response =
            test::call_service(&app, with_cookie("GET", "/auth/me", &cookie).to_request()).await;
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "随 cookie 变化的响应不能被缓存"
        );
        let logged_in: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(logged_in["user"]["email"], "carol@example.com");

        let logout = test::call_service(
            &app,
            with_cookie("POST", "/auth/logout", &cookie).to_request(),
        )
        .await;
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        assert!(
            logout
                .headers()
                .get_all(header::SET_COOKIE)
                .filter_map(|value| value.to_str().ok())
                .any(|value| value.starts_with("sl_session=;")),
            "注销要下发同名空 cookie"
        );

        let after: serde_json::Value = test::read_body_json(
            test::call_service(&app, with_cookie("GET", "/auth/me", &cookie).to_request()).await,
        )
        .await;
        assert!(after["user"].is_null(), "服务端会话必须同时失效");
    }

    #[actix_web::test]
    async fn created_links_are_owned_by_the_signed_in_user() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let alice = session_cookie(&register!(&app, "alice@example.com"));
        let bob = session_cookie(&register!(&app, "bob@example.com"));

        let created = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/shorten")
                .insert_header((header::COOKIE, alice.clone()))
                .set_json(serde_json::json!({
                    "url": "https://example.com/mine",
                    "custom_code": "owned1"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(created.status(), StatusCode::CREATED);
        assert!(
            created.headers().get(header::SET_COOKIE).is_none(),
            "登录用户不需要访客 cookie"
        );

        let body: serde_json::Value = test::read_body_json(
            test::call_service(&app, with_cookie("GET", "/recent", &alice).to_request()).await,
        )
        .await;
        assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["items"][0]["code"], "owned1");

        let others: serde_json::Value = test::read_body_json(
            test::call_service(&app, with_cookie("GET", "/recent", &bob).to_request()).await,
        )
        .await;
        assert!(
            others["items"].as_array().is_some_and(Vec::is_empty),
            "别人的链接不该出现在自己的列表里"
        );

        let anonymous: serde_json::Value =
            test::read_body_json(test::call_service(&app, get("/recent").to_request()).await).await;
        assert!(
            anonymous["items"].as_array().is_some_and(Vec::is_empty),
            "匿名访客的清单与账号归属无关"
        );
    }

    #[actix_web::test]
    async fn deleting_an_owned_link_takes_effect_immediately() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;
        let alice = session_cookie(&register!(&app, "alice@example.com"));

        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/shorten")
                .insert_header((header::COOKIE, alice.clone()))
                .set_json(
                    serde_json::json!({ "url": "https://example.com/x", "custom_code": "doomed" }),
                )
                .to_request(),
        )
        .await;

        let removed = test::call_service(
            &app,
            with_cookie("DELETE", "/recent/doomed", &alice).to_request(),
        )
        .await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        let redirect = test::call_service(&app, get("/doomed").to_request()).await;
        assert_eq!(
            redirect.status(),
            StatusCode::NOT_FOUND,
            "登录用户删的是短链本身，不是清单条目"
        );
    }

    #[actix_web::test]
    async fn users_cannot_delete_each_others_links() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;
        let alice = session_cookie(&register!(&app, "alice@example.com"));
        let bob = session_cookie(&register!(&app, "bob@example.com"));

        test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/shorten")
                .insert_header((header::COOKIE, alice))
                .set_json(
                    serde_json::json!({ "url": "https://example.com/x", "custom_code": "mine" }),
                )
                .to_request(),
        )
        .await;

        let denied = test::call_service(
            &app,
            with_cookie("DELETE", "/recent/mine", &bob).to_request(),
        )
        .await;
        assert_eq!(
            denied.status(),
            StatusCode::NOT_FOUND,
            "不能暴露别人的短码存在"
        );

        let still_there = test::call_service(&app, get("/mine").to_request()).await;
        assert_eq!(still_there.status(), StatusCode::FOUND);
    }

    #[actix_web::test]
    async fn anonymous_delete_only_drops_the_record() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let cookie = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/a", "custom_code": "keepme" })
        ));

        let removed =
            test::call_service(&app, recent_delete("/recent/keepme", &cookie).to_request()).await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);

        let redirect = test::call_service(&app, get("/keepme").to_request()).await;
        assert_eq!(
            redirect.status(),
            StatusCode::FOUND,
            "匿名访客移出清单不该影响短链"
        );
    }

    #[actix_web::test]
    async fn login_page_is_served_instead_of_looking_like_a_code() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/login").to_request()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));

        let html = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();
        assert!(
            html.contains("auth.js") || html.contains("login.js"),
            "返回的应是登录页"
        );
    }
}
