//! 中转页：把目标地址填进一张静态模板。
//!
//! 页面里没有脚本。点「继续访问」会带着票据回到 `GET /{code}`，由服务端换一次
//! 302 —— 中转页只是确认页，不算一次访问，这一跳才算。代价是比直连多一次往返，
//! 换来的是计数口径与「访问者真的到达了」一致。

use actix_web::http::header;
use actix_web::HttpResponse;

use crate::api::template::fill;
use crate::assets::AssetServer;

/// 中转页模板名，与 `web/` 下的文件对应。
const TEMPLATE: &str = "interstitial.html";

/// 渲染中转页。
///
/// `target` 来自数据库，必须当作不可信输入处理；
/// `continue_url` 是「继续访问」的目标 —— 通常是带票据的回跳地址，
/// 只有访问者真的点下去，服务端才知道这次跳转发生了、才会计数。
pub fn render(assets: &AssetServer, code: &str, target: &str, continue_url: &str) -> HttpResponse {
    let Some(template) = assets.get(TEMPLATE).and_then(|asset| asset.as_text()) else {
        // 启动时已校验模板存在，这里只可能是运行期被删
        log::error!("interstitial template `{TEMPLATE}` is missing");
        return HttpResponse::InternalServerError().finish();
    };

    let body = fill(
        &template,
        &[
            ("code", code),
            ("host", host_of(target)),
            ("url", target),
            ("next", continue_url),
        ],
    );

    HttpResponse::Ok()
        // 链接状态随时可能变化（过期、删除），中转页不能被缓存后继续放行
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .content_type("text/html; charset=utf-8")
        .body(body)
}

/// 取展示用的主机名。取不到就退回整串，反正是当纯文本显示的。
fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_host_from_url() {
        assert_eq!(host_of("https://example.com/a?b=1"), "example.com");
        assert_eq!(host_of("http://127.0.0.1:9981/x"), "127.0.0.1:9981");
        assert_eq!(host_of("https://example.com"), "example.com");
        assert_eq!(host_of("https://example.com?q=1"), "example.com");
        assert_eq!(host_of("https://example.com#top"), "example.com");
    }

    #[test]
    fn renders_template_with_target_and_host() {
        let assets = AssetServer::new(
            crate::settings::AssetsMode::Embedded,
            std::path::PathBuf::from("web"),
            false,
        )
        .expect("内嵌资源总是可用");
        let template = assets
            .get(TEMPLATE)
            .and_then(|asset| asset.as_text())
            .expect("中转页模板必须打包进二进制");

        let page = fill(
            &template,
            &[
                ("code", "abc"),
                ("host", "example.com"),
                ("url", "https://example.com/a"),
                ("next", "/abc?confirm=deadbeef"),
            ],
        );

        assert!(page.contains("example.com"));
        assert!(
            page.contains("https://example.com/a"),
            "目标地址仍要在页面上写清楚"
        );
        assert!(page.contains("abc"));
        assert!(!page.contains("{{"), "不应残留未替换的占位符");

        // 「继续访问」必须走带票据的回跳地址，直连目标就没法计数了
        assert!(page.contains("href=\"/abc?confirm=deadbeef\""));
        assert!(
            !page.contains("href=\"https://example.com/a\""),
            "继续访问不能直连目标：那样服务端就统计不到这次跳转"
        );
    }
}
