//! 创建短链接的输入模型与校验规则。
//!
//! 校验分成两层，位置不同：
//!
//! - **归一化**（去首尾空格、空串归 `None`、负数拒绝）在 `api::dto`，
//!   因为「多了个空格」「传了 -1」是输入格式问题，不属于业务规则；
//! - **合法性**（长度、字符集、上下界、时间先后）在这里，
//!   换一个入口（CLI、gRPC、定时任务）同样要守。
//!
//! 约定：进入本层的 `url` / `code` / `password` 已经是 trim 过的，
//! 本层只判断「剩下的这串东西合不合法」，不再改动它 —— 存储的就是校验过的原值。

use chrono::NaiveDateTime;
use serde_json::Value;

use super::code::is_valid_code;

/// 目标地址长度上限。
///
/// `long_url` 列是 `TEXT`，本身能存更多；真正的约束在出口：
/// 跳转时要把它塞进 `Location` 响应头，而反代（nginx 默认 8k）对头部长度有限制。
/// 2048 是「浏览器地址栏还能用」和「头部不会爆」的交点，超过它基本已是异常输入。
pub const MAX_URL_LEN: usize = 2048;

/// 访问密码长度上限。
///
/// 数据库列是 `VARCHAR(255)`，不校验的话超长密码会在写库时变成 1406 错误（500）。
/// 这里收得更紧：访问密码是给人记的短口令，不是密钥。
pub const MAX_PASSWORD_LEN: usize = 64;

/// 访问次数上限的上界。挡住 `4294967295` 这类天文数字 ——
/// 它等价于「不限制」，但会让计数、展示和统计都出现无意义的极值。
pub const MAX_VISITS_LIMIT: u32 = 100_000_000;

/// `ip_limit` / `ua_limit` 的序列化上限。
///
/// 这两个字段当前只落库、不参与判定，但不该因此成为「往库里塞任意大 JSON」的入口：
/// 一条链接挂上几百 KB 的规则，列表查询和备份都会被拖累。
const MAX_RULE_BYTES: usize = 4096;

/// 引号、反引号、反斜杠、分号：SQL 与命令注入里最常见的分隔与转义字符。
/// 访问密码是短口令，用不到这些。
const FORBIDDEN_PASSWORD_CHARS: [char; 5] = ['\'', '"', '`', '\\', ';'];

/// 注释与语句分隔的片段，同样只在注入载荷里有意义。
const FORBIDDEN_PASSWORD_SEQUENCES: [&str; 3] = ["--", "/*", "*/"];

/// 常见 SQL 关键字。
const SQL_KEYWORDS: [&str; 25] = [
    "select",
    "insert",
    "update",
    "delete",
    "drop",
    "truncate",
    "alter",
    "create",
    "grant",
    "revoke",
    "union",
    "join",
    "where",
    "from",
    "exec",
    "execute",
    "declare",
    "sleep",
    "benchmark",
    "outfile",
    "load_file",
    "information_schema",
    "sqlite_master",
    "or",
    "and",
];

