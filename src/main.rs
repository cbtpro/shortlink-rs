use actix_web::{web, App, HttpServer};
use dotenvy::dotenv;
use sqlx::MySqlPool;

mod cache;
mod config;
mod db;
mod errors;
mod models;
mod routes;
mod tasks;
mod utils;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();
    dotenv().ok();
    let config = config::load();

    let db_pool = MySqlPool::connect(&*config.database_url)
        .await
        .expect("DB connection failed");
    let redis_client = redis::Client::open(&*config.redis_url).expect("Redis connection failed");
    let server_port = config.server_port;

    // 启动同步任务
    tasks::sync_visits::start_sync_task(redis_client.clone(), db_pool.clone()).await;

    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(db_pool.clone()))
            .app_data(web::Data::new(redis_client.clone()))
            .configure(routes::init_routes)
    })
    .bind(("0.0.0.0", server_port))?
    .bind("[::1]:9000")?;

    println!(
        "Server starting at: http://127.0.0.1:{}, http://[::1]:9000",
        server_port
    );

    // 等待服务器启动并运行
    server.run().await
}
