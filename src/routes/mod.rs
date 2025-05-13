pub mod redirect;
pub mod shorten;

use actix_web::web;

pub fn init_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(shorten::shorten_handler);
    cfg.service(redirect::redirect_handler);
}