/// 创建短链接的意图。
///
/// 与 HTTP 请求结构解耦：接口层负责把 JSON 转成它，
/// 因此换协议（gRPC / CLI）不需要改业务代码。
#[derive(Clone, Debug, Default)]
pub struct NewShortLink {
    pub url: String,
    /// 为空表示由系统生成
    pub code: Option<String>,
    /// 归属用户；`None` 表示匿名创建
    pub owner_id: Option<u64>,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    /// 已 trim 过；`Some("")` 表示「用户想设密码但填了空白」，校验会拒绝
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
    /// 跳转前先给一张中转确认页
    pub interstitial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    EmptyUrl,
    UrlTooLong,
    UnsupportedScheme,
    UrlContainsWhitespace,
    InvalidCode,
    EmptyPassword,
    PasswordTooLong,
    PasswordContainsWhitespace,
    PasswordHasForbiddenChars,
    PasswordLooksLikeSql,
    InvalidMaxVisits,
    ExpireInPast,
    InvalidRule,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptyUrl => write!(f, "url must not be empty"),
            ValidationError::UrlTooLong => {
                write!(f, "url must be at most {MAX_URL_LEN} characters")
            }
            ValidationError::UnsupportedScheme => {
                write!(f, "url must start with http:// or https://")
            }
            ValidationError::UrlContainsWhitespace => write!(f, "url must not contain whitespace"),
            ValidationError::InvalidCode => {
                write!(f, "custom code must be 1-10 chars of [A-Za-z0-9-_]")
            }
            ValidationError::EmptyPassword => write!(f, "password must not be empty"),
            ValidationError::PasswordTooLong => {
                write!(f, "password must be at most {MAX_PASSWORD_LEN} characters")
            }
            ValidationError::PasswordContainsWhitespace => {
                write!(f, "password must not contain whitespace")
            }
            ValidationError::PasswordHasForbiddenChars => {
                write!(
                    f,
                    "password must not contain quotes, backslashes or semicolons"
                )
            }
            ValidationError::PasswordLooksLikeSql => {
                write!(f, "password must not contain SQL keywords")
            }
            ValidationError::InvalidMaxVisits => {
                write!(f, "max_visits must be between 1 and {MAX_VISITS_LIMIT}")
            }
            ValidationError::ExpireInPast => write!(f, "expire_at must be in the future"),
            ValidationError::InvalidRule => write!(
                f,
                "ip_limit and ua_limit must be JSON objects or arrays under {MAX_RULE_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

impl NewShortLink {
    /// 在落库前一次性校验所有字段，避免半合法数据进入仓储层。
    ///
    /// `now` 由调用方传入（UTC），而不是在这里读时钟：既保证同一次请求里
    /// 各字段基于同一时刻，也让「过期时间刚好等于此刻」这类边界可测。
    pub fn validate(
        &self,
        max_code_length: usize,
        now: NaiveDateTime,
    ) -> Result<(), ValidationError> {
        self.validate_url()?;
        self.validate_code(max_code_length)?;
        self.validate_expire(now)?;
        self.validate_max_visits()?;
        self.validate_password()?;
        self.validate_rules()?;
        Ok(())
    }

    fn validate_url(&self) -> Result<(), ValidationError> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(ValidationError::EmptyUrl);
        }
        // 先判长度：一个几万字符的串去做后面的扫描没必要
        if url.chars().count() > MAX_URL_LEN {
            return Err(ValidationError::UrlTooLong);
        }
        if url.chars().any(char::is_whitespace) {
            return Err(ValidationError::UrlContainsWhitespace);
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ValidationError::UnsupportedScheme);
        }
        Ok(())
    }

    fn validate_code(&self, max_code_length: usize) -> Result<(), ValidationError> {
        match self.code.as_deref() {
            Some(code) if !is_valid_code(code, max_code_length) => {
                Err(ValidationError::InvalidCode)
            }
            _ => Ok(()),
        }
    }

    /// 过期时间必须晚于此刻。
    ///
    /// 用 `<=` 而不是 `<`：跳转时的判定就是「到期即失效」，
    /// 允许一个「刚好等于现在」的过期时间，等于建出来一条立刻 410 的链接。
    fn validate_expire(&self, now: NaiveDateTime) -> Result<(), ValidationError> {
        match self.expire_at {
            Some(at) if at <= now => Err(ValidationError::ExpireInPast),
            _ => Ok(()),
        }
    }

    /// 访问次数上限：`0` 与超上界都拒。
    ///
    /// `0` 次是「合法但无意义」—— 建出来就注定 410，除了占一个短码没有任何作用。
    fn validate_max_visits(&self) -> Result<(), ValidationError> {
        match self.max_visits {
            Some(visits) if !(1..=MAX_VISITS_LIMIT).contains(&visits) => {
                Err(ValidationError::InvalidMaxVisits)
            }
            _ => Ok(()),
        }
    }

    fn validate_password(&self) -> Result<(), ValidationError> {
        let Some(password) = self.password.as_deref() else {
            return Ok(());
        };

        // 纯空白等于「想设密码但没填」，说成空密码比说成含空格更准确
        if password.trim().is_empty() {
            return Err(ValidationError::EmptyPassword);
        }
        if password.chars().count() > MAX_PASSWORD_LEN {
            return Err(ValidationError::PasswordTooLong);
        }
        if password.chars().any(char::is_whitespace) {
            return Err(ValidationError::PasswordContainsWhitespace);
        }
        if password
            .chars()
            .any(|c| FORBIDDEN_PASSWORD_CHARS.contains(&c))
            || FORBIDDEN_PASSWORD_SEQUENCES
                .iter()
                .any(|seq| password.contains(seq))
        {
            return Err(ValidationError::PasswordHasForbiddenChars);
        }
        if looks_like_sql(password) {
            return Err(ValidationError::PasswordLooksLikeSql);
        }
        Ok(())
    }

    /// 规则字段只要「形状对、别太大」。
    ///
    /// 具体内容当前没有语义（`AccessPolicy` 还没读它们），所以不做结构校验 ——
    /// 那会把一个未定型的扩展点提前锁死。挡掉标量和超大对象就够。
    fn validate_rules(&self) -> Result<(), ValidationError> {
        for rule in [&self.ip_limit, &self.ua_limit] {
            let Some(value) = rule else { continue };

            if !value.is_object() && !value.is_array() {
                return Err(ValidationError::InvalidRule);
            }
            // 序列化一次是这里唯一能拿到字节数的办法，只在字段存在时才发生
            if value.to_string().len() > MAX_RULE_BYTES {
                return Err(ValidationError::InvalidRule);
            }
        }
        Ok(())
    }
}

