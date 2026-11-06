//! 组合根：读配置、装配依赖、启动 HTTP 服务与后台任务。
//!
//! 这里是唯一知道「具体用哪个实现」的地方：换个存储就是换这里的两行。

use std::path::Path;
use std::sync::Arc;

use actix_web::middleware::Logger;
use actix_web::{web, App, HttpServer};
use anyhow::{anyhow, Context, Result};
use log::{info, warn};
use sqlx::mysql::MySqlPoolOptions;

use crate::api;
use crate::assets::AssetServer;
use crate::cache::redis_store::{self, RedisGateTicket, RedisLinkCache, RedisVisitCounter};
use crate::cache::{GateTicket, LinkCache, VisitCounter};
use crate::domain::account::{Access, NewUser};
use crate::domain::code::RandomCodeGenerator;
use crate::health::Readiness;
use crate::repository::mysql::{MySqlAccountRepository, MySqlLinkRepository};
use crate::repository::{
    CreationHistory, LinkRepository, RepoError, SessionRepository, UserRepository,
};
use crate::security::password::random_password;
use crate::security::{PasswordHasher, Pbkdf2Hasher};
use crate::service::auth::AuthService;
use crate::service::console::ConsoleService;
use crate::service::history::HistoryService;
use crate::service::my_links::MyLinksService;
use crate::service::redirect::RedirectService;
use crate::service::shorten::ShortenService;
use crate::settings::Config;
use crate::state::AppState;
use crate::tasks::flush_visits::VisitFlusher;
use crate::tasks::purge_sessions::SessionPurger;

/// 访问日志：方法与路径用引号括起来，方便直接粘贴到 `grep`。
const REQUEST_LOG: &str = r#"%a "%r" %s %b %Dms"#;

/// 初始管理员口令长度。要人肉抄进登录框，太长只会被贴在便签上。
const INITIAL_PASSWORD_LEN: usize = 20;

pub async fn run() -> Result<()> {
    let config = Config::load().context("failed to load configuration")?;
    init_logging();

    // 配置加载发生在日志初始化之前（RUST_LOG 本身来自配置），所以层来源放到这里打
    for source in &config.sources {
        log::debug!("config source: {source}");
    }
    info!("{}", config.summary());

    // 占位域名会静默生成打不开的短链，与其上线后被用户发现，不如启动时就喊一声
    if config.profile == crate::settings::Profile::Production
        && config.server.public_base_url.contains("example.com")
    {
        warn!(
            "PUBLIC_BASE_URL is still the placeholder `{}`; short links handed to users will not work. \
             Set it to the real public origin.",
            config.server.public_base_url
        );
    }

    let assets = Arc::new(
        AssetServer::new(
            config.assets.mode,
            config.assets.root.clone(),
            config.assets.live_reload,
        )
        .context("failed to initialise assets")?,
    );
    if assets.live_reload() {
        info!(
            "assets served from {} at request time, browser auto-reload is on",
            config.assets.root.display()
        );
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| {
            format!(
                "failed to connect to MySQL at {}",
                crate::settings::redact_credentials(&config.database.url)
            )
        })?;

    let redis_conn = redis_store::connect(&config.redis.url).await?;

    info!(
        "dependencies ready: mysql={} redis={}",
        crate::settings::redact_credentials(&config.database.url),
        crate::settings::redact_credentials(&config.redis.url)
    );

    // 依赖以 trait object 注入：换实现只改这几行
    let readiness = Arc::new(Readiness::new(pool.clone(), redis_conn.clone()));
    let store = Arc::new(MySqlLinkRepository::new(pool.clone()));
    let repository: Arc<dyn LinkRepository> = store.clone();
    // 同一个实现同时充当两个端口，避免再开一条连接池
    let creations: Arc<dyn CreationHistory> = store;
    let accounts = Arc::new(MySqlAccountRepository::new(pool));
    let users: Arc<dyn UserRepository> = accounts.clone();
    let sessions: Arc<dyn SessionRepository> = accounts;
    let link_cache: Arc<dyn LinkCache> = Arc::new(RedisLinkCache::new(redis_conn.clone()));
    let visit_counter: Arc<dyn VisitCounter> = Arc::new(RedisVisitCounter::new(redis_conn.clone()));
    let gate_ticket: Arc<dyn GateTicket> = Arc::new(RedisGateTicket::new(redis_conn));
    let hasher: Arc<dyn PasswordHasher> =
        Arc::new(Pbkdf2Hasher::new(config.auth.password_iterations));

    let shorten = Arc::new(ShortenService::new(
        repository.clone(),
        Arc::new(RandomCodeGenerator),
        config.link.code_length,
        config.link.max_code_length,
        config.server.public_base_url.clone(),
    ));
    let redirect = Arc::new(RedirectService::new(
        repository.clone(),
        link_cache.clone(),
        visit_counter.clone(),
        gate_ticket,
        config.cache.link_ttl,
    ));
    let console = Arc::new(ConsoleService::new(
        repository.clone(),
        users.clone(),
        sessions.clone(),
        link_cache.clone(),
        visit_counter.clone(),
        hasher.clone(),
    ));
    let history = Arc::new(HistoryService::new(creations, config.history.recent_limit));
    let my_links = Arc::new(MyLinksService::new(
        repository.clone(),
        link_cache,
        config.history.recent_limit,
    ));
    let auth = Arc::new(AuthService::new(
        users.clone(),
        sessions,
        hasher.clone(),
        config.auth.session_ttl,
    ));

    // 库里没有管理员时生成一个，口令写进文件并要求首次登录改密
    ensure_admin(&config, users.as_ref(), hasher).await?;

    VisitFlusher::new(repository, visit_counter).spawn(config.cache.visit_flush_interval);
    SessionPurger::new(auth.clone()).spawn();

    let state = web::Data::new(AppState {
        shorten,
        redirect,
        console,
        auth,
        my_links,
        history,
        assets,
        readiness: Some(readiness),
        profile: config.profile,
        site_name: config.site.name.clone(),
        api_base: config.site.api_base.clone(),
        cookie_secure: config.history.cookie_secure,
        admin_password_file: Some(config.admin.password_file.clone()),
    });

    let host = config.server.host.clone();
    let port = config.server.port;

    let server = HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(Logger::new(REQUEST_LOG))
            .configure(api::configure)
    })
    .bind((host.as_str(), port))
    .map_err(|err| bind_error(err, &host, port))?
    // 收到退出信号后给在途请求留出时间，避免回刷任务被硬切
    .shutdown_timeout(10);

    info!("listening on http://{host}:{port}");

    server.run().await?;

    Ok(())
}

