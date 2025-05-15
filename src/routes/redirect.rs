use actix_web::{get, web, HttpResponse, Responder};
use redis::Client;
use serde::Deserialize;
use sqlx::MySqlPool;

use crate::{
    cache,
    db::link::{get_link_by_code, increment_visit_count},
    models::short_link::ShortLink,
};

#[derive(Deserialize)]
pub struct QueryParams {
    password: Option<String>,
}

impl ShortLink {
    pub fn clone_with_new_count(&self, new_count: u32) -> Self {
        Self {
            visit_count: new_count,
            ..self.clone()
        }
    }
}

#[get("/{code}")]
pub async fn redirect_handler(
    path: web::Path<String>,
    query: web::Query<QueryParams>,
    db: web::Data<MySqlPool>,
    redis: web::Data<Client>,
) -> impl Responder {
    let code = path.into_inner();
    let provided_password = query.password.clone();
    let redis_key = format!("short:code:{}", &code);
    println!("Redis key: {}", redis_key);
    let client = redis.clone();

    // 获取 Redis 连接
    let mut redis_conn = match client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(_) => return HttpResponse::InternalServerError().body("Redis unavailable"),
    };

    // 先查缓存
    let cache_url = match cache::get_cached_by_code(&mut redis_conn, &redis_key).await {
        Ok(url) => url,
        Err(e) => {
            eprintln!("Failed to get cached URL: {:?}", e);
            return HttpResponse::InternalServerError().body("Database error");
        }
    };

    let redis_visit_key = format!("shortlink:visits:{}", &code);
    let visit_count = match cache::get_cached_by_code(&mut redis_conn, &redis_visit_key).await {
        Ok(count) => count
            .as_deref()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0),
        Err(e) => {
            eprintln!("Failed to increment visit count: {:?}", e);
            return HttpResponse::InternalServerError().body("Database error");
        }
    };
    fn increment_to_string(num: u32) -> String {
        (num + 1).to_string()
    }
    if visit_count > 0 {
        let _: () = match cache::set_cached(
            &mut redis_conn,
            &redis_visit_key,
            &increment_to_string(visit_count),
            3600,
        )
        .await
        {
            Ok(_) => {
                if let Some(url) = cache_url {
                    return HttpResponse::Found()
                        .append_header(("Location", &*url))
                        .finish();
                }
            }
            Err(e) => {
                eprintln!("Failed to increment visit count: {:?}", e);
                return HttpResponse::InternalServerError().body("Database error");
            }
        };
    }

    // 查数据库
    match get_link_by_code(&db, &code).await {
        Ok(Some(short_link)) => {
            let url = &short_link.long_url;

            // 校验密码（如有设置）
            if let Some(ref actual_password) = short_link.password {
                match provided_password {
                    Some(ref p) if p == actual_password => {} // 密码正确，继续
                    _ => return HttpResponse::Forbidden().body("Invalid or missing password"),
                }
            }

            // 检查访问次数
            if let Some(max) = short_link.max_visits {
                if short_link.visit_count >= max {
                    return HttpResponse::Gone().body("Visit limit exceeded");
                }
            }

            // 更新访问次数
            let updated_count = match increment_visit_count(&db, short_link.id).await {
                Ok(count) => count,
                Err(e) => {
                    eprintln!("Failed to increment visit count: {:?}", e);
                    return HttpResponse::InternalServerError().body("Database error");
                }
            };
            let new_short_link = short_link.clone_with_new_count(updated_count);
            // 缓存结果
            let _: () = cache::set_cached(
                &mut redis_conn,
                &redis_key,
                &serde_json::json!(&new_short_link).to_string(),
                3600,
            )
            .await
            .unwrap_or(());

            // let _ = cache::set_cached(
            //     &mut redis_conn,
            //     &redis_visit_key,
            //     &updated_count.to_string(),
            //     3600,
            // )
            // .await
            // .unwrap_or(());

            HttpResponse::Found()
                .append_header(("Location", &**url))
                .finish()
        }
        Ok(None) => HttpResponse::NotFound().body("Short URL not found"),
        Err(_) => HttpResponse::InternalServerError().body("Database error"),
    }
}
