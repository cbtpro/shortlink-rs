use actix_web::{post, web, HttpResponse, Responder};
use serde::Deserialize;
use sqlx::MySqlPool;
use redis::Client;

use crate::{utils::shortcode::generate_random_string, db::link::save_link};

#[derive(Deserialize)]
pub struct ShortenRequest {
    pub url: String,
    pub custom_code: Option<String>,
}

#[post("/shorten")]
pub async fn shorten_handler(
    payload: web::Json<ShortenRequest>,
    db: web::Data<MySqlPool>,
    _redis: web::Data<Client>,
) -> impl Responder {
    let code = payload.custom_code.clone().unwrap_or_else(|| generate_random_string(16));
    match save_link(&db, &code, &payload.url).await {
        Ok(_) => HttpResponse::Ok().json( serde_json::json!({ "short_url": format!("https://sh.rt/{}", code) }) ),
        Err(e) => HttpResponse::InternalServerError().body(format!("Failed: {}", e))
    }
}