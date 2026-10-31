//! 账号：用户实体与注册输入校验。

use std::fmt;

use chrono::NaiveDateTime;
use sqlx::FromRow;

/// 口令最短长度。低于这个长度，PBKDF2 也只是在给弱口令做包装。
pub const MIN_PASSWORD_LEN: usize = 8;
/// 上限是拒绝服务防护：PBKDF2 的耗时随输入长度线性增长。
pub const MAX_PASSWORD_LEN: usize = 128;
/// 与 `users.email VARCHAR(254)` 对齐（RFC 5321 规定的最大长度）。
const MAX_EMAIL_LEN: usize = 254;
/// 邮箱 @ 前的最大长度，传统限制，超长的一般是垃圾注册。
const MAX_LOCAL_LEN: usize = 64;
/// 与 `users.display_name VARCHAR(32)` 对齐。
pub const MAX_DISPLAY_NAME_LEN: usize = 32;

/// 用户实体，对应 MySQL 表 `users`。
///
/// `password_hash` 留在这里是因为用例层要用它做校验；它从不出现在任何 DTO 里，
/// 由 `api::auth::UserDto` 的字段列表来保证（那里没有这个字段）。
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct User {
    pub id: u64,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub created_at: NaiveDateTime,
}

/// 注册输入。
#[derive(Clone, Debug)]
pub struct NewUser {
    pub email: String,
    pub password: String,
    /// 为空时取邮箱 @ 前的部分
    pub display_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountError {
    InvalidEmail,
    PasswordTooShort,
    PasswordTooLong,
    PasswordBlank,
    DisplayNameTooLong,
    /// 昵称里的换行、制表符会把页面排版搞乱，也常是复制粘贴带进来的脏数据
    DisplayNameHasControlChars,
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountError::InvalidEmail => write!(f, "email is not a valid address"),
            AccountError::PasswordTooShort => {
                write!(f, "password must be at least {MIN_PASSWORD_LEN} characters")
            }
            AccountError::PasswordTooLong => {
                write!(f, "password must be at most {MAX_PASSWORD_LEN} characters")
            }
            AccountError::PasswordBlank => write!(f, "password must not be blank"),
            AccountError::DisplayNameTooLong => write!(
                f,
                "display name must be at most {MAX_DISPLAY_NAME_LEN} characters"
            ),
            AccountError::DisplayNameHasControlChars => {
                write!(f, "display name must not contain control characters")
            }
        }
    }
}

impl std::error::Error for AccountError {}

impl NewUser {
    /// 规范化：邮箱去空白并转小写（大小写不敏感是邮箱的通行事实），
    /// 昵称去空白。口令原样保留 —— 去空白会让用户设的和存的不是同一个。
    pub fn normalized(mut self) -> Self {
        self.email = self.email.trim().to_ascii_lowercase();
        self.display_name = self
            .display_name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        self
    }

    pub fn validate(&self) -> Result<(), AccountError> {
        if !is_valid_email(&self.email) {
            return Err(AccountError::InvalidEmail);
        }

        if self.password.trim().is_empty() {
            return Err(AccountError::PasswordBlank);
        }

        // 按字符数而不是字节数算长度，中文口令不该被当成「超长」
        let length = self.password.chars().count();
        if length < MIN_PASSWORD_LEN {
            return Err(AccountError::PasswordTooShort);
        }
        if length > MAX_PASSWORD_LEN {
            return Err(AccountError::PasswordTooLong);
        }

        if let Some(name) = self.display_name.as_deref() {
            if name.chars().any(char::is_control) {
                return Err(AccountError::DisplayNameHasControlChars);
            }
            if name.chars().count() > MAX_DISPLAY_NAME_LEN {
                return Err(AccountError::DisplayNameTooLong);
            }
        }

        Ok(())
    }

    /// 最终写库的昵称：没填就用邮箱 @ 前面那段。
    pub fn resolve_display_name(&self) -> String {
        match self.display_name.as_deref() {
            Some(name) => name.to_string(),
            None => self
                .email
                .split_once('@')
                .map_or_else(|| self.email.clone(), |(local, _)| local.to_string()),
        }
    }
}

