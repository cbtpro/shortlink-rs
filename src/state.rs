//! 应用级共享状态：由组合根构建，处理器只读。

use std::path::PathBuf;
use std::sync::Arc;

use crate::assets::AssetServer;
use crate::health::Readiness;
use crate::service::auth::AuthService;
use crate::service::console::ConsoleService;
use crate::service::history::HistoryService;
use crate::service::my_links::MyLinksService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;
use crate::settings::Profile;

pub struct AppState {
    pub shorten: Arc<ShortenService>,
    pub redirect: Arc<RedirectService>,
    /// 后台控制台：普通用户看自己的，管理员看全站并管账号
    pub console: Arc<ConsoleService>,
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
    /// 初始化生成的管理员口令文件。管理员改完密码就删掉，避免旧口令留在磁盘上。
    pub admin_password_file: Option<PathBuf>,
}

impl AppState {
    /// 删除初始化口令文件。删不掉只记日志：文件里那串口令此刻已经作废，
    /// 留着它最多是碍眼，不该让一次成功的改密因此返回错误。
    pub fn discard_admin_password_file(&self) {
        let Some(path) = self.admin_password_file.as_deref() else {
            return;
        };

        match std::fs::remove_file(path) {
            Ok(()) => log::info!("removed initial admin password file {}", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => log::warn!(
                "failed to remove initial admin password file {}: {err}",
                path.display()
            ),
        }
    }
}
