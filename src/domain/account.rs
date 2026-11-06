//! 账号：用户实体、角色与注册输入校验。

use std::fmt;

use chrono::NaiveDateTime;

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

/// 账号角色。
///
/// 只有两种：管理员能看所有人，普通用户只能看自己。角色存在库里而不是配置里，
/// 是因为「谁是谁的管理员」属于数据，改一次不该重启服务。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    #[default]
    User,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Admin => "admin",
        }
    }

    /// 解析库里的值。**未知值一律当普通用户**：脏数据最多让人少一点权限，
    /// 反过来（未知即管理员）就是白送一个后台。
    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("admin") {
            Self::Admin
        } else {
            Self::User
        }
    }

    pub fn is_admin(self) -> bool {
        self == Self::Admin
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 账号状态。禁用后不能登录，已有会话也立刻失效。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountStatus {
    #[default]
    Active,
    Disabled,
}

impl AccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }

    /// 与 [`Role::parse`] 相反，这里对未知值**一律当禁用**：
    /// 认不出的状态不能放行，否则库被写坏就等于人人可登录。
    pub fn parse(raw: &str) -> Self {
        if raw.trim().eq_ignore_ascii_case("active") {
            Self::Active
        } else {
            Self::Disabled
        }
    }

    pub fn is_active(self) -> bool {
        self == Self::Active
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 建号时决定的访问级别。
///
/// 单独一个结构而不是塞进 [`NewUser`]：注册输入来自用户，角色来自系统。
/// 混在一起，早晚会有人把请求体里的 `role` 直接透传下来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub role: Role,
    /// 密码是系统生成的临时口令，必须先改才能用
    pub must_change_password: bool,
}

impl Access {
    /// 自助注册出来的账号。
    pub const USER: Self = Self {
        role: Role::User,
        must_change_password: false,
    };

    /// 初始化生成的管理员：密码在文件里，首次登录必须改。
    pub const ADMIN_WITH_TEMP_PASSWORD: Self = Self {
        role: Role::Admin,
        must_change_password: true,
    };
}

impl Default for Access {
    fn default() -> Self {
        Self::USER
    }
}

/// 用户实体，对应 MySQL 表 `users`。
///
/// `password_hash` 留在这里是因为用例层要用它做校验；它从不出现在任何 DTO 里，
/// 由 `api::auth::UserDto` 的字段列表来保证（那里没有这个字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: u64,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub role: Role,
    pub status: AccountStatus,
    /// 临时口令尚未更换。为真时只允许改密码与看自己的信息。
    pub must_change_password: bool,
    pub created_at: NaiveDateTime,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role.is_admin()
    }

    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }
}

/// 从行里取一列，取不到就当作缺列。
///
/// 手写 `FromRow` 而不 derive：角色与状态要在解析时套用「未知值降级」规则，
/// 这属于领域判断，藏在 derive 属性里不如放在这里看得见。
impl sqlx::FromRow<'_, sqlx::mysql::MySqlRow> for User {
    fn from_row(row: &sqlx::mysql::MySqlRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row as _;

        Ok(Self {
            id: row.try_get("id")?,
            email: row.try_get("email")?,
            display_name: row.try_get("display_name")?,
            password_hash: row.try_get("password_hash")?,
            role: Role::parse(row.try_get::<String, _>("role")?.as_str()),
            status: AccountStatus::parse(row.try_get::<String, _>("status")?.as_str()),
            must_change_password: row.try_get("must_change_password")?,
            created_at: row.try_get("created_at")?,
        })
    }
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
    /// 新口令与原口令相同 —— 改密等于没改
    PasswordUnchanged,
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
            AccountError::PasswordUnchanged => {
                write!(f, "new password must differ from the current one")
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

        validate_password(&self.password)?;

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

/// 口令规则。注册、改密、管理员重置都走这一份 ——
/// 三处各写一遍，迟早会出现「注册要求 8 位、改密接受 1 位」的漏洞。
pub fn validate_password(password: &str) -> Result<(), AccountError> {
    if password.trim().is_empty() {
        return Err(AccountError::PasswordBlank);
    }

    // 按字符数而不是字节数算长度，中文口令不该被当成「超长」
    let length = password.chars().count();
    if length < MIN_PASSWORD_LEN {
        return Err(AccountError::PasswordTooShort);
    }
    if length > MAX_PASSWORD_LEN {
        return Err(AccountError::PasswordTooLong);
    }

    Ok(())
}

/// 邮箱合法性。
///
/// 刻意保守：只接受 `local@domain.tld` 这种形态，且要求 ASCII。
/// 用正则去追 RFC 5322 得不偿失 —— 真正能证明邮箱可用的只有「发一封信过去」，
/// 这里的目标是挡住明显的垃圾输入，不是做语法完备的解析器。
pub fn is_valid_email(email: &str) -> bool {
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
    fn unknown_role_never_grants_admin() {
        // 库被写脏时宁可少给权限
        assert_eq!(Role::parse("root"), Role::User);
        assert_eq!(Role::parse(""), Role::User);
        assert_eq!(Role::parse("ADMIN"), Role::Admin);
        assert!(Role::parse(" admin ").is_admin());
        assert!(!Role::parse("administrator").is_admin());
    }

    #[test]
    fn unknown_status_is_treated_as_disabled() {
        // 认不出的状态不能放行
        assert_eq!(AccountStatus::parse("frozen"), AccountStatus::Disabled);
        assert_eq!(AccountStatus::parse(""), AccountStatus::Disabled);
        assert!(AccountStatus::parse("ACTIVE").is_active());
        assert!(!AccountStatus::parse("disabled").is_active());
    }

    #[test]
    fn default_access_is_a_plain_user() {
        let access = Access::default();
        assert_eq!(access.role, Role::User);
        assert!(!access.must_change_password);
        assert!(Access::ADMIN_WITH_TEMP_PASSWORD.role.is_admin());
        // 常量表达式，编译期就该站得住
        const { assert!(Access::ADMIN_WITH_TEMP_PASSWORD.must_change_password) };
    }

    #[test]
    fn wire_names_round_trip_through_parse() {
        // as_str 写进库、parse 读出来：这一对必须互逆，否则重启一次就掉权限
        for role in [Role::User, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), role);
        }
        for status in [AccountStatus::Active, AccountStatus::Disabled] {
            assert_eq!(AccountStatus::parse(status.as_str()), status);
        }
    }

    #[test]
    fn user_predicates_follow_role_and_status() {
        let signup = signup("me@example.com", "long-enough-pass");
        let user = User {
            id: 1,
            email: signup.email.clone(),
            display_name: signup.resolve_display_name(),
            password_hash: "plain$x".into(),
            role: Role::Admin,
            status: AccountStatus::Disabled,
            must_change_password: false,
            created_at: chrono::Utc::now().naive_utc(),
        };

        assert!(user.is_admin());
        // 两件事互不牵连：管理员也可能被停用，停用期间它照样是管理员
        assert!(!user.is_active());
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
