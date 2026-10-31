//! 创建短链接的输入模型与校验规则。

use chrono::NaiveDateTime;
use serde_json::Value;

use super::code::is_valid_code;

/// 创建短链接的意图。
///
/// 与 HTTP 请求结构解耦：接口层负责把 JSON 转成它，
/// 因此换协议（gRPC / CLI）不需要改业务代码。
#[derive(Clone, Debug, Default)]
pub struct NewShortLink {
    pub url: String,
    /// 为空表示由系统生成
    pub code: Option<String>,
    pub expire_at: Option<NaiveDateTime>,
    pub max_visits: Option<u32>,
    pub password: Option<String>,
    pub ip_limit: Option<Value>,
    pub ua_limit: Option<Value>,
    /// 跳转前先给一张中转确认页
    pub interstitial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    EmptyUrl,
    UnsupportedScheme,
    UrlContainsWhitespace,
    InvalidCode,
    EmptyPassword,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::EmptyUrl => write!(f, "url must not be empty"),
            ValidationError::UnsupportedScheme => {
                write!(f, "url must start with http:// or https://")
            }
            ValidationError::UrlContainsWhitespace => write!(f, "url must not contain whitespace"),
            ValidationError::InvalidCode => {
                write!(f, "custom code must be 1-10 chars of [A-Za-z0-9-_]")
            }
            ValidationError::EmptyPassword => write!(f, "password must not be empty"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl NewShortLink {
    /// 在落库前一次性校验所有字段，避免半合法数据进入仓储层。
    pub fn validate(&self, max_code_length: usize) -> Result<(), ValidationError> {
        self.validate_url()?;
        self.validate_code(max_code_length)?;
        self.validate_password()?;
        Ok(())
    }

    fn validate_url(&self) -> Result<(), ValidationError> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(ValidationError::EmptyUrl);
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
            Some(code) if !is_valid_code(code, max_code_length) => Err(ValidationError::InvalidCode),
            _ => Ok(()),
        }
    }

    fn validate_password(&self) -> Result<(), ValidationError> {
        match self.password.as_deref() {
            Some(password) if password.trim().is_empty() => Err(ValidationError::EmptyPassword),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> NewShortLink {
        NewShortLink {
            url: "https://example.com/a/b".into(),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_http_and_https() {
        assert!(valid().validate(10).is_ok());
    }

    #[test]
    fn rejects_empty_url() {
        let mut link = valid();
        link.url = "   ".into();
        assert_eq!(link.validate(10), Err(ValidationError::EmptyUrl));
    }

    #[test]
    fn rejects_other_schemes() {
        let mut link = valid();
        link.url = "ftp://example.com".into();
        assert_eq!(link.validate(10), Err(ValidationError::UnsupportedScheme));
    }

    #[test]
    fn rejects_overlong_custom_code() {
        let mut link = valid();
        link.code = Some("0123456789abc".into());
        assert_eq!(link.validate(10), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn rejects_blank_password() {
        let mut link = valid();
        link.password = Some("  ".into());
        assert_eq!(link.validate(10), Err(ValidationError::EmptyPassword));
    }

    #[test]
    fn rejects_whitespace_inside_url() {
        let mut link = valid();
        link.url = "https://example.com/a b".into();
        assert_eq!(link.validate(10), Err(ValidationError::UrlContainsWhitespace));
    }

    #[test]
    fn tolerates_url_surrounded_by_spaces() {
        let mut link = valid();
        link.url = "  https://example.com  ".into();
        assert!(link.validate(10).is_ok());
    }

    #[test]
    fn reports_url_problem_before_code_problem() {
        let mut link = valid();
        link.url = String::new();
        link.code = Some("bad code".into());
        // 一次只报第一个错误，顺序固定为 url → code → password
        assert_eq!(link.validate(10), Err(ValidationError::EmptyUrl));
    }

    #[test]
    fn accepts_code_at_max_length() {
        let mut link = valid();
        link.code = Some("0123456789".into());
        assert!(link.validate(10).is_ok());
    }

    #[test]
    fn rejects_code_one_over_max_length() {
        let mut link = valid();
        link.code = Some("01234567890".into());
        assert_eq!(link.validate(10), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn rejects_code_with_separator() {
        let mut link = valid();
        link.code = Some("a/b".into());
        assert_eq!(link.validate(10), Err(ValidationError::InvalidCode));
    }

    #[test]
    fn accepts_code_with_dash_and_underscore() {
        let mut link = valid();
        link.code = Some("a-b_c".into());
        assert!(link.validate(10).is_ok());
    }

    #[test]
    fn accepts_link_without_optional_fields() {
        // 省略号构造即为「只给必填项」，用于确认可选字段不参与校验
        assert!(NewShortLink {
            url: "http://localhost:9981/x".into(),
            ..Default::default()
        }
        .validate(10)
        .is_ok());
    }
}
