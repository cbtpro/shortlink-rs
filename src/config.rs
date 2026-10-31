//! 应用配置：所有可变参数在此读取。除数据库与 Redis 地址外均有默认值。

use std::env;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_CACHE_TTL_SECONDS: u64 = 3600;
const DEFAULT_VISIT_FLUSH_SECONDS: u64 = 300;
const DEFAULT_CODE_LENGTH: usize = 7;
/// 与 `short_links.code VARCHAR(10)` 保持一致
const MAX_CODE_LENGTH: usize = 10;
/// 低于 8 位的令牌在暴力枚举下形同虚设
const MIN_ADMIN_TOKEN_LENGTH: usize = 8;
/// 首页「最近创建」保留多少条
const DEFAULT_RECENT_LIMIT: u32 = 20;

#[derive(Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub cache: CacheConfig,
    pub link: LinkConfig,
    pub admin: AdminConfig,
    pub history: HistoryConfig,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// 对外可访问的站点根地址，用于拼接返回给用户的短链接
    pub public_base_url: String,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone)]
pub struct RedisConfig {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// 短链内容缓存存活时间
    pub link_ttl: Duration,
    /// 访问计数回刷间隔
    pub visit_flush_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// 自动生成短码的长度
    pub code_length: usize,
    /// 自定义短码允许的最大长度
    pub max_code_length: usize,
}

#[derive(Debug, Clone)]
pub struct AdminConfig {
    /// 后台访问令牌。`None` 表示未启用后台，相关路由一律 404。
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HistoryConfig {
    /// 每个访客保留的创建记录条数
    pub recent_limit: u32,
    /// 访客 cookie 是否加 `Secure`。由对外地址的协议推断：
    /// 只有 https 站点才加，否则本地 http 调试时浏览器不会回传。
    pub cookie_secure: bool,
}

impl Config {
    /// 从环境变量装配配置（调用前应先加载 `.env`）。
    pub fn from_env() -> Result<Self> {
        let port: u16 = optional("SERVER_PORT", DEFAULT_PORT)?;
        let host: String = optional("SERVER_HOST", DEFAULT_HOST.to_string())?;
        let public_base_url: String =
            optional("PUBLIC_BASE_URL", format!("http://127.0.0.1:{port}"))?;
        // 统一去掉尾部斜杠，避免拼接短链接时出现 `//`
        let public_base_url = public_base_url.trim_end_matches('/').to_string();

        Ok(Self {
            server: ServerConfig {
                host,
                port,
                public_base_url: public_base_url.clone(),
            },
            database: DatabaseConfig {
                url: required("DATABASE_URL")?,
                max_connections: optional("DATABASE_MAX_CONNECTIONS", DEFAULT_DB_MAX_CONNECTIONS)?,
            },
            redis: RedisConfig {
                url: required("REDIS_URL")?,
            },
            cache: CacheConfig {
                link_ttl: Duration::from_secs(optional(
                    "CACHE_LINK_TTL_SECONDS",
                    DEFAULT_CACHE_TTL_SECONDS,
                )?),
                visit_flush_interval: Duration::from_secs(optional(
                    "VISIT_FLUSH_INTERVAL_SECONDS",
                    DEFAULT_VISIT_FLUSH_SECONDS,
                )?),
            },
            link: LinkConfig {
                code_length: optional("SHORT_CODE_LENGTH", DEFAULT_CODE_LENGTH)?,
                max_code_length: MAX_CODE_LENGTH,
            },
            admin: AdminConfig {
                token: parse_token("ADMIN_TOKEN", env::var("ADMIN_TOKEN").ok().as_deref())?,
            },
            history: HistoryConfig {
                recent_limit: optional("HISTORY_RECENT_LIMIT", DEFAULT_RECENT_LIMIT)?,
                cookie_secure: public_base_url.starts_with("https://"),
            },
        })
    }
}

/// 解析后台令牌。
///
/// 空串等同未配置（方便用 `.env` 占位），非空但过短直接报错 ——
/// 与其带着一个可被枚举的口令上线，不如启动时失败。
fn parse_token(key: &str, raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else { return Ok(None) };

    let token = raw.trim();
    if token.is_empty() {
        return Ok(None);
    }
    if token.chars().count() < MIN_ADMIN_TOKEN_LENGTH {
        return Err(anyhow!(
            "`{key}` must be at least {MIN_ADMIN_TOKEN_LENGTH} characters"
        ));
    }
    Ok(Some(token.to_string()))
}

fn required(key: &str) -> Result<String> {
    env::var(key).map_err(|_| anyhow!("required environment variable `{key}` is not set"))
}

/// 隐去连接串里的凭据，只保留协议、主机与库名，用于日志与报错展示。
///
/// `mysql://user:pass@host:3306/db` → `mysql://***@host:3306/db`
pub fn redact_credentials(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };

    let rest = &url[scheme_end + 3..];
    match rest.rfind('@') {
        Some(at) => format!("{}***@{}", &url[..scheme_end + 3], &rest[at + 1..]),
        // 没有 userinfo 段，说明本来就没带账号密码
        None => url.to_string(),
    }
}

fn optional<T>(key: &str, default: T) -> Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(raw) => raw
            .trim()
            .parse::<T>()
            .map_err(|err| anyhow!("environment variable `{key}` is invalid: {err}")),
        Err(_) => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_base_url_has_no_trailing_slash() {
        let trimmed = "http://example.com/".trim_end_matches('/');
        assert_eq!(trimmed, "http://example.com");
    }

    #[test]
    fn code_length_must_fit_column() {
        assert!(DEFAULT_CODE_LENGTH <= MAX_CODE_LENGTH);
    }

    #[test]
    fn redact_hides_userinfo() {
        assert_eq!(
            redact_credentials("mysql://u:p@127.0.0.1:3306/db"),
            "mysql://***@127.0.0.1:3306/db"
        );
    }

    #[test]
    fn redact_keeps_url_without_credentials() {
        assert_eq!(
            redact_credentials("redis://127.0.0.1:6379/0"),
            "redis://127.0.0.1:6379/0"
        );
        assert_eq!(redact_credentials("not-a-url"), "not-a-url");
    }

    #[test]
    fn blank_admin_token_disables_admin() {
        assert_eq!(parse_token("ADMIN_TOKEN", None).unwrap(), None);
        assert_eq!(parse_token("ADMIN_TOKEN", Some("   ")).unwrap(), None);
    }

    #[test]
    fn admin_token_is_trimmed() {
        assert_eq!(
            parse_token("ADMIN_TOKEN", Some("  s3cret-token  ")).unwrap(),
            Some("s3cret-token".to_string())
        );
    }

    #[test]
    fn short_admin_token_is_rejected() {
        // 与其带着可枚举的口令上线，不如启动时失败
        assert!(parse_token("ADMIN_TOKEN", Some("short")).is_err());
    }
}
