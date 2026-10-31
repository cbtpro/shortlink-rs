//! 应用配置：分层加载、统一校验、启动摘要。
//!
//! 优先级（低到高）：
//! 1. 编译进二进制的 `config/base.env`
//! 2. 磁盘 `config/{APP_ENV}.env`
//! 3. 工作目录的 `.env`（兼容旧习惯）
//! 4. 工作目录的 `.env.local`（个人覆盖，不入库）
//! 5. 真实进程环境变量 —— 部署时只认这一层
//!
//! 这样同一份产物能在任何环境跑：镜像里注入环境变量即可，
//! 本地则靠 profile 文件拿到「前端直读磁盘 + 热刷新」这类开发专用设置。

pub mod env_file;

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Result};

use self::env_file::Layer;

/// 作为最低层的默认值，编进二进制，丢到任何目录都能启动。
const BASE_ENV: &str = include_str!("../../config/base.env");

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
const DEFAULT_CACHE_TTL_SECONDS: u64 = 3600;
const DEFAULT_VISIT_FLUSH_SECONDS: u64 = 300;
const DEFAULT_CODE_LENGTH: usize = 7;
/// 与 `short_links.code VARCHAR(10)` 保持一致
pub const MAX_CODE_LENGTH: usize = 10;
/// 低于 8 位的令牌在暴力枚举下形同虚设
const MIN_ADMIN_TOKEN_LENGTH: usize = 8;
/// 首页「最近创建」保留多少条
const DEFAULT_RECENT_LIMIT: u32 = 20;
/// 登录会话有效期
const DEFAULT_SESSION_TTL_DAYS: u64 = 30;
/// 口令散列迭代次数，取 OWASP 对 PBKDF2-HMAC-SHA256 的建议值
const DEFAULT_PASSWORD_ITERATIONS: u32 = crate::security::password::DEFAULT_ITERATIONS;
const DEFAULT_ASSETS_ROOT: &str = "web";
const DEFAULT_SITE_NAME: &str = "shortlink";

/// 运行环境。只影响默认值，不改变任何业务分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Development,
    Test,
    Production,
}