/// 邮箱合法性。
///
/// 刻意保守：只接受 `local@domain.tld` 这种形态，且要求 ASCII。
/// 用正则去追 RFC 5322 得不偿失 —— 真正能证明邮箱可用的只有「发一封信过去」，
/// 这里的目标是挡住明显的垃圾输入，不是做语法完备的解析器。
fn is_valid_email(email: &str) -> bool {
    if email.len() > MAX_EMAIL_LEN || !email.is_ascii() {
        return false;
    }

    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };

    if local.is_empty() || local.len() > MAX_LOCAL_LEN {
        return false;
    }

    // 只允许一个 @，且各段都不含空白与首尾的点
    if domain.contains('@') || domain.starts_with('.') || domain.ends_with('.') {
        return false;
    }

    if domain.contains(char::is_whitespace) || local.contains(char::is_whitespace) {
        return false;
    }

    // 域名必须有至少一个点，且顶级域不少于两位
    match domain.rsplit_once('.') {
        Some((host, tld)) => !host.is_empty() && tld.len() >= 2,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signup(email: &str, password: &str) -> NewUser {
        NewUser {
            email: email.into(),
            password: password.into(),
            display_name: None,
        }
    }

    #[test]
    fn accepts_common_addresses() {
        for email in [
            "a@b.co",
            "first.last@sub.example.com",
            "user+tag@example.org",
            "user_name@example-domain.com",
        ] {
            assert!(is_valid_email(email), "{email} 应被接受");
        }
    }

    #[test]
    fn rejects_malformed_addresses() {
        for email in [
            "",
            "@example.com",
            "user@",
            "user@example",
            "user@@example.com",
            "user name@example.com",
            "user@exa mple.com",
            "user@.example.com",
            "user@example.com.",
            "中文@example.com",
            "user@example.c",
        ] {
            assert!(!is_valid_email(email), "{email} 应被拒绝");
        }
    }

    #[test]
    fn normalizes_email_and_display_name() {
        let user = NewUser {
            email: "  User@Example.COM ".into(),
            password: "secret123".into(),
            display_name: Some("  小明  ".into()),
        }
        .normalized();

        assert_eq!(user.email, "user@example.com");
        assert_eq!(user.display_name.as_deref(), Some("小明"));
    }

    #[test]
    fn keeps_password_verbatim() {
        // 口令首尾空格是用户有意为之，去掉会让人再也登录不上
        let user = NewUser {
            email: "a@b.co".into(),
            password: "  secret123  ".into(),
            display_name: None,
        }
        .normalized();

        assert_eq!(user.password, "  secret123  ");
    }

    #[test]
    fn blank_display_name_falls_back_to_email() {
        let user = NewUser {
            email: "someone@example.com".into(),
            password: "secret123".into(),
            display_name: Some("   ".into()),
        }
        .normalized();

        assert_eq!(user.display_name, None);
        assert_eq!(user.resolve_display_name(), "someone");
    }

    #[test]
    fn enforces_password_length_by_characters() {
        assert_eq!(
            signup("a@b.co", "1234567").validate(),
            Err(AccountError::PasswordTooShort)
        );
        assert!(signup("a@b.co", "12345678").validate().is_ok());

        // 32 个汉字算 32 个字符，不是 96 字节
        let chinese = "密".repeat(32);
        assert!(signup("a@b.co", &chinese).validate().is_ok());

        assert_eq!(
            signup("a@b.co", &"a".repeat(MAX_PASSWORD_LEN + 1)).validate(),
            Err(AccountError::PasswordTooLong)
        );
    }

    #[test]
    fn rejects_blank_password_even_if_long_enough() {
        assert_eq!(
            signup("a@b.co", "          ").validate(),
            Err(AccountError::PasswordBlank)
        );
    }

    #[test]
    fn rejects_overlong_display_name() {
        let user = NewUser {
            email: "a@b.co".into(),
            password: "secret123".into(),
            display_name: Some("名".repeat(MAX_DISPLAY_NAME_LEN + 1)),
        };

        assert_eq!(user.validate(), Err(AccountError::DisplayNameTooLong));
    }

    #[test]
    fn rejects_display_name_with_control_characters() {
        for name in ["a\nb", "a\tb", "a\u{0}b"] {
            let user = NewUser {
                email: "a@b.co".into(),
                password: "secret123".into(),
                // 首尾空白会被 normalized 去掉，这里的换行在中间，去不掉
                display_name: Some(name.into()),
            };

            assert_eq!(
                user.validate(),
                Err(AccountError::DisplayNameHasControlChars),
                "{name:?} 应被拒绝"
            );
        }
    }

    #[test]
    fn validation_order_is_stable() {
        // 邮箱不对时先报邮箱，用户不用逐个试
        assert_eq!(
            signup("nope", "1").validate(),
            Err(AccountError::InvalidEmail)
        );
    }
}
