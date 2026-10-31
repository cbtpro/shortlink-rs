//! 中转页：把目标地址填进一张静态模板。
//!
//! 页面里没有脚本，点「继续访问」直接去目标站点：少一次往返，
//! 目标地址也不必经过一个可被枚举的跳转端点。

use actix_web::http::header;
use actix_web::HttpResponse;

use crate::assets::AssetServer;

/// 中转页模板名，与 `web/` 下的文件对应。
const TEMPLATE: &str = "interstitial.html";

/// 渲染中转页。`target` 来自数据库，必须当作不可信输入处理。
pub fn render(assets: &AssetServer, code: &str, target: &str) -> HttpResponse {
    let Some(template) = assets.get(TEMPLATE).and_then(|asset| asset.as_text()) else {
        // 启动时已校验模板存在，这里只可能是运行期被删
        log::error!("interstitial template `{TEMPLATE}` is missing");
        return HttpResponse::InternalServerError().finish();
    };

    let body = fill(
        &template,
        &[("code", code), ("host", host_of(target)), ("url", target)],
    );

    HttpResponse::Ok()
        // 链接状态随时可能变化（过期、删除），中转页不能被缓存后继续放行
        .insert_header((header::CACHE_CONTROL, "no-store"))
        .content_type("text/html; charset=utf-8")
        .body(body)
}

/// 单遍替换 `{{name}}`，替换进来的值一律转义。
///
/// 单遍是必须的：若按占位符逐个 `replace`，先插入的值里如果含有另一个占位符，
/// 就会被再展开一次。
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 128);
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);

        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // 模板自身写错了：原样输出，不要在运行时悄悄吞掉
            out.push_str(&rest[start..]);
            return out;
        };

        match values.iter().find(|(name, _)| *name == &after[..end]) {
            Some((_, value)) => out.push_str(&escape_html(value)),
            None => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }

        rest = &after[end + 2..];
    }

    out.push_str(rest);
    out
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
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
    fn escapes_markup_in_values() {
        let page = fill(
            "<a href=\"{{url}}\">{{url}}</a>",
            &[("url", "https://e.test/\"onmouseover=\"alert(1)")],
        );

        assert!(!page.contains("onmouseover=\"alert"), "属性必须被截断");
        assert!(page.contains("&quot;onmouseover=&quot;alert(1)"));
    }

    #[test]
    fn escapes_script_tags() {
        let page = fill("{{url}}", &[("url", "<script>alert(1)</script>")]);
        assert_eq!(page, "&lt;script&gt;alert(1)&lt;/script&gt;");
    }

    #[test]
    fn inserts_each_value_once() {
        // 值里若有另一个占位符，不能再被展开
        let page = fill(
            "{{code}}|{{url}}",
            &[("code", "abc"), ("url", "https://e.test/{{code}}")],
        );
        assert_eq!(page, "abc|https://e.test/{{code}}");
    }

    #[test]
    fn keeps_unknown_placeholders_visible() {
        // 模板写错时留痕，而不是渲染出一个空字符串
        assert_eq!(fill("[{{nope}}]", &[("code", "abc")]), "[{{nope}}]");
    }

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
            ],
        );

        assert!(page.contains("example.com"));
        assert!(page.contains("href=\"https://example.com/a\""));
        assert!(page.contains("abc"));
        assert!(!page.contains("{{"), "不应残留未替换的占位符");
    }
}