impl Profile {
    /// 从 `APP_ENV` 读取，未设置时按开发环境处理。
    pub fn from_env() -> Result<Self> {
        Self::parse(&env::var("APP_ENV").unwrap_or_default())
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "dev" | "development" | "local" => Ok(Self::Development),
            "test" | "testing" | "ci" => Ok(Self::Test),
            "prod" | "production" => Ok(Self::Production),
            other => Err(anyhow!(
                "unknown APP_ENV `{other}`, expected development | test | production"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        }
    }

    /// 对应 `config/` 下的覆盖层文件名。
    pub fn env_file_name(self) -> String {
        format!("{}.env", self.as_str())
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 静态资源来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetsMode {
    /// 每次请求读磁盘，改完刷新即可看到（开发）
    Disk,
    /// 编译进二进制，单文件部署（生产）
    Embedded,
}

impl AssetsMode {
    fn parse(raw: &str) -> Result<Option<Self>> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(None),
            "disk" | "fs" | "filesystem" => Ok(Some(Self::Disk)),
            "embedded" | "bundle" => Ok(Some(Self::Embedded)),
            other => Err(anyhow!(
                "unknown ASSETS_MODE `{other}`, expected auto | disk | embedded"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Embedded => "embedded",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub profile: Profile,
    /// 实际生效的配置层（名字 + 路径），启动后由 bootstrap 打到 debug 日志
    pub sources: Vec<String>,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub redis: RedisConfig,
    pub cache: CacheConfig,
    pub link: LinkConfig,
    pub admin: AdminConfig,
    pub auth: AuthConfig,
    pub history: HistoryConfig,
    pub assets: AssetsConfig,
    pub site: SiteConfig,
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
pub struct AuthConfig {
    /// 登录会话有效期。到期后需要重新登录，不做滑动续期：
    /// 续期会让「忘记退出」的会话一直活着，而绝对到期是可预期的。
    pub session_ttl: Duration,
    /// PBKDF2 迭代次数。散列串里自带这个值，
    /// 所以调高它不会让老账号登录失败。
    pub password_iterations: u32,
}

#[derive(Debug, Clone)]
pub struct HistoryConfig {
    /// 每个访客保留的创建记录条数
    pub recent_limit: u32,
    /// 访客 cookie 是否加 `Secure`。由对外地址的协议推断：
    /// 只有 https 站点才加，否则本地 http 调试时浏览器不会回传。
    pub cookie_secure: bool,
}

#[derive(Debug, Clone)]
pub struct AssetsConfig {
    pub mode: AssetsMode,
    /// `disk` 模式下的资源根目录
    pub root: PathBuf,
    /// 是否注入浏览器自动刷新脚本
    pub live_reload: bool,
}

#[derive(Debug, Clone)]
pub struct SiteConfig {
    /// 前端展示用站点名
    pub name: String,
    /// 对外地址里的路径前缀，反代到子路径时由前端拼接口地址
    pub api_base: String,
}

impl Config {
    /// 按约定的层叠顺序加载全部配置，并写回进程环境变量。
    ///
    /// 必须在初始化日志之前调用：`RUST_LOG` 可能来自 profile 文件。
    pub fn load() -> Result<Self> {
        let profile = Profile::from_env()?;
        let layers = collect_layers(profile);

        let resolved = env_file::resolve(&layers);
        apply_to_process(&resolved);

        let mut config = Self::from_lookup(profile, |key| env::var(key).ok())?;
        // 此时日志还没初始化（RUST_LOG 可能就来自这里），先记下来交给 bootstrap 打
        config.sources = layers.iter().map(describe).collect();
        Ok(config)
    }

    /// 从任意键值来源装配配置。测试用它注入固定值，避免依赖全局状态。
    pub fn from_lookup<F>(profile: Profile, lookup: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let source = Lookup { lookup };

        let port: u16 = source.optional("SERVER_PORT", DEFAULT_PORT)?;
        let host: String = source.optional("SERVER_HOST", DEFAULT_HOST.to_string())?;
        let public_base_url: String =
            source.optional("PUBLIC_BASE_URL", format!("http://127.0.0.1:{port}"))?;
        // 统一去掉尾部斜杠，避免拼接短链接时出现 `//`
        let public_base_url = public_base_url.trim_end_matches('/').to_string();

        let database_url = source.required("DATABASE_URL")?;
        let redis_url = source.required("REDIS_URL")?;

        let max_connections: u32 =
            source.optional("DATABASE_MAX_CONNECTIONS", DEFAULT_DB_MAX_CONNECTIONS)?;
        if max_connections == 0 {
            return Err(anyhow!("`DATABASE_MAX_CONNECTIONS` must be at least 1"));
        }

        let code_length: usize = source.optional("SHORT_CODE_LENGTH", DEFAULT_CODE_LENGTH)?;
        if code_length == 0 || code_length > MAX_CODE_LENGTH {
            return Err(anyhow!(
                "`SHORT_CODE_LENGTH` must be between 1 and {MAX_CODE_LENGTH}"
            ));
        }

        let recent_limit: u32 = source.optional("HISTORY_RECENT_LIMIT", DEFAULT_RECENT_LIMIT)?;
        if recent_limit == 0 {
            return Err(anyhow!("`HISTORY_RECENT_LIMIT` must be at least 1"));
        }

        let session_ttl_days: u64 =
            source.optional("SESSION_TTL_DAYS", DEFAULT_SESSION_TTL_DAYS)?;
        if session_ttl_days == 0 {
            return Err(anyhow!("`SESSION_TTL_DAYS` must be at least 1"));
        }

        let password_iterations: u32 =
            source.optional("PASSWORD_HASH_ITERATIONS", DEFAULT_PASSWORD_ITERATIONS)?;
        if password_iterations < crate::security::password::MIN_ITERATIONS {
            return Err(anyhow!(
                "`PASSWORD_HASH_ITERATIONS` must be at least {}",
                crate::security::password::MIN_ITERATIONS
            ));
        }

        let link_ttl: u64 = source.optional("CACHE_LINK_TTL_SECONDS", DEFAULT_CACHE_TTL_SECONDS)?;
        if link_ttl == 0 {
            return Err(anyhow!("`CACHE_LINK_TTL_SECONDS` must be at least 1"));
        }

        let flush_interval: u64 =
            source.optional("VISIT_FLUSH_INTERVAL_SECONDS", DEFAULT_VISIT_FLUSH_SECONDS)?;
        if flush_interval == 0 {
            return Err(anyhow!("`VISIT_FLUSH_INTERVAL_SECONDS` must be at least 1"));
        }

        let mode = AssetsMode::parse(&source.text("ASSETS_MODE").unwrap_or_default())?.unwrap_or(
            match profile {
                Profile::Development => AssetsMode::Disk,
                Profile::Test | Profile::Production => AssetsMode::Embedded,
            },
        );
        // 只有开发环境默认开热刷新：生产环境注入脚本没有意义，还会破坏缓存
        let default_live_reload = profile == Profile::Development && mode == AssetsMode::Disk;
        let live_reload = source.flag("LIVE_RELOAD", default_live_reload)?;

        let assets_root: String =
            source.optional("ASSETS_ROOT", DEFAULT_ASSETS_ROOT.to_string())?;

        Ok(Self {
            profile,
            sources: Vec::new(),
            server: ServerConfig {
                host,
                port,
                public_base_url: public_base_url.clone(),
            },
            database: DatabaseConfig {
                url: database_url,
                max_connections,
            },
            redis: RedisConfig { url: redis_url },
            cache: CacheConfig {
                link_ttl: Duration::from_secs(link_ttl),
                visit_flush_interval: Duration::from_secs(flush_interval),
            },
            link: LinkConfig {
                code_length,
                max_code_length: MAX_CODE_LENGTH,
            },
            admin: AdminConfig {
                token: parse_token("ADMIN_TOKEN", source.text("ADMIN_TOKEN").as_deref())?,
            },
            auth: AuthConfig {
                session_ttl: Duration::from_secs(session_ttl_days * 24 * 60 * 60),
                password_iterations,
            },
            history: HistoryConfig {
                recent_limit,
                cookie_secure: public_base_url.starts_with("https://"),
            },
            assets: AssetsConfig {
                mode,
                root: PathBuf::from(assets_root),
                live_reload: live_reload && mode == AssetsMode::Disk,
            },
            site: SiteConfig {
                name: source.optional("SITE_NAME", DEFAULT_SITE_NAME.to_string())?,
                api_base: path_prefix(&public_base_url),
            },
        })
    }

    /// 启动日志用的一行摘要，连接串已隐去凭据。
    pub fn summary(&self) -> String {
        format!(
            "profile={} addr={}:{} public={} db={} redis={} assets={} live_reload={} \
             admin={} session_ttl={}d password_iterations={}",
            self.profile,
            self.server.host,
            self.server.port,
            self.server.public_base_url,
            redact_credentials(&self.database.url),
            redact_credentials(&self.redis.url),
            self.assets.mode.as_str(),
            if self.assets.live_reload { "on" } else { "off" },
            if self.admin.token.is_some() {
                "enabled"
            } else {
                "disabled"
            },
            self.auth.session_ttl.as_secs() / (24 * 60 * 60),
            self.auth.password_iterations,
        )
    }
}

struct Lookup<F> {
    lookup: F,
}

impl<F> Lookup<F>
where
    F: Fn(&str) -> Option<String>,
{
    /// 去掉首尾空白并视空串为未设置，避免 `KEY=` 这种占位写法被当成有效值。
    fn text(&self, key: &str) -> Option<String> {
        (self.lookup)(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn required(&self, key: &str) -> Result<String> {
        self.text(key)
            .ok_or_else(|| anyhow!("required configuration `{key}` is missing"))
    }

    fn optional<T>(&self, key: &str, default: T) -> Result<T>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        match self.text(key) {
            Some(raw) => raw
                .parse::<T>()
                .map_err(|err| anyhow!("configuration `{key}` is invalid: {err}")),
            None => Ok(default),
        }
    }

    fn flag(&self, key: &str, default: bool) -> Result<bool> {
        match self.text(key).map(|raw| raw.to_ascii_lowercase()) {
            None => Ok(default),
            Some(raw) => match raw.as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                other => Err(anyhow!(
                    "configuration `{key}` expects a boolean, got `{other}`"
                )),
            },
        }
    }
}

/// 按优先级收集配置层。缺文件属正常情况，只有在读失败时才报警。
fn collect_layers(profile: Profile) -> Vec<Layer> {
    let mut layers = vec![Layer::parse_str("builtin config/base.env", BASE_ENV)];

    if let Some(dir) = locate_config_dir() {
        let path = dir.join(profile.env_file_name());
        // profile 文件缺失时说清楚找了哪里，不然「改了不生效」很难查
        match Layer::read(format!("config/{}", profile.env_file_name()), &path) {
            Ok(layer) => layers.push(layer),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("no profile file at {}", path.display());
            }
            Err(err) => log::warn!("failed to read {}: {err}", path.display()),
        }
    } else {
        log::debug!("no config directory found, using built-in defaults only");
    }

    for name in [".env", ".env.local"] {
        let path = Path::new(name);
        if !path.is_file() {
            continue;
        }
        match Layer::read(name, path) {
            Ok(layer) => layers.push(layer),
            Err(err) => log::warn!("failed to read {name}: {err}"),
        }
    }

    layers
}

/// 配置目录查找顺序：显式指定 → 工作目录 → 可执行文件同级。
///
/// 最后一项让「发布包只带 config/ 和可执行文件」也能工作。
fn locate_config_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("SHORTLINK_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }

    let cwd = env::current_dir().ok()?.join("config");
    if cwd.is_dir() {
        return Some(cwd);
    }

    let beside_exe = env::current_exe().ok()?.parent()?.join("config");
    beside_exe.is_dir().then_some(beside_exe)
}

/// 把合并结果写回进程环境变量，但绝不覆盖真实环境变量。
fn apply_to_process(resolved: &std::collections::BTreeMap<String, String>) {
    for (key, value) in resolved {
        if env::var_os(key).is_none() {
            env::set_var(key, value);
        }
    }
}

/// 一行描述一层配置的来源，用于 debug 日志。
fn describe(layer: &Layer) -> String {
    match &layer.path {
        Some(path) => format!("{} <- {}", layer.name, path.display()),
        None => format!("{} <- built-in", layer.name),
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

/// 取出对外地址里的路径部分，作为反代子路径前缀。
///
/// `https://s.test/s` → `/s`；根路径部署则是空串。
/// 前端拿它拼接口地址，省掉「按当前 document 路径去猜」那套容易出错的做法。
fn path_prefix(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);

    match rest.find('/') {
        Some(index) => rest[index..].trim_end_matches('/').to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(pairs: &[(&str, &str)]) -> Result<Config> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        Config::from_lookup(Profile::Development, move |key| {
            owned
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        })
    }

    fn minimal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("DATABASE_URL", "mysql://u:p@127.0.0.1:3306/db"),
            ("REDIS_URL", "redis://127.0.0.1:6379/0"),
        ]
    }

    #[test]
    fn parses_known_profiles() {
        assert_eq!(Profile::parse("").unwrap(), Profile::Development);
        assert_eq!(Profile::parse("dev").unwrap(), Profile::Development);
        assert_eq!(
            Profile::parse("  PRODUCTION ").unwrap(),
            Profile::Production
        );
        assert_eq!(Profile::parse("ci").unwrap(), Profile::Test);
        assert!(Profile::parse("staging").is_err());
    }

    #[test]
    fn profile_selects_its_own_file() {
        assert_eq!(Profile::Development.env_file_name(), "development.env");
        assert_eq!(Profile::Production.env_file_name(), "production.env");
    }

    #[test]
    fn requires_connection_strings() {
        let err = config_with(&[]).unwrap_err().to_string();
        assert!(err.contains("DATABASE_URL"), "报错要指名缺的是哪一项");
    }

    #[test]
    fn blank_value_counts_as_missing() {
        let mut pairs = minimal();
        pairs[0] = ("DATABASE_URL", "   ");
        assert!(config_with(&pairs).is_err());
    }

    #[test]
    fn defaults_are_usable() {
        let config = config_with(&minimal()).unwrap();

        assert_eq!(config.server.port, DEFAULT_PORT);
        assert_eq!(config.server.public_base_url, "http://127.0.0.1:8080");
        assert_eq!(config.link.code_length, DEFAULT_CODE_LENGTH);
        assert_eq!(config.history.recent_limit, DEFAULT_RECENT_LIMIT);
        assert_eq!(
            config.auth.session_ttl,
            Duration::from_secs(DEFAULT_SESSION_TTL_DAYS * 24 * 60 * 60)
        );
        assert_eq!(config.auth.password_iterations, DEFAULT_PASSWORD_ITERATIONS);
        assert!(config.admin.token.is_none());
        assert_eq!(config.site.name, "shortlink");
    }

    #[test]
    fn development_defaults_to_disk_assets_and_live_reload() {
        let config = config_with(&minimal()).unwrap();

        assert_eq!(config.assets.mode, AssetsMode::Disk);
        assert!(config.assets.live_reload);
    }

    #[test]
    fn production_defaults_to_embedded_without_live_reload() {
        let owned: Vec<(String, String)> = minimal()
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let config = Config::from_lookup(Profile::Production, move |key| {
            owned
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        })
        .unwrap();

        assert_eq!(config.assets.mode, AssetsMode::Embedded);
        assert!(!config.assets.live_reload);
    }

    #[test]
    fn live_reload_is_forced_off_when_assets_are_embedded() {
        // 资源都在二进制里，注入刷新脚本只会看到旧内容
        let mut pairs = minimal();
        pairs.push(("ASSETS_MODE", "embedded"));
        pairs.push(("LIVE_RELOAD", "true"));

        assert!(!config_with(&pairs).unwrap().assets.live_reload);
    }

    #[test]
    fn public_base_url_has_no_trailing_slash() {
        let mut pairs = minimal();
        pairs.push(("PUBLIC_BASE_URL", "https://s.test/"));

        let config = config_with(&pairs).unwrap();

        assert_eq!(config.server.public_base_url, "https://s.test");
        assert!(
            config.history.cookie_secure,
            "https 站点应给 cookie 加 Secure"
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        for (key, value) in [
            ("SHORT_CODE_LENGTH", "0"),
            ("SHORT_CODE_LENGTH", "11"),
            ("DATABASE_MAX_CONNECTIONS", "0"),
            ("HISTORY_RECENT_LIMIT", "0"),
            ("SESSION_TTL_DAYS", "0"),
            ("PASSWORD_HASH_ITERATIONS", "10"),
            ("CACHE_LINK_TTL_SECONDS", "0"),
            ("SERVER_PORT", "not-a-port"),
        ] {
            let mut pairs = minimal();
            pairs.push((key, value));
            assert!(config_with(&pairs).is_err(), "{key}={value} 应被拒绝");
        }
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

    #[test]
    fn summary_never_leaks_passwords() {
        let summary = config_with(&minimal()).unwrap().summary();

        assert!(!summary.contains(":p@"), "摘要里不能出现密码：{summary}");
        assert!(summary.contains("profile=development"));
    }

    #[test]
    fn extracts_path_prefix_for_sub_path_deployments() {
        assert_eq!(path_prefix("https://s.test"), "");
        assert_eq!(path_prefix("https://s.test/"), "");
        assert_eq!(path_prefix("https://s.test/s"), "/s");
        assert_eq!(path_prefix("https://s.test/s/"), "/s");
        assert_eq!(path_prefix("http://127.0.0.1:9981"), "");
    }

    #[test]
    fn builtin_layer_covers_every_documented_default() {
        // base.env 是「零配置可启动」的保证，被删掉会让发布包起不来
        let layer = Layer::parse_str("builtin", BASE_ENV);
        let keys: Vec<&str> = layer
            .entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect();

        for required in ["SERVER_PORT", "DATABASE_URL", "REDIS_URL", "ASSETS_MODE"] {
            assert!(keys.contains(&required), "base.env 缺少 {required}");
        }
    }
}
