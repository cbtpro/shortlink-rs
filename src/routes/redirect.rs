use actix_web::{get, web, HttpResponse, Responder};
use redis::{AsyncCommands, Client};
use sqlx::MySqlPool;

use crate::db::link::get_link_by_code;

#[get("/{code}")]
pub async fn redirect_handler(
    path: web::Path<String>,
    db: web::Data<MySqlPool>,
    redis: web::Data<Client>,
) -> impl Responder {
    let code = path.into_inner();
    let client = redis.clone();
    // 获取异步连接
    let mut redis_conn = match client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().body("Redis unavailable"),
    };

    let redis_key = format!("short:code:{}", &code);
    if let Ok(cached_url) = redis_conn.get::<_, String>(redis_key).await {
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
            let _: () = redis_conn
                .set_ex(format!("short:code:{}", code), &url, 3600)
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
