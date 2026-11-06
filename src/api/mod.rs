//! 接口层：HTTP 协议适配。只做参数解析、调用用例、序列化响应。

pub mod dto;
pub mod session;
pub mod visitor;

mod app_config;
mod assets;
mod auth;
mod console;
mod dev;
mod health;
mod history;
mod interstitial;
mod notice;
mod redirect;
mod shorten;
mod template;

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

    // 密码表单同理：解析失败不该漏出 actix 的纯文本 400
    cfg.app_data(web::FormConfig::default().error_handler(|error, _| {
        log::debug!("rejected form body: {error}");
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
    cfg.service(auth::change_password);

    cfg.service(shorten::create_link);
    cfg.service(history::list);
    cfg.service(history::forget);
    cfg.service(history::clear);

    // 控制台：登录用户按角色看到不同的数据，规则在 service::console
    cfg.service(console::overview);
    cfg.service(console::list);
    cfg.service(console::delete_link);

    // 账号管理：只有管理员能进
    cfg.service(console::list_users);
    cfg.service(console::reset_password);
    cfg.service(console::set_status);

    cfg.service(redirect::redirect);
    cfg.service(redirect::submit_password);
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
    use crate::domain::account::{Access, NewUser, Role, User};
    use crate::domain::code::RandomCodeGenerator;
    use crate::repository::{CreationHistory, LinkRepository, SessionRepository, UserRepository};
    use crate::service::auth::AuthService;
    use crate::service::console::ConsoleService;
    use crate::service::history::HistoryService;
    use crate::service::my_links::MyLinksService;
    use crate::service::redirect::RedirectService;
    use crate::service::shorten::ShortenService;
    use crate::settings::{AssetsMode, Profile};
    use crate::state::AppState;
    use crate::test_support::{
        InMemoryGateTicket, InMemoryLinkRepository, NoOpCache, NoOpCounter, PlainHasher,
        RecordingCache, RecordingCounter,
    };

    const BASE_URL: &str = "http://short.test";
    /// 测试账号统一用这个口令；`PlainHasher` 把它原样当成散列
    const PASSWORD: &str = "secret123";
    const ADMIN_EMAIL: &str = "admin@example.com";

    /// 组装一个不依赖 MySQL / Redis 的应用实例。
    fn state() -> web::Data<AppState> {
        state_of(
            Arc::new(InMemoryLinkRepository::new()),
            Arc::new(NoOpCounter),
            Arc::new(NoOpCache),
        )
    }

    /// 同一个组合根，但依赖由调用方给 —— 需要预置数据或观察副作用时用。
    fn state_of(
        store: Arc<InMemoryLinkRepository>,
        counter: Arc<dyn VisitCounter>,
        cache: Arc<dyn LinkCache>,
    ) -> web::Data<AppState> {
        build_state(store, counter, cache, test_assets())
    }

    fn build_state(
        store: Arc<InMemoryLinkRepository>,
        counter: Arc<dyn VisitCounter>,
        cache: Arc<dyn LinkCache>,
        assets: Arc<crate::assets::AssetServer>,
    ) -> web::Data<AppState> {
        build_state_with(store, counter, cache, assets, None)
    }

    /// 和 `build_state` 一样，但可以指定「初始口令文件」的位置。
    /// 只有验证「改密后清掉凭据」的用例需要它。
    fn build_state_with(
        store: Arc<InMemoryLinkRepository>,
        counter: Arc<dyn VisitCounter>,
        cache: Arc<dyn LinkCache>,
        assets: Arc<crate::assets::AssetServer>,
        admin_password_file: Option<std::path::PathBuf>,
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
            counter.clone(),
            Arc::new(InMemoryGateTicket::new()),
            Duration::from_secs(60),
        ));
        let console = Arc::new(ConsoleService::new(
            repository.clone(),
            users.clone(),
            sessions.clone(),
            cache.clone(),
            counter,
            Arc::new(PlainHasher),
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
            console,
            auth,
            my_links,
            history,
            assets,
            readiness: None,
            profile: Profile::Test,
            site_name: "shortlink".to_string(),
            api_base: String::new(),
            cookie_secure: false,
            admin_password_file,
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

    /// 造一份「初始口令文件」，返回路径。调用方负责删掉它所在的目录。
    fn temp_password_file(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "shortlink-api-{tag}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("临时目录应当建得出来");
        let path = dir.join("admin-initial-password.txt");
        std::fs::write(&path, "email=admin@example.com\npassword=secret123\n")
            .expect("凭据文件应当写得进去");
        path
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

    /// 读出响应体文本。跳转失败时给人看的页面走这条。
    async fn body_text<B>(response: actix_web::dev::ServiceResponse<B>) -> String
    where
        B: actix_web::body::MessageBody,
        B::Error: std::fmt::Debug,
    {
        let bytes = test::read_body(response).await;
        String::from_utf8(bytes.to_vec()).expect("页面应当以 utf-8 编码")
    }

    /// 直接在仓储里造一个账号：不关心注册流程本身的用例走这条。
    async fn add_user(store: &InMemoryLinkRepository, email: &str, access: Access) -> User {
        <InMemoryLinkRepository as UserRepository>::create(
            store,
            &NewUser {
                email: email.to_string(),
                password: PASSWORD.to_string(),
                display_name: None,
            },
            &format!("plain${PASSWORD}"),
            access,
        )
        .await
        .expect("user should be created")
    }

    /// 登录并取回可放进 `Cookie` 头的 `sl_session=...`。
    ///
    /// 用宏而不是函数：`test::call_service` 要求写出具体的 service 类型，
    /// 那串泛型参数在调用处没有信息量。
    macro_rules! sign_in {
        ($app:expr, $email:expr) => {{
            let request = test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({ "email": $email, "password": PASSWORD }))
                .to_request();
            let response = test::call_service($app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{} 应当能登录",
                $email
            );
            session_cookie(&response)
        }};
    }

    /// 状态页上的密码表单。
    fn password_form(code: &str, password: &str) -> actix_web::test::TestRequest {
        test::TestRequest::post()
            .uri(&format!("/{code}"))
            .insert_header((header::CONTENT_TYPE, "application/x-www-form-urlencoded"))
            .set_payload(format!("password={password}"))
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
                .app_data(build_state(
                    store,
                    Arc::new(NoOpCounter),
                    Arc::new(NoOpCache),
                    test_assets(),
                ))
                .configure(configure),
        )
        .await;

        let response = test::call_service(&app, get("/old").to_request()).await;
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));

        // 访客点开的是浏览器：给他一张说得清原因的页面，不是一段 JSON
        let body = body_text(response).await;
        assert!(body.contains("这条短链已经过期"), "{body}");
        assert!(body.contains("<code>old</code>"));
        assert!(!body.contains("<form"), "过期页不该要密码");
    }

    #[actix_web::test]
    async fn rejects_exhausted_link_with_a_page() {
        // 库里 visit_count 还是 0，上限 1，Redis 里已有 1 次未回刷：
        // 判定必须立刻拒绝，页面也要说得清是「次数用完」而不是「过期」
        let store = Arc::new(InMemoryLinkRepository::new());
        store
            .insert(
                &crate::domain::new_link::NewShortLink {
                    url: "https://example.com".into(),
                    max_visits: Some(1),
                    ..Default::default()
                },
                "lim",
            )
            .await
            .expect("seed insert should succeed");

        let app = test::init_service(
            App::new()
                .app_data(build_state(
                    store,
                    Arc::new(RecordingCounter::with_pending("lim", 1)),
                    Arc::new(NoOpCache),
                    test_assets(),
                ))
                .configure(configure),
        )
        .await;

        let response = test::call_service(&app, get("/lim").to_request()).await;
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));

        let body = body_text(response).await;
        assert!(body.contains("访问次数已达上限"), "{body}");
    }

    #[actix_web::test]
    async fn missing_code_gets_a_page_too() {
        // 短码打错是常事，404 也该有页面，而不是把访客留在 JSON 里
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        let response = test::call_service(&app, get("/typo").to_request()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(content_type(&response), Some("text/html; charset=utf-8"));

        let body = body_text(response).await;
        assert!(body.contains("没有找到这个短码"), "{body}");
    }

    #[actix_web::test]
    async fn password_gate_renders_a_form_and_accepts_a_post() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/x",
                "custom_code": "vault2",
                "password": "s3cret"
            })
        );

        // 1. 没带密码：给输入框，而不是只有一句 403
        let required = test::call_service(&app, get("/vault2").to_request()).await;
        assert_eq!(required.status(), StatusCode::FORBIDDEN);
        assert_eq!(content_type(&required), Some("text/html; charset=utf-8"));
        let body = body_text(required).await;
        assert!(body.contains("请输入访问密码"), "{body}");
        assert!(body.contains("name=\"password\""));
        assert!(body.contains("method=\"post\""), "密码不能拼进 URL");

        // 2. 密码错了：同一页，换成「再试一次」的文案
        let rejected =
            test::call_service(&app, password_form("vault2", "wrong").to_request()).await;
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let body = body_text(rejected).await;
        assert!(body.contains("密码不正确"), "{body}");
        assert!(body.contains("<form"), "错了要能接着试");

        // 3. 密码对了：303 换回 GET 再走目标地址
        let accepted =
            test::call_service(&app, password_form("vault2", "s3cret").to_request()).await;
        assert_eq!(accepted.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            accepted
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/x")
        );
    }

    #[actix_web::test]
    async fn empty_password_field_counts_as_missing() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/x",
                "custom_code": "vault3",
                "password": "s3cret"
            })
        );

        let response = test::call_service(&app, password_form("vault3", "").to_request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let body = body_text(response).await;
        assert!(
            body.contains("请输入访问密码"),
            "空串是「没填」，不该报成「密码不正确」：{body}"
        );
    }

    /// 从中转页里取出「继续访问」的地址。
    fn confirm_link(body: &str, code: &str) -> String {
        let needle = format!("href=\"/{code}?confirm=");
        let start = body
            .find(&needle)
            .unwrap_or_else(|| panic!("中转页应给出带票据的继续地址：{body}"))
            + "href=\"".len();
        let rest = &body[start..];
        let end = rest.find('"').expect("href 必须有闭合引号");
        rest[..end].to_string()
    }

    #[actix_web::test]
    async fn interstitial_page_counts_only_after_continue() {
        let counter = Arc::new(RecordingCounter::new());
        let app = test::init_service(
            App::new()
                .app_data(state_of(
                    Arc::new(InMemoryLinkRepository::new()),
                    counter.clone(),
                    Arc::new(NoOpCache),
                ))
                .configure(configure),
        )
        .await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/gate",
                "custom_code": "gate",
                "interstitial": true
            })
        );

        // 1. 打开确认页：什么都不发生，次数不动
        let page = test::call_service(&app, get("/gate").to_request()).await;
        assert_eq!(page.status(), StatusCode::OK);
        let body = body_text(page).await;
        let confirm = confirm_link(&body, "gate");
        assert_eq!(counter.increments_of("gate"), 0, "打开中转页不该计次");

        // 2. 点「继续访问」：这一跳才算一次
        let jumped = test::call_service(&app, get(&confirm).to_request()).await;
        assert_eq!(jumped.status(), StatusCode::FOUND);
        assert_eq!(
            jumped
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/gate")
        );
        assert_eq!(counter.increments_of("gate"), 1);

        // 3. 票据是一次性的：重放同一张只会退回确认页，不再计次
        let replay = test::call_service(&app, get(&confirm).to_request()).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            counter.increments_of("gate"),
            1,
            "用过或伪造的票据不能跳转，也不能计数"
        );
    }

    #[actix_web::test]
    async fn forged_confirm_ticket_does_not_bypass_the_password() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        post_shorten!(
            &app,
            serde_json::json!({
                "url": "https://example.com/secret",
                "custom_code": "vault4",
                "password": "s3cret",
                "interstitial": true
            })
        );

        // 手写一个 confirm 参数，换不来一次跳转：票据是服务端签发的，不是请求里说了算
        let response = test::call_service(&app, get("/vault4?confirm=whatever").to_request()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let body = body_text(response).await;
        assert!(body.contains("请输入访问密码"));
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
                .app_data(state_of(
                    Arc::new(InMemoryLinkRepository::new()),
                    Arc::new(RecordingCounter::new()),
                    Arc::new(NoOpCache),
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

    /// 一个「已经可以正常使用」的管理员访问级别（不强制改密）。
    fn admin_access() -> Access {
        Access {
            role: Role::Admin,
            must_change_password: false,
        }
    }

    fn now_well_formed_token() -> String {
        // 会话令牌在查库前先做格式校验，随便一串会被直接当成「没登录」
        "a".repeat(64)
    }

    #[actix_web::test]
    async fn console_api_requires_a_session() {
        let app = test::init_service(App::new().app_data(state()).configure(configure)).await;

        for uri in [
            "/console/api/links",
            "/console/api/overview",
            "/admin/api/users",
        ] {
            let response = test::call_service(&app, get(uri).to_request()).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} 应要求登录"
            );
        }

        let bogus = format!("sl_session={}", now_well_formed_token());
        let response = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &bogus).to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn users_only_see_and_delete_their_own_links() {
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, "alice@example.com", Access::USER).await;
        add_user(&store, "bob@example.com", Access::USER).await;
        add_user(&store, ADMIN_EMAIL, admin_access()).await;

        let app = test::init_service(
            App::new()
                .app_data(state_of(
                    store.clone(),
                    Arc::new(NoOpCounter),
                    Arc::new(NoOpCache),
                ))
                .configure(configure),
        )
        .await;

        let alice = sign_in!(&app, "alice@example.com");
        post_shorten_as!(
            &app,
            &alice,
            serde_json::json!({ "url": "https://example.com/a", "custom_code": "mine" })
        );

        let bob = sign_in!(&app, "bob@example.com");
        post_shorten_as!(
            &app,
            &bob,
            serde_json::json!({ "url": "https://example.com/b", "custom_code": "theirs" })
        );

        let listed = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &alice).to_request(),
        )
        .await;
        assert_eq!(listed.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(listed).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["code"], "mine");
        assert!(
            body["items"][0]["owner"].is_null(),
            "普通用户不需要看到归属列"
        );

        // 别人的短码：删除一律 404，不区分「不是你的」和「不存在」
        let refused = test::call_service(
            &app,
            with_cookie("DELETE", "/console/api/links/theirs", &alice).to_request(),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::NOT_FOUND);

        // 管理员看全站，并带上归属
        let admin = sign_in!(&app, ADMIN_EMAIL);
        let all = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &admin).to_request(),
        )
        .await;
        let body: serde_json::Value = test::read_body_json(all).await;
        assert_eq!(body["total"], 2);
        let owner_of_bob = body["items"]
            .as_array()
            .expect("items should be an array")
            .iter()
            .find(|item| item["code"] == "theirs")
            .expect("bob 的链接应在列表里")
            .pointer("/owner/email")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        assert_eq!(owner_of_bob.as_deref(), Some("bob@example.com"));

        // 管理员可以删别人的链接
        let removed = test::call_service(
            &app,
            with_cookie("DELETE", "/console/api/links/theirs", &admin).to_request(),
        )
        .await;
        assert_eq!(removed.status(), StatusCode::NO_CONTENT);
        assert!(store.find_by_code("theirs").await.unwrap().is_none());
    }

    #[actix_web::test]
    async fn user_management_is_admin_only() {
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, "alice@example.com", Access::USER).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let alice = sign_in!(&app, "alice@example.com");
        let response = test::call_service(
            &app,
            with_cookie("GET", "/admin/api/users", &alice).to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["message"], "administrator privileges required");
    }

    #[actix_web::test]
    async fn temporary_password_must_be_changed_before_the_console_opens() {
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, Access::ADMIN_WITH_TEMP_PASSWORD).await;
        let password_file = temp_password_file("initial");
        let app = test::init_service(
            App::new()
                .app_data(build_state_with(
                    store,
                    Arc::new(NoOpCounter),
                    Arc::new(NoOpCache),
                    test_assets(),
                    Some(password_file.clone()),
                ))
                .configure(configure),
        )
        .await;

        // 初始口令能登录，但只能用来改密码
        let temporary = sign_in!(&app, ADMIN_EMAIL);
        let blocked = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &temporary).to_request(),
        )
        .await;
        assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = test::read_body_json(blocked).await;
        assert_eq!(
            body["message"],
            "password must be changed before continuing"
        );
        assert!(
            password_file.exists(),
            "还没改密，文件仍是有效凭据，不能提前删"
        );

        // 当前口令不对
        let wrong = test::call_service(
            &app,
            with_cookie("POST", "/auth/password", &temporary)
                .set_json(serde_json::json!({
                    "current_password": "nope",
                    "new_password": "brandnew123"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        // 太短
        let weak = test::call_service(
            &app,
            with_cookie("POST", "/auth/password", &temporary)
                .set_json(serde_json::json!({
                    "current_password": PASSWORD,
                    "new_password": "short"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(weak.status(), StatusCode::BAD_REQUEST);

        // 改密成功：换发新 cookie，旧会话作废
        let changed = test::call_service(
            &app,
            with_cookie("POST", "/auth/password", &temporary)
                .set_json(serde_json::json!({
                    "current_password": PASSWORD,
                    "new_password": "brandnew123"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        let fresh = session_cookie(&changed);
        let body: serde_json::Value = test::read_body_json(changed).await;
        assert_eq!(body["user"]["must_change_password"], false);

        let opened = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &fresh).to_request(),
        )
        .await;
        assert_eq!(opened.status(), StatusCode::OK);

        let stale = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &temporary).to_request(),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::UNAUTHORIZED, "旧会话应已失效");

        // 之后只能用新口令登录
        let with_old = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({ "email": ADMIN_EMAIL, "password": PASSWORD }))
                .to_request(),
        )
        .await;
        assert_eq!(with_old.status(), StatusCode::UNAUTHORIZED);

        let with_new = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({ "email": ADMIN_EMAIL, "password": "brandnew123" }))
                .to_request(),
        )
        .await;
        assert_eq!(with_new.status(), StatusCode::OK);

        // 一次性口令已经作废，文件留着只会让运维以为还能用
        assert!(
            !password_file.exists(),
            "管理员改完初始口令后应立刻删除凭据文件"
        );
        let _ = std::fs::remove_dir_all(password_file.parent().unwrap());
    }

    #[actix_web::test]
    async fn changing_a_reset_password_keeps_the_admin_credential_file() {
        // 被管理员重置过的普通用户同样是「待改密」，但它的口令不在文件里。
        // 清文件的条件必须同时看角色，否则随便一个普通用户改密就能把管理员凭据删掉。
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(
            &store,
            "bob@example.com",
            Access {
                role: Role::User,
                must_change_password: true,
            },
        )
        .await;
        let password_file = temp_password_file("reset");
        let app = test::init_service(
            App::new()
                .app_data(build_state_with(
                    store,
                    Arc::new(NoOpCounter),
                    Arc::new(NoOpCache),
                    test_assets(),
                    Some(password_file.clone()),
                ))
                .configure(configure),
        )
        .await;

        let cookie = sign_in!(&app, "bob@example.com");
        let changed = test::call_service(
            &app,
            with_cookie("POST", "/auth/password", &cookie)
                .set_json(serde_json::json!({
                    "current_password": PASSWORD,
                    "new_password": "brandnew123"
                }))
                .to_request(),
        )
        .await;
        assert_eq!(changed.status(), StatusCode::OK);
        assert!(password_file.exists(), "普通用户改密不该动管理员的初始凭据");

        let _ = std::fs::remove_dir_all(password_file.parent().unwrap());
    }

    #[actix_web::test]
    async fn login_reports_the_role_and_status_the_console_branches_on() {
        // 前端只看 `role === "admin"` 决定要不要显示用户页，
        // 看 `must_change_password` 决定要不要挡在改密面板。这三个字段是契约。
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        add_user(&store, "plain@example.com", Access::USER).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response =
            test::call_service(&app, with_cookie("GET", "/auth/me", &admin).to_request()).await;
        let body = body_json(response).await;
        assert_eq!(body["user"]["role"], "admin");
        assert_eq!(body["user"]["status"], "active");
        assert_eq!(body["user"]["must_change_password"], false);

        let plain = sign_in!(&app, "plain@example.com");
        let response =
            test::call_service(&app, with_cookie("GET", "/auth/me", &plain).to_request()).await;
        let body = body_json(response).await;
        assert_eq!(body["user"]["role"], "user");
    }

    #[actix_web::test]
    async fn only_admins_see_who_owns_a_link() {
        // 归属列只对管理员有意义；普通用户的行里 `owner` 必须是 null，
        // 否则前端会画出「别人的邮箱」这种既没用又泄露信息的格子。
        let store = Arc::new(InMemoryLinkRepository::new());
        let alice = add_user(&store, "alice@example.com", Access::USER).await;
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        <InMemoryLinkRepository as LinkRepository>::insert(
            &store,
            &crate::domain::new_link::NewShortLink {
                url: "https://example.com/alice".into(),
                owner_id: Some(alice.id),
                ..Default::default()
            },
            "mine",
        )
        .await
        .expect("link should be inserted");
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let plain = sign_in!(&app, "alice@example.com");
        let response = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &plain).to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["items"][0]["code"], "mine");
        assert!(body["items"][0]["owner"].is_null(), "普通用户不该看到归属");

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &admin).to_request(),
        )
        .await;
        let body = body_json(response).await;
        assert_eq!(body["items"][0]["owner"]["email"], "alice@example.com");
    }

    #[actix_web::test]
    async fn admin_can_reset_a_user_password() {
        let store = Arc::new(InMemoryLinkRepository::new());
        let alice = add_user(&store, "alice@example.com", Access::USER).await;
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie(
                "POST",
                &format!("/admin/api/users/{}/password", alice.id),
                &admin,
            )
            .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(body["email"], "alice@example.com");
        let temporary = body["password"]
            .as_str()
            .expect("重置密码应返回一次性口令")
            .to_string();
        assert!(temporary.len() >= 8);

        // 旧口令进不来
        let old = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({ "email": "alice@example.com", "password": PASSWORD }))
                .to_request(),
        )
        .await;
        assert_eq!(old.status(), StatusCode::UNAUTHORIZED);

        // 临时口令能登录，但必须先改密
        let login = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({
                    "email": "alice@example.com",
                    "password": temporary
                }))
                .to_request(),
        )
        .await;
        assert_eq!(login.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(login).await;
        assert_eq!(body["user"]["must_change_password"], true);
    }

    #[actix_web::test]
    async fn disabling_a_user_takes_effect_immediately() {
        let store = Arc::new(InMemoryLinkRepository::new());
        let alice = add_user(&store, "alice@example.com", Access::USER).await;
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let alice_cookie = sign_in!(&app, "alice@example.com");
        let admin = sign_in!(&app, ADMIN_EMAIL);

        let disabled = test::call_service(
            &app,
            with_cookie(
                "POST",
                &format!("/admin/api/users/{}/status", alice.id),
                &admin,
            )
            .set_json(serde_json::json!({ "status": "disabled" }))
            .to_request(),
        )
        .await;
        assert_eq!(disabled.status(), StatusCode::OK);
        let body: serde_json::Value = test::read_body_json(disabled).await;
        assert_eq!(body["status"], "disabled");

        // 已登录的会话立刻失效
        let stale = test::call_service(
            &app,
            with_cookie("GET", "/console/api/links", &alice_cookie).to_request(),
        )
        .await;
        assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

        // 再登录被告知账号已停用
        let login = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/auth/login")
                .set_json(serde_json::json!({ "email": "alice@example.com", "password": PASSWORD }))
                .to_request(),
        )
        .await;
        assert_eq!(login.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = test::read_body_json(login).await;
        assert_eq!(body["message"], "account is disabled");

        // 重新启用后恢复
        let enabled = test::call_service(
            &app,
            with_cookie(
                "POST",
                &format!("/admin/api/users/{}/status", alice.id),
                &admin,
            )
            .set_json(serde_json::json!({ "status": "active" }))
            .to_request(),
        )
        .await;
        assert_eq!(enabled.status(), StatusCode::OK);

        let _ = sign_in!(&app, "alice@example.com");
    }

    #[actix_web::test]
    async fn admin_cannot_disable_themselves() {
        let store = Arc::new(InMemoryLinkRepository::new());
        let admin = add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let cookie = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie(
                "POST",
                &format!("/admin/api/users/{}/status", admin.id),
                &cookie,
            )
            .set_json(serde_json::json!({ "status": "disabled" }))
            .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body: serde_json::Value = test::read_body_json(response).await;
        assert_eq!(
            body["message"],
            "cannot change the status of your own account"
        );
    }

    #[actix_web::test]
    async fn rejects_unknown_status_values() {
        let store = Arc::new(InMemoryLinkRepository::new());
        let alice = add_user(&store, "alice@example.com", Access::USER).await;
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie(
                "POST",
                &format!("/admin/api/users/{}/status", alice.id),
                &admin,
            )
            // 拼错的状态不能被当成「禁用」：一次手滑不该停掉别人的号
            .set_json(serde_json::json!({ "status": "deactivated" }))
            .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let cookie = visitor_cookie(&post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/x", "custom_code": "doomed" })
        ));

        let admin = sign_in!(&app, ADMIN_EMAIL);
        test::call_service(
            &app,
            with_cookie("DELETE", "/console/api/links/doomed", &admin).to_request(),
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
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie("DELETE", "/console/api/links/ghost", &admin).to_request(),
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

        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), cache.clone()))
                .configure(configure),
        )
        .await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com/cached", "custom_code": "cached" })
        );

        let admin = sign_in!(&app, ADMIN_EMAIL);
        test::call_service(
            &app,
            with_cookie("DELETE", "/console/api/links/cached", &admin).to_request(),
        )
        .await;

        assert!(!cache.contains("cached"), "删链后必须清缓存");
    }

    #[actix_web::test]
    async fn admin_overview_reports_totals() {
        let store = Arc::new(InMemoryLinkRepository::new());
        add_user(&store, ADMIN_EMAIL, admin_access()).await;
        let app = test::init_service(
            App::new()
                .app_data(state_of(store, Arc::new(NoOpCounter), Arc::new(NoOpCache)))
                .configure(configure),
        )
        .await;

        post_shorten!(
            &app,
            serde_json::json!({ "url": "https://example.com", "custom_code": "one" })
        );
        // 访问两次，计数写进 Redis 替身，不影响库里的 visit_count
        test::call_service(&app, get("/one").to_request()).await;
        test::call_service(&app, get("/one").to_request()).await;

        let admin = sign_in!(&app, ADMIN_EMAIL);
        let response = test::call_service(
            &app,
            with_cookie("GET", "/console/api/overview", &admin).to_request(),
        )
        .await;
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
            "notice.html",
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
                    Arc::new(InMemoryLinkRepository::new()),
                    Arc::new(NoOpCounter),
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
