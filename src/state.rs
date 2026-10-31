//! 应用级共享状态：由组合根构建，处理器只读。

use std::sync::Arc;

use crate::service::admin::AdminService;
use crate::service::history::HistoryService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;

pub struct AppState {
    pub shorten: Arc<ShortenService>,
    pub redirect: Arc<RedirectService>,
    pub admin: Arc<AdminService>,
    pub history: Arc<HistoryService>,
    /// 对外站点是 https 时给访客 cookie 加 `Secure`
    pub cookie_secure: bool,
}