/// 确保库里存在管理员账号。
///
/// 只在「一个管理员都没有」时动手：生成随机口令、写文件、建号，
/// 并要求首次登录必须改密。已有管理员时什么也不做 —— 重启一次就换一次口令，
/// 只会让人怀疑自己记错了。
async fn ensure_admin(
    config: &Config,
    users: &dyn UserRepository,
    hasher: Arc<dyn PasswordHasher>,
) -> Result<()> {
    let path = &config.admin.password_file;

    if let Some(admin) = users
        .find_first_admin()
        .await
        .context("failed to look up the administrator account")?
    {
        info!("administrator account ready: {}", admin.email);
        if !admin.must_change_password {
            // 口令已经改过，文件里那串就是作废的凭据，留着只会误导运维
            remove_quietly(path, "initial password file");
        }
        return Ok(());
    }

    let password = random_password(INITIAL_PASSWORD_LEN);
    let hash = {
        // PBKDF2 几百毫秒起步，启动阶段也不该占住 runtime 线程
        let hasher = hasher.clone();
        let password = password.clone();
        tokio::task::spawn_blocking(move || hasher.hash(&password))
            .await
            .context("password hashing task failed")?
    };

    // 先落盘再建号：反过来一旦写文件失败，账号建好了却没人知道口令，
    // 下次启动又因为「已有管理员」而不再生成 —— 直接把自己锁在门外。
    write_credential_file(path, &config.admin.email, &password)?;

    let new = NewUser {
        email: config.admin.email.clone(),
        password: password.clone(),
        display_name: Some("管理员".to_string()),
    };

    match users
        .create(&new, &hash, Access::ADMIN_WITH_TEMP_PASSWORD)
        .await
    {
        Ok(admin) => {
            warn!(
                "no administrator found; created `{}` with a one-time password",
                admin.email
            );
            warn!(
                "read it from {} and change it after the first login",
                path.display()
            );
            Ok(())
        }
        Err(err) => {
            remove_quietly(path, "initial password file");
            if matches!(err, RepoError::DuplicateEmail) {
                return Err(anyhow!(
                    "cannot create the administrator account `{}`: the address is already used by a \
                     regular account. Set ADMIN_EMAIL to another address and restart.",
                    config.admin.email
                ));
            }
            Err(anyhow::Error::new(err).context("failed to create the administrator account"))
        }
    }
}

