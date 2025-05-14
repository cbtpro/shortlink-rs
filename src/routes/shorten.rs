use actix_web::{post, web, HttpResponse, Responder};
use chrono::NaiveDateTime;
use redis::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::MySqlPool;

use crate::{db::link::save_link, utils::shortcode::generate_random_string};

#[derive(Deserialize)]
pub struct ShortenRequest {
    pub url: String,
    pub custom_code: Option<String>,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
}
#[derive(Serialize)]
struct ErrorResponse {
    code: u16,
    data: String,
    message: String,
}

#[post("/shorten")]
pub async fn shorten_handler(
    payload: web::Json<ShortenRequest>,
    db: web::Data<MySqlPool>,
    _redis: web::Data<Client>,
) -> impl Responder {
    let code = payload
        .custom_code
        .clone()
        .unwrap_or_else(|| generate_random_string(16));
    match save_link(&db, &payload, &code).await {
        Ok(short_link) => HttpResponse::Ok().json(serde_json::json!({
          "short_url": format!("http://127.0.0.1:9981/{}",short_link.code),
          "long_url": short_link.long_url,
          "created_at": short_link.created_at.to_string(),
        })),
        Err(e) => {
            let error_response = ErrorResponse {
                code: 200,
                data: String::new(),
                message: e.to_string(),
            };
            HttpResponse::InternalServerError().json(error_response)
        }
    }
}
