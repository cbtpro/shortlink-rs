//! 组合根：读配置、装配依赖、启动 HTTP 服务与后台任务。
//!
//! 这里是唯一知道「具体用哪个实现」的地方：换个存储就是换这里的两行。

use std::sync::Arc;

use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use sqlx::mysql::MySqlPoolOptions;

use crate::api;
use crate::assets::AssetServer;
use crate::cache::redis_store::{self, RedisLinkCache, RedisVisitCounter};
use crate::cache::{LinkCache, VisitCounter};
use crate::domain::code::RandomCodeGenerator;
use crate::health::Readiness;
use crate::repository::mysql::{MySqlAccountRepository, MySqlLinkRepository};
use crate::repository::{CreationHistory, LinkRepository, SessionRepository, UserRepository};
use crate::security::Pbkdf2Hasher;
use crate::service::admin::AdminService;
use crate::service::auth::AuthService;
use crate::service::history::HistoryService;
use crate::service::my_links::MyLinksService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;
use crate::settings::Config;
use crate::state::AppState;
use crate::tasks::flush_visits::VisitFlusher;
use crate::tasks::purge_sessions::SessionPurger;

/// 访问日志：方法与路径用引号括起来，方便直接粘贴到 `grep`。
const REQUEST_LOG: &str = r#"%a "%r" %s %b %Dms"#;

pub async fn run() -> Result<()> {
    let config = Config::load().context("failed to load configuration")?;
    init_logging();

    // 配置加载发生在日志初始化之前（RUST_LOG 本身来自配置），所以层来源放到这里打
    for source in &config.sources {
        log::debug!("config source: {source}");
    }
    info!("{}", config.summary());

    // 占位域名会静默生成打不开的短链，与其上线后被用户发现，不如启动时就喊一声
    if config.profile == crate::settings::Profile::Production
        && config.server.public_base_url.contains("example.com")
    {
        warn!(
            "PUBLIC_BASE_URL is still the placeholder `{}`; short links handed to users will not work. \
             Set it to the real public origin.",
            config.server.public_base_url
        );
    }

    let assets = Arc::new(
        AssetServer::new(
            config.assets.mode,
            config.assets.root.clone(),
            config.assets.live_reload,
        )
        .context("failed to initialise assets")?,
    );
    if assets.live_reload() {
        info!(
            "assets served from {} at request time, browser auto-reload is on",
            config.assets.root.display()
        );
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| {
            format!(
                "failed to connect to MySQL at {}",
                crate::settings::redact_credentials(&config.database.url)
            )
        })?;

    let redis_conn = redis_store::connect(&config.redis.url).await?;

    info!(
        "dependencies ready: mysql={} redis={}",
        crate::settings::redact_credentials(&config.database.url),
        crate::settings::redact_credentials(&config.redis.url)
    );

    // 依赖以 trait object 注入：换实现只改这几行
    let readiness = Arc::new(Readiness::new(pool.clone(), redis_conn.clone()));
    let store = Arc::new(MySqlLinkRepository::new(pool.clone()));
    let repository: Arc<dyn LinkRepository> = store.clone();
    // 同一个实现同时充当两个端口，避免再开一条连接池
    let creations: Arc<dyn CreationHistory> = store;
    let accounts = Arc::new(MySqlAccountRepository::new(pool));
    let users: Arc<dyn UserRepository> = accounts.clone();
    let sessions: Arc<dyn SessionRepository> = accounts;
    let link_cache: Arc<dyn LinkCache> = Arc::new(RedisLinkCache::new(redis_conn.clone()));
    let visit_counter: Arc<dyn VisitCounter> = Arc::new(RedisVisitCounter::new(redis_conn));

    let shorten = Arc::new(ShortenService::new(
        repository.clone(),
        Arc::new(RandomCodeGenerator),
        config.link.code_length,
        config.link.max_code_length,
        config.server.public_base_url.clone(),
    ));
    let redirect = Arc::new(RedirectService::new(
        repository.clone(),
        link_cache.clone(),
        visit_counter.clone(),
        config.cache.link_ttl,
    ));
    let admin = Arc::new(AdminService::new(
        repository.clone(),
        link_cache.clone(),
        config.admin.token.clone(),
    ));
    let history = Arc::new(HistoryService::new(creations, config.history.recent_limit));
    let my_links = Arc::new(MyLinksService::new(
        repository.clone(),
        link_cache,
        config.history.recent_limit,
    ));
    let auth = Arc::new(AuthService::new(
        users,
        sessions,
        Arc::new(Pbkdf2Hasher::new(config.auth.password_iterations)),
        config.auth.session_ttl,
    ));

    if admin.is_enabled() {
        info!("admin console enabled at /admin");
    } else {
        warn!("ADMIN_TOKEN is not set, admin console is disabled");
    }

    VisitFlusher::new(repository, visit_counter).spawn(config.cache.visit_flush_interval);
    SessionPurger::new(auth.clone()).spawn();

    let state = web::Data::new(AppState {
        shorten,
        redirect,
        admin,
        auth,
        my_links,
        history,
        assets,
        readiness: Some(readiness),
        profile: config.profile,
        site_name: config.site.name.clone(),
        api_base: config.site.api_base.clone(),
        cookie_secure: config.history.cookie_secure,
    });

    let host = config.server.host.clone();
    let port = config.server.port;

    let server = HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(Logger::new(REQUEST_LOG))
            .configure(api::configure)
    })
    .bind((host.as_str(), port))
    .map_err(|err| bind_error(err, &host, port))?
    // 收到退出信号后给在途请求留出时间，避免回刷任务被硬切
    .shutdown_timeout(10);

    info!("listening on http://{host}:{port}");

    server.run().await?;

    Ok(())
}

fn init_logging() {
    // RUST_LOG 可能来自 profile 文件，必须等配置加载完再初始化
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
}

/// 端口被占用是最常见的启动失败，默认报错太抽象，这里直接给出下一步。
fn bind_error(err: std::io::Error, host: &str, port: u16) -> anyhow::Error {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        return anyhow!(
            "{host}:{port} is already in use. Another instance is probably still running: \
             find it with `netstat -ano | findstr :{port}` and stop it, or start on another port \
             with SERVER_PORT=<port>"
        );
    }

    anyhow!(err).context(format!("failed to bind {host}:{port}"))
}
