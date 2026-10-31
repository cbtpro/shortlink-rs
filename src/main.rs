//! 组合根：读配置、装配依赖、启动 HTTP 服务与后台任务。

use std::sync::Arc;

use actix_web::{web, App, HttpServer};
use anyhow::{Context, Result};
use log::{info, warn};
use sqlx::mysql::MySqlPoolOptions;

mod api;
mod cache;
mod config;
mod domain;
mod error;
mod repository;
mod service;
mod state;
mod tasks;
mod types;

#[cfg(test)]
mod test_support;

use crate::cache::redis_store::{RedisLinkCache, RedisVisitCounter};
use crate::cache::{LinkCache, VisitCounter};
use crate::config::{redact_credentials, Config};
use crate::domain::code::RandomCodeGenerator;
use crate::repository::mysql::MySqlLinkRepository;
use crate::repository::{CreationHistory, LinkRepository};
use crate::service::admin::AdminService;
use crate::service::history::HistoryService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;
use crate::state::AppState;
use crate::tasks::flush_visits::VisitFlusher;

#[actix_web::main]
async fn main() -> Result<()> {
    // 先加载 .env，日志级别由 RUST_LOG 控制，必须在 init_logging 之前生效
    dotenvy::dotenv().ok();
    init_logging();

    let config = Config::from_env().context("failed to load configuration")?;

    let pool = MySqlPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| {
            format!(
                "failed to connect to MySQL at {}",
                redact_credentials(&config.database.url)
            )
        })?;

    let redis_conn = crate::cache::redis_store::connect(&config.redis.url).await?;

    info!(
        "dependencies ready: mysql={} redis={}",
        redact_credentials(&config.database.url),
        redact_credentials(&config.redis.url)
    );

    // 依赖以 trait object 注入：换实现只改这几行
    let store = Arc::new(MySqlLinkRepository::new(pool));
    let repository: Arc<dyn LinkRepository> = store.clone();
    // 同一个实现同时充当两个端口，避免再开一条连接池
    let creations: Arc<dyn CreationHistory> = store;
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
        link_cache,
        config.admin.token.clone(),
    ));
    let history = Arc::new(HistoryService::new(
        creations,
        config.history.recent_limit,
    ));

    if admin.is_enabled() {
        info!("admin console enabled at /admin");
    } else {
        warn!("ADMIN_TOKEN is not set, admin console is disabled");
    }

    VisitFlusher::new(repository, visit_counter).spawn(config.cache.visit_flush_interval);

    let state = web::Data::new(AppState {
        shorten,
        redirect,
        admin,
        history,
        cookie_secure: config.history.cookie_secure,
    });
    let bind_address = (config.server.host, config.server.port);
    info!("listening on http://{}:{}", bind_address.0, bind_address.1);

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .configure(api::configure)
    })
    .bind(bind_address)?
    .run()
    .await?;

    Ok(())
}

fn init_logging() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
}
