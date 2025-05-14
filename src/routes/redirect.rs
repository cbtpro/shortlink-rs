use actix_web::{get, web, HttpResponse, Responder};
use redis::Client;
use sqlx::MySqlPool;

use crate::{cache, db::link::get_link_by_code};

#[get("/{code}")]
pub async fn redirect_handler(
    path: web::Path<String>,
    db: web::Data<MySqlPool>,
    redis: web::Data<Client>,
) -> impl Responder {
    let code = path.into_inner();
    let redis_key = format!("short:code:{}", &code);
    println!("Redis key: {}", redis_key);
    let client = redis.clone();
    // 获取异步连接
    let mut redis_conn = match client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().body("Redis unavailable"),
    };

    if let Ok(Some(cached_url)) = cache::get_cached_url(&mut redis_conn, &redis_key).await {
        println!("Cache hit: {}", cached_url);
        return HttpResponse::Found()
            .append_header(("Location", cached_url))
            .finish();
    }

    println!("Redis miss or cache not used, querying DB...");
    // 从数据库获取
    match get_link_by_code(&db, &code).await {
        Ok(Some(url)) => {
            println!("DB hit: {}", url);
            // 写入 Redis 缓存，设置过期时间 1 小时
            let _: () = cache::set_cached_url(&mut redis_conn, &redis_key, &url, 3600)
                .await
                .unwrap_or(());
            HttpResponse::Found()
                .append_header(("Location", url))
                .finish()
        }
        Ok(None) => HttpResponse::NotFound().body("Short URL not found"),
        Err(_) => HttpResponse::InternalServerError().body("Database error"),
    }
}
