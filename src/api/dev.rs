//! 开发期端点：给浏览器自动刷新用。生产环境（未开热刷新）一律 404。

use actix_web::http::header;
use actix_web::{get, web, HttpResponse};

use crate::state::AppState;

const LIVE_RELOAD_JS: &str = include_str!("../../web/assets/js/dev/live-reload.js");

#[get("/__dev/revision")]
pub async fn revision(state: web::Data<AppState>) -> HttpResponse {
    if !state.assets.live_reload() {
        return HttpResponse::NotFound().finish();
    }

    HttpResponse::Ok()
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .json(serde_json::json!({
            "revision": state.assets.revision(),
            "stamp": crate::assets::ASSET_STAMP,
        }))
}

#[get("/__dev/live-reload.js")]
pub async fn live_reload_script(state: web::Data<AppState>) -> HttpResponse {
    if !state.assets.live_reload() {
        return HttpResponse::NotFound().finish();
    }

    HttpResponse::Ok()
        // 脚本本身必须每次回源，否则改了刷新逻辑也看不到
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .content_type("text/javascript; charset=utf-8")
        .body(LIVE_RELOAD_JS)
}