/// 把初始凭据写到文件，并尽量收紧权限。
///
/// 纯文本是刻意的：它要被人打开、抄进登录框，加密只会多一道没人会做的解密步骤。
/// 真正的保护是「首次登录必须改密 + 改完即删」。
fn write_credential_file(path: &Path, email: &str, password: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create directory {}", dir.display()))?;
        }
    }

    let created_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let content = format!(
        "# shortlink 管理员初始凭据\n\
         # 首次登录后必须修改密码；修改完成后本文件会被自动删除。\n\
         email={email}\n\
         password={password}\n\
         created_at={created_at}\n"
    );

    std::fs::write(path, content)
        .with_context(|| format!("failed to write initial password to {}", path.display()))?;
    restrict_file_permissions(path)?;

    Ok(())
}

/// 只有属主可读写。Windows 没有 POSIX 权限位，靠目录 ACL 兜底。
#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// 删文件，但「本来就没有」不算错。
fn remove_quietly(path: &Path, label: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => info!("removed stale {label} {}", path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => warn!("failed to remove {label} {}: {err}", path.display()),
    }
}

fn init_logging() {
    // RUST_LOG 可能来自 profile 文件，必须等配置加载完再初始化
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
}

/// 端口被占用是最常见的启动失败，默认报错太抽象，这里直接给出下一步。
fn bind_error(err: std::io::Error, host: &str, port: u16) -> anyhow::Error {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        return anyhow!(
            "{host}:{port} is already in use. Another instance is probably still running: \
             find it with `netstat -ano | findstr :{port}` and stop it, or start on another port \
             with SERVER_PORT=<port>"
        );
    }

    anyhow!(err).context(format!("failed to bind {host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("shortlink-{tag}-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn credential_file_carries_everything_an_operator_needs() {
        let dir = unique_dir("credential");
        let path = dir.join("nested/admin.txt");

        write_credential_file(&path, "admin@example.com", "s3cret-pass").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("email=admin@example.com"));
        assert!(content.contains("password=s3cret-pass"));
        assert!(content.contains("必须修改密码"), "要提醒改密：{content}");
        assert!(path.exists());

        // 目录是自动建出来的
        assert!(dir.join("nested").is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn credential_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_dir("permissions");
        let path = dir.join("admin.txt");
        write_credential_file(&path, "admin@example.com", "s3cret-pass").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "初始口令不该让同机其他账号读到");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_password_file_is_not_an_error() {
        // 启动时的清理逻辑据此忽略「已经删掉了」
        remove_quietly(
            &unique_dir("missing").join("nope.txt"),
            "initial password file",
        );
    }

    /* ---------- 初始化管理员 ----------
     * 这一段出错只有一种后果：谁也进不了后台。所以每个分支都要钉住。
     */

    use crate::domain::account::{Access, NewUser, Role};
    use crate::repository::memory::InMemoryLinkRepository;
    use crate::repository::UserRepository;
    use crate::security::password::PasswordHasher;
    use crate::settings::Profile;
    use crate::test_support::PlainHasher;

    fn config_at(dir: &Path) -> Config {
        let mut config = Config::from_lookup(Profile::Development, |key| match key {
            "DATABASE_URL" => Some("mysql://user:pass@127.0.0.1:3306/short_link".to_string()),
            "REDIS_URL" => Some("redis://127.0.0.1:6379".to_string()),
            _ => None,
        })
        .expect("最小配置应当可用");
        config.admin.email = "admin@example.com".to_string();
        config.admin.password_file = dir.join("admin-initial-password.txt");
        config
    }

    async fn add_account(
        store: &InMemoryLinkRepository,
        email: &str,
        access: Access,
    ) -> crate::domain::account::User {
        <InMemoryLinkRepository as UserRepository>::create(
            store,
            &NewUser {
                email: email.to_string(),
                password: "secret123".to_string(),
                display_name: None,
            },
            "plain$secret123",
            access,
        )
        .await
        .expect("账号应当建得出来")
    }

    #[tokio::test]
    async fn first_boot_creates_a_locked_admin_and_hands_the_password_over_in_a_file() {
        let dir = unique_dir("ensure-admin");
        let config = config_at(&dir);
        let store = InMemoryLinkRepository::new();

        ensure_admin(&config, &store, Arc::new(PlainHasher))
            .await
            .expect("空库应当能建出管理员");

        let admin = store
            .find_first_admin()
            .await
            .unwrap()
            .expect("应当建出管理员");
        assert_eq!(admin.email, "admin@example.com");
        assert!(admin.is_active());
        // 一次性口令的标志位：不置它，首次登录就不会被要求改密
        assert!(admin.must_change_password, "初始管理员必须先改密");

        let content =
            std::fs::read_to_string(&config.admin.password_file).expect("凭据文件应当写出来");
        assert!(content.contains("email=admin@example.com"), "{content}");
        // 文件里的口令就是库里的那一份：抄进登录框必须能进
        assert!(
            content.contains(&format!(
                "password={}",
                admin.password_hash.trim_start_matches("plain$")
            )),
            "文件与库里的口令不一致：{content}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_admin_who_still_owes_a_password_change_keeps_the_file() {
        let dir = unique_dir("ensure-admin-pending");
        let config = config_at(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&config.admin.password_file, "email=admin@example.com\n").unwrap();

        let store = InMemoryLinkRepository::new();
        add_account(&store, "boss@example.com", Access::ADMIN_WITH_TEMP_PASSWORD).await;

        ensure_admin(&config, &store, Arc::new(PlainHasher))
            .await
            .unwrap();

        // 还没改密，文件仍是有效凭据
        assert!(config.admin.password_file.exists(), "没改密就不该删文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_established_admin_is_left_alone_and_the_stale_file_is_removed() {
        let dir = unique_dir("ensure-admin-existing");
        let config = config_at(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&config.admin.password_file, "email=admin@example.com\n").unwrap();

        let store = InMemoryLinkRepository::new();
        let existing = add_account(
            &store,
            "boss@example.com",
            Access {
                role: Role::Admin,
                must_change_password: false,
            },
        )
        .await;

        ensure_admin(&config, &store, Arc::new(PlainHasher))
            .await
            .unwrap();

        // 重启不换口令：否则每重启一次大家就怀疑自己记错了
        assert!(
            store
                .find_by_email("admin@example.com")
                .await
                .unwrap()
                .is_none(),
            "已有管理员时不该再建一个"
        );
        let admin = store.find_first_admin().await.unwrap().unwrap();
        assert_eq!(admin.id, existing.id);
        assert_eq!(admin.password_hash, "plain$secret123");
        // 改过密码后文件里那串就是作废凭据，留着只会误导运维
        assert!(
            !config.admin.password_file.exists(),
            "陈旧凭据文件应当被清掉"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_admin_email_taken_by_a_regular_user_fails_with_a_way_out() {
        let dir = unique_dir("ensure-admin-conflict");
        let config = config_at(&dir);
        let store = InMemoryLinkRepository::new();
        add_account(&store, "admin@example.com", Access::USER).await;

        let err = ensure_admin(&config, &store, Arc::new(PlainHasher))
            .await
            .expect_err("邮箱被普通用户占用时应当报错");
        let text = format!("{err:#}");
        assert!(
            text.contains("ADMIN_EMAIL"),
            "报错要直接告诉运维改哪个变量：{text}"
        );

        // 建号失败时不能留下一个「看似可用」的口令文件
        assert!(!config.admin.password_file.exists());
        assert!(store.find_first_admin().await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn no_account_is_created_when_the_credential_file_cannot_be_written() {
        let dir = unique_dir("ensure-admin-unwritable");
        std::fs::create_dir_all(&dir).unwrap();
        let config = config_at(&dir);
        // 把「文件」的位置占成目录，写入必然失败
        std::fs::create_dir_all(&config.admin.password_file).unwrap();

        let store = InMemoryLinkRepository::new();
        assert!(
            ensure_admin(&config, &store, Arc::new(PlainHasher))
                .await
                .is_err(),
            "写不了凭据文件就应当失败"
        );

        // 关键：账号没建出来，下次启动会重试。
        // 反过来（先建号后写文件）就会「有管理员但没人知道口令」，永久锁死。
        assert!(
            store.find_first_admin().await.unwrap().is_none(),
            "写文件失败时绝不能已经把管理员建出来"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真实散列下也要能走通一遍，确认 spawn_blocking 那段没有把口令丢掉。
    #[tokio::test]
    async fn the_stored_hash_matches_the_password_in_the_file() {
        let dir = unique_dir("ensure-admin-hash");
        let config = config_at(&dir);
        let store = InMemoryLinkRepository::new();
        let hasher: Arc<dyn PasswordHasher> = Arc::new(crate::security::Pbkdf2Hasher::new(1_000));

        ensure_admin(&config, &store, hasher.clone()).await.unwrap();

        let content = std::fs::read_to_string(&config.admin.password_file).unwrap();
        let password = content
            .lines()
            .find_map(|line| line.strip_prefix("password="))
            .expect("文件里应当有口令");
        let admin = store.find_first_admin().await.unwrap().unwrap();
        assert!(
            hasher.verify(password, &admin.password_hash),
            "文件里的口令必须能验证通过库里的散列"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
