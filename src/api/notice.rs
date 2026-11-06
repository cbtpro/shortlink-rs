//! 跳转失败时的状态页。
//!
//! 过期、次数用尽、需要密码、密码错误、短码不存在 —— 这几种状态都会出现在
//! 已经分享出去的链接上，点开的是浏览器，不是 API 客户端。给访客一段 JSON
//! 既读不懂也不知道下一步做什么，所以响应体换成一张能和首页对上的页面。
//!
//! 状态码保持原有语义（410 / 403 / 404）：页面是给人的，状态码是给缓存、
//! 爬虫和监控的，两者不该互相将就。

use actix_web::http::{header, StatusCode};
use actix_web::HttpResponse;

use super::template::fill;
use crate::assets::AssetServer;

/// 状态页模板名，与 `web/` 下的文件对应。
const TEMPLATE: &str = "notice.html";

/// 一张状态页。字段与模板里的占位符一一对应。
#[derive(Clone, Copy, Debug)]
pub struct Notice {
    pub status: StatusCode,
    /// 顶部小标签：先告诉访客这是哪一类问题
    pub kicker: &'static str,
    pub title: &'static str,
    pub lead: &'static str,
    /// 是否渲染密码输入表单
    pub asks_password: bool,
}

impl Notice {
    pub fn expired() -> Self {
        Self {
            status: StatusCode::GONE,
            kicker: "链接已失效",
            title: "这条短链已经过期",
            lead: "创建者给它设了有效期，现在已经超过。请联系分享者换一条新的链接。",
            asks_password: false,
        }
    }

    pub fn exhausted() -> Self {
        Self {
            status: StatusCode::GONE,
            kicker: "链接已失效",
            title: "访问次数已达上限",
            lead: "创建者给这条短链设了访问次数上限，名额已经用完。请联系分享者换一条新的链接。",
            asks_password: false,
        }
    }

    pub fn password_required() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            kicker: "需要验证",
            title: "请输入访问密码",
            lead: "这条短链设置了访问密码。输入正确后即可前往目标页面。",
            asks_password: true,
        }
    }

    pub fn password_rejected() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            kicker: "需要验证",
            title: "密码不正确",
            lead: "请确认大小写与字符后再试一次。忘记了可以联系分享者索取。",
            asks_password: true,
        }
    }

    pub fn missing() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            kicker: "链接不存在",
            title: "没有找到这个短码",
            lead: "它可能从未被创建，也可能已被创建者删除。检查一下地址里有没有多余的字符。",
            asks_password: false,
        }
    }
}

/// 渲染状态页。`code` 来自 URL，必须当作不可信输入处理。
pub fn render(assets: &AssetServer, notice: Notice, code: &str) -> HttpResponse {
    let Some(template) = assets.get(TEMPLATE).and_then(|asset| asset.as_text()) else {
        // 启动时已校验模板存在，这里只可能是运行期被删
        log::error!("notice template `{TEMPLATE}` is missing");
        return HttpResponse::InternalServerError().finish();
    };

    let body = fill(
        &template,
        &[
            ("kicker", notice.kicker),
            ("title", notice.title),
            ("lead", notice.lead),
            ("code", code),
            // 条件块只看值非空，不关心具体内容
            ("password", if notice.asks_password { "1" } else { "" }),
        ],
    );

    HttpResponse::build(notice.status)
        // 链接状态随时会变（可能刚被续期），这一页不能被缓存后继续拦人
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .insert_header((header::REFERRER_POLICY, "no-referrer"))
        .content_type("text/html; charset=utf-8")
        .body(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::AssetsMode;
    use std::path::PathBuf;

    fn assets() -> AssetServer {
        AssetServer::new(AssetsMode::Embedded, PathBuf::from("web"), false)
            .expect("内嵌资源总是可用")
    }

    async fn body_of(notice: Notice, code: &str) -> String {
        let response = render(&assets(), notice, code);
        assert_eq!(response.status(), notice.status);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );

        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("body should be readable");
        String::from_utf8(bytes.to_vec()).expect("page is utf-8")
    }

    #[actix_web::test]
    async fn renders_expired_page() {
        let body = body_of(Notice::expired(), "old").await;

        assert!(body.contains("这条短链已经过期"));
        assert!(body.contains("<code>old</code>"));
        assert!(!body.contains("{{"), "不应残留未替换的占位符");
        assert!(!body.contains("<form"), "过期页不该要密码");
    }

    #[actix_web::test]
    async fn renders_exhausted_page_with_gone_status() {
        let body = body_of(Notice::exhausted(), "lim").await;

        assert!(body.contains("访问次数已达上限"));
        assert_eq!(Notice::exhausted().status, StatusCode::GONE);
    }

    #[actix_web::test]
    async fn renders_password_form() {
        let body = body_of(Notice::password_required(), "prot").await;

        assert!(body.contains("请输入访问密码"));
        assert!(body.contains("<form"));
        assert!(body.contains("name=\"password\""));
        assert!(body.contains("type=\"password\""));
        assert_eq!(Notice::password_required().status, StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn renders_rejection_hint() {
        let body = body_of(Notice::password_rejected(), "prot").await;

        assert!(body.contains("密码不正确"));
        assert!(body.contains("<form"), "密码错了要能再试一次");
    }

    #[actix_web::test]
    async fn renders_missing_page_not_found() {
        let body = body_of(Notice::missing(), "nope").await;

        assert!(body.contains("没有找到这个短码"));
        assert_eq!(Notice::missing().status, StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn escapes_code_in_the_page() {
        let body = body_of(Notice::missing(), "<script>alert(1)</script>").await;

        assert!(!body.contains("<script>alert(1)</script>"));
        assert!(body.contains("&lt;script&gt;"));
    }

    #[test]
    fn every_page_links_bundled_assets() {
        // 状态页和首页共用样式，路径写错就只有点开失效链接才发现
        let assets = assets();
        let html = assets
            .get(TEMPLATE)
            .and_then(|asset| asset.as_text())
            .expect("状态页模板必须打包进二进制");

        for path in ["assets/css/base.css", "assets/css/notice.css"] {
            assert!(html.contains(path), "模板应引用 {path}");
            assert!(assets.get(path).is_some(), "{path} 没打进二进制");
        }
    }
}
