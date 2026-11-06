//! 极简模板填充：`{{name}}` 取值，`{{#name}}…{{/name}}` 按值有无决定留不留。
//!
//! 页面只有两张、占位符是个位数，引入模板引擎换来的只是多一层抽象和一套
//! 需要学的语法。这里的实现不到百行，行为完全可测。
//!
//! 三条硬规则：
//! - 替换值一律 HTML 转义。短码、目标地址都来自外部输入，不能直接拼进文档。
//! - 单遍扫描。替换进去的值里若带有 `{{…}}`，不会被再展开一次。
//! - 模板写错（占位符没闭合）时原样输出，留痕而不是悄悄渲染成空白。

/// 填充模板。`values` 中同名的项以第一条为准。
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 128);
    render_into(&mut out, template, values);
    out
}

fn render_into(out: &mut String, source: &str, values: &[(&str, &str)]) {
    let mut rest = source;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);

        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // 落单的 `{{`：原样输出，不要在运行时吞掉
            out.push_str(&rest[start..]);
            return;
        };

        let tag = &after[..end];
        let tail = &after[end + 2..];

        match tag.strip_prefix('#') {
            Some(name) => match find_block_end(tail, name) {
                Some((body, after_block)) => {
                    // 有值就渲染块内容（块内的占位符同样要替换），没值整块丢弃
                    if has_value(values, name) {
                        render_into(out, body, values);
                    }
                    rest = after_block;
                }
                None => {
                    // 开块没有配对的闭块：留痕
                    out.push_str("{{");
                    out.push_str(tag);
                    out.push_str("}}");
                    rest = tail;
                }
            },
            None => {
                match values.iter().find(|(name, _)| name == &tag) {
                    Some((_, value)) => out.push_str(&escape_html(value)),
                    None => {
                        out.push_str("{{");
                        out.push_str(tag);
                        out.push_str("}}");
                    }
                }
                rest = tail;
            }
        }
    }

    out.push_str(rest);
}

/// 找到与 `{{#name}}` 配对的 `{{/name}}`，返回块体与其后的剩余部分。
///
/// 支持同名块嵌套，靠深度计数配对 —— 否则内层的闭块会被当成本块的结束。
fn find_block_end<'a>(source: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let mut depth = 1usize;
    let mut cursor = 0usize;

    while let Some(offset) = source[cursor..].find("{{") {
        let start = cursor + offset;
        let inner = &source[start + 2..];
        let end = inner.find("}}")?;
        let tag = &inner[..end];
        let next = start + 2 + end + 2;

        // 只认 `#name` 与 `/name`，`{{name}}` 是取值不是块标记
        if tag.len() > 1 && tag[1..] == *name {
            match &tag[..1] {
                "#" => depth += 1,
                "/" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&source[..start], &source[next..]));
                    }
                }
                _ => {}
            }
        }

        cursor = next;
    }

    None
}

/// 条件块的判据：键存在且值非空。
///
/// 调用方因此可以用 `""` 表达「不要这块」，不必再定义一个布尔数组。
fn has_value(values: &[(&str, &str)], name: &str) -> bool {
    values
        .iter()
        .any(|(key, value)| *key == name && !value.is_empty())
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
        assert_eq!(
            fill("{{url}}", &[("url", "<script>alert(1)</script>")]),
            "&lt;script&gt;alert(1)&lt;/script&gt;"
        );
    }

    #[test]
    fn inserts_each_value_once() {
        // 值里若有另一个占位符，不能再被展开
        assert_eq!(
            fill(
                "{{code}}|{{url}}",
                &[("code", "abc"), ("url", "https://e.test/{{code}}")]
            ),
            "abc|https://e.test/{{code}}"
        );
    }

    #[test]
    fn keeps_unknown_placeholders_visible() {
        assert_eq!(fill("[{{nope}}]", &[("code", "abc")]), "[{{nope}}]");
    }

    #[test]
    fn keeps_unclosed_block_visible() {
        // 模板少写一个闭块时应该看得出来，而不是渲染出一段空文档
        assert_eq!(fill("a{{#x}}b", &[("x", "1")]), "a{{#x}}b");
    }

    #[test]
    fn keeps_block_with_non_empty_value() {
        assert_eq!(
            fill(
                "<p>{{#form}}</p>{{x}}<p>{{/form}}</p>",
                &[("form", "1"), ("x", "hi")]
            ),
            "<p></p>hi<p></p>"
        );
    }

    #[test]
    fn drops_block_with_empty_or_missing_value() {
        let template = "<p>{{#form}}secret{{/form}}</p>";

        assert_eq!(fill(template, &[("form", "")]), "<p></p>");
        assert_eq!(fill(template, &[]), "<p></p>");
    }

    #[test]
    fn escapes_values_inside_blocks() {
        assert_eq!(
            fill(
                "{{#form}}{{name}}{{/form}}",
                &[("form", "1"), ("name", "<b>")]
            ),
            "&lt;b&gt;"
        );
    }

    #[test]
    fn pairs_nested_blocks_of_the_same_name() {
        let template = "{{#a}}1{{#a}}2{{/a}}3{{/a}}";

        assert_eq!(fill(template, &[("a", "x")]), "123");
        assert_eq!(fill(template, &[("a", "")]), "");
    }

    #[test]
    fn treats_lookup_tag_as_a_value_not_a_block() {
        // `{{a}}` 不能把后面的 `{{/a}}` 当自己的闭标记
        assert_eq!(fill("{{a}}{{/a}}", &[("a", "v")]), "v{{/a}}");
    }
}
