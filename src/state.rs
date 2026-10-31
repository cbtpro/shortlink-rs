//! 应用级共享状态：由组合根构建，处理器只读。

use std::sync::Arc;

use crate::assets::AssetServer;
use crate::health::Readiness;
use crate::service::admin::AdminService;
use crate::service::auth::AuthService;
use crate::service::history::HistoryService;
use crate::service::my_links::MyLinksService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;
use crate::settings::Profile;

pub struct AppState {
    pub shorten: Arc<ShortenService>,
    pub redirect: Arc<RedirectService>,
    pub admin: Arc<AdminService>,
    pub auth: Arc<AuthService>,
    /// 登录用户自己的链接
    pub my_links: Arc<MyLinksService>,
    pub history: Arc<HistoryService>,
    /// 前端资源来源：开发读磁盘、生产读内嵌
    pub assets: Arc<AssetServer>,
    /// 依赖探活。测试替身没有真实依赖，置 `None` 时 `/readyz` 返回 503。
    pub readiness: Option<Arc<Readiness>>,
    pub profile: Profile,
    pub site_name: String,
    /// 反代部署的子路径前缀（如 `/s`），同源部署为空串
    pub api_base: String,
    /// 对外站点是 https 时给访客 cookie 加 `Secure`
    pub cookie_secure: bool,
}