/// 密码里出现 SQL 关键字（整词）就拒掉。
///
/// **这不是注入防线**：存取全部走参数化查询，密码永远不会拼进 SQL 文本。
/// 它挡的是「把一段 SQL 当密码粘进来」这类明显异常输入 —— 这种密码没人记得住，
/// 只会污染日志、导出与排障现场。
///
/// 逐词匹配而不是子串匹配：`candy` 里含 `and`，但按关键字拒掉它显然是错的。
fn looks_like_sql(password: &str) -> bool {
    password
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| {
            !token.is_empty()
                && SQL_KEYWORDS
                    .iter()
                    .any(|keyword| token.eq_ignore_ascii_case(keyword))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定「现在」，让过期时间的边界可复现。
    fn now() -> NaiveDateTime {
        NaiveDateTime::parse_from_str("2026-06-01 12:00:00", "%Y-%m-%d %H:%M:%S")
            .expect("valid datetime")
    }

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").expect("valid datetime")
    }

    fn valid() -> NewShortLink {
        NewShortLink {
            url: "https://example.com/a/b".into(),
            ..Default::default()
        }
    }

    fn check(link: &NewShortLink) -> Result<(), ValidationError> {
        link.validate(10, now())
    }

    #[test]
    fn accepts_http_and_https() {
        assert!(check(&valid()).is_ok());
    }

    #[test]
    fn rejects_empty_url() {
        let mut link = valid();
        link.url = "   ".into();
        assert_eq!(check(&link), Err(ValidationError::EmptyUrl));
    }

    #[test]
    fn rejects_other_schemes() {
        let mut link = valid();
        link.url = "ftp://example.com".into();
        assert_eq!(check(&link), Err(ValidationError::UnsupportedScheme));
    }

    #[test]
    fn rejects_overlong_custom_code() {
        let mut link = valid();
        link.code = Some("0123456789abc".into());
        assert_eq!(check(&link), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn rejects_blank_password() {
        let mut link = valid();
        link.password = Some("  ".into());
        assert_eq!(check(&link), Err(ValidationError::EmptyPassword));
    }

    #[test]
    fn rejects_whitespace_inside_url() {
        let mut link = valid();
        link.url = "https://example.com/a b".into();
        assert_eq!(check(&link), Err(ValidationError::UrlContainsWhitespace));
    }

    #[test]
    fn tolerates_url_surrounded_by_spaces() {
        let mut link = valid();
        link.url = "  https://example.com  ".into();
        assert!(check(&link).is_ok());
    }

    #[test]
    fn reports_url_problem_before_code_problem() {
        let mut link = valid();
        link.url = String::new();
        link.code = Some("bad code".into());
        // 一次只报第一个错误，顺序固定为 url → code → expire → max_visits → password
        assert_eq!(check(&link), Err(ValidationError::EmptyUrl));
    }

    #[test]
    fn accepts_code_at_max_length() {
        let mut link = valid();
        link.code = Some("0123456789".into());
        assert!(check(&link).is_ok());
    }

    #[test]
    fn rejects_code_one_over_max_length() {
        let mut link = valid();
        link.code = Some("01234567890".into());
        assert_eq!(check(&link), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn rejects_code_with_separator() {
        let mut link = valid();
        link.code = Some("a/b".into());
        assert_eq!(check(&link), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn accepts_code_with_dash_and_underscore() {
        let mut link = valid();
        link.code = Some("a-b_c".into());
        assert!(check(&link).is_ok());
    }

    #[test]
    fn accepts_link_without_optional_fields() {
        // 省略号构造即为「只给必填项」，用于确认可选字段不参与校验
        assert!(NewShortLink {
            url: "http://localhost:9981/x".into(),
            ..Default::default()
        }
        .validate(10, now())
        .is_ok());
    }

    #[test]
    fn accepts_url_at_max_length() {
        let mut link = valid();
        let prefix = "https://example.com/";
        link.url = format!("{prefix}{}", "a".repeat(MAX_URL_LEN - prefix.len()));
        assert_eq!(link.url.chars().count(), MAX_URL_LEN);
        assert!(check(&link).is_ok());
    }

    #[test]
    fn rejects_url_one_over_max_length() {
        let mut link = valid();
        let prefix = "https://example.com/";
        link.url = format!("{prefix}{}", "a".repeat(MAX_URL_LEN - prefix.len() + 1));
        assert_eq!(check(&link), Err(ValidationError::UrlTooLong));
    }

    #[test]
    fn rejects_expiry_in_the_past() {
        let mut link = valid();
        link.expire_at = Some(at("2026-06-01 11:59:59"));
        assert_eq!(check(&link), Err(ValidationError::ExpireInPast));
    }

    #[test]
    fn rejects_expiry_exactly_now() {
        // 等于此刻等于「建出来就过期」，跳转判定用的也是 `<=`
        let mut link = valid();
        link.expire_at = Some(now());
        assert_eq!(check(&link), Err(ValidationError::ExpireInPast));
    }

    #[test]
    fn accepts_expiry_one_second_ahead() {
        let mut link = valid();
        link.expire_at = Some(at("2026-06-01 12:00:01"));
        assert!(check(&link).is_ok());
    }

    #[test]
    fn rejects_zero_max_visits() {
        let mut link = valid();
        link.max_visits = Some(0);
        assert_eq!(check(&link), Err(ValidationError::InvalidMaxVisits));
    }

    #[test]
    fn accepts_first_and_last_allowed_max_visits() {
        for visits in [1, MAX_VISITS_LIMIT] {
            let mut link = valid();
            link.max_visits = Some(visits);
            assert!(check(&link).is_ok(), "{visits} 应当在允许范围内");
        }
    }

    #[test]
    fn rejects_max_visits_over_the_limit() {
        let mut link = valid();
        link.max_visits = Some(MAX_VISITS_LIMIT + 1);
        assert_eq!(check(&link), Err(ValidationError::InvalidMaxVisits));
    }

    #[test]
    fn accepts_password_at_max_length() {
        let mut link = valid();
        link.password = Some("a".repeat(MAX_PASSWORD_LEN));
        assert!(check(&link).is_ok());
    }

    #[test]
    fn rejects_password_one_over_max_length() {
        let mut link = valid();
        link.password = Some("a".repeat(MAX_PASSWORD_LEN + 1));
        assert_eq!(check(&link), Err(ValidationError::PasswordTooLong));
    }

    #[test]
    fn rejects_password_with_inner_space() {
        let mut link = valid();
        link.password = Some("abc def".into());
        assert_eq!(
            check(&link),
            Err(ValidationError::PasswordContainsWhitespace)
        );
    }

    #[test]
    fn rejects_password_with_tab_or_newline() {
        for password in ["abc\tdef", "abc\ndef", "abc\u{3000}def"] {
            let mut link = valid();
            link.password = Some(password.into());
            assert_eq!(
                check(&link),
                Err(ValidationError::PasswordContainsWhitespace),
                "空白字符都应被拒绝：{password:?}"
            );
        }
    }

    #[test]
    fn rejects_password_with_injection_punctuation() {
        for password in ["a'b", "a\"b", "a`b", "a\\b", "a;b", "a--b", "a/*b", "a*/b"] {
            let mut link = valid();
            link.password = Some(password.into());
            assert_eq!(
                check(&link),
                Err(ValidationError::PasswordHasForbiddenChars),
                "注入常用字符都应被拒绝：{password:?}"
            );
        }
    }

    #[test]
    fn rejects_password_that_is_a_sql_keyword() {
        for password in ["select", "DROP", "union", "or", "and", "information_schema"] {
            let mut link = valid();
            link.password = Some(password.into());
            assert_eq!(
                check(&link),
                Err(ValidationError::PasswordLooksLikeSql),
                "关键字应被拒绝：{password}"
            );
        }
    }

    #[test]
    fn rejects_password_containing_a_keyword_as_a_whole_word() {
        let mut link = valid();
        link.password = Some("foo-union-bar".into());
        assert_eq!(check(&link), Err(ValidationError::PasswordLooksLikeSql));
    }

    #[test]
    fn accepts_passwords_where_a_keyword_is_only_a_substring() {
        // `candy` 含 `and`、`order` 含 `or` —— 按整词匹配就不该误伤
        for password in ["candy", "order", "android", "selecta", "dropbox"] {
            let mut link = valid();
            link.password = Some(password.into());
            assert!(check(&link).is_ok(), "不该误伤：{password}");
        }
    }

    #[test]
    fn accepts_ordinary_passwords() {
        for password in ["s3cret", "p@ssw0rd!", "my-pass_2026", "中文密码"] {
            let mut link = valid();
            link.password = Some(password.into());
            assert!(check(&link).is_ok(), "正常密码应当通过：{password}");
        }
    }

    #[test]
    fn reports_expiry_problem_before_visits_problem() {
        let mut link = valid();
        link.expire_at = Some(at("2020-01-01 00:00:00"));
        link.max_visits = Some(0);
        assert_eq!(check(&link), Err(ValidationError::ExpireInPast));
    }

    #[test]
    fn reports_visits_problem_before_password_problem() {
        let mut link = valid();
        link.max_visits = Some(0);
        link.password = Some("a b".into());
        assert_eq!(check(&link), Err(ValidationError::InvalidMaxVisits));
    }

    #[test]
    fn accepts_object_and_array_rules() {
        for rule in [
            serde_json::json!({ "cidrs": ["10.0.0.0/8"] }),
            serde_json::json!(["10.0.0.0/8"]),
        ] {
            let mut link = valid();
            link.ip_limit = Some(rule.clone());
            link.ua_limit = Some(rule);
            assert!(check(&link).is_ok());
        }
    }

    #[test]
    fn rejects_scalar_rules() {
        for rule in [
            serde_json::json!("10.0.0.0/8"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let mut link = valid();
            link.ip_limit = Some(rule.clone());
            assert_eq!(check(&link), Err(ValidationError::InvalidRule), "{rule}");

            let mut link = valid();
            link.ua_limit = Some(rule.clone());
            assert_eq!(check(&link), Err(ValidationError::InvalidRule), "{rule}");
        }
    }

    #[test]
    fn rejects_oversized_rules() {
        let mut link = valid();
        // 用一堆短键把体积撑过上限
        let big: serde_json::Map<String, serde_json::Value> = (0..200)
            .map(|index| (format!("k{index:04}"), serde_json::json!("0123456789")))
            .collect();
        link.ua_limit = Some(serde_json::Value::Object(big));

        assert_eq!(check(&link), Err(ValidationError::InvalidRule));
    }
}
