//! 测试替身与构造辅助。
//!
//! 只在 `cargo test` 下编译。集中放一处，是为了让用例层、后台任务、HTTP 层
//! 共用同一批假实现 —— 否则「仿真」会长出好几份，各自语义还会慢慢漂移。
//!
//! 命名约定：`NoOp*` 表示什么都不做且从不失败，`Broken*` 表示必定失败（用于验证
//! 降级与报错路径），`Recording*` 表示会记录调用过程（用于断言副作用）。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use crate::cache::{CacheError, GateTicket, LinkCache, PendingVisit, VisitCounter};
use crate::domain::code::ShortCodeGenerator;
use crate::domain::short_link::CachedLink;
use crate::repository::{
    LinkOverview, LinkPage, LinkQuery, LinkRepository, OverviewWindow, RepoError,
    SessionRepository, UserPage, UserQuery, UserRepository,
};
use crate::types::BoxFuture;

pub use crate::repository::memory::InMemoryLinkRepository;

/// 不做散列的替身：把明文当成「散列」。
///
/// 用例层测的是流程（谁什么时候被拒绝），不是 PBKDF2 本身；用真散列会让每个
/// 用例多跑几百毫秒。散列算法自己的正确性在 `security::password` 里单独测。
pub struct PlainHasher;

impl crate::security::password::PasswordHasher for PlainHasher {
    fn hash(&self, password: &str) -> String {
        format!("plain${password}")
    }

    fn verify(&self, password: &str, encoded: &str) -> bool {
        encoded == format!("plain${password}")
    }
}

/// 构造一个后端失败错误。
///
/// 用序列化错误代替 `RedisError`，避免测试依赖 redis 内部构造 API。
fn backend_failure() -> CacheError {
    CacheError::Serialize(
        serde_json::from_str::<i32>("not-json").expect_err("literal is not valid json"),
    )
}

/// 永远未命中、写入即丢弃的缓存。
pub struct NoOpCache;

impl LinkCache for NoOpCache {
    fn get<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<Option<CachedLink>, CacheError>> {
        Box::pin(async { Ok(None) })
    }

    fn put<'a>(
        &'a self,
        _code: &'a str,
        _link: &'a CachedLink,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Ok(()) })
    }

    fn evict<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Ok(()) })
    }
}

/// 读写都失败的缓存。
pub struct BrokenCache;

impl LinkCache for BrokenCache {
    fn get<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<Option<CachedLink>, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn put<'a>(
        &'a self,
        _code: &'a str,
        _link: &'a CachedLink,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn evict<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }
}

/// 在内存里记录写入的缓存，可预置条目以模拟缓存命中。
#[derive(Default)]
pub struct RecordingCache {
    entries: Mutex<HashMap<String, CachedLink>>,
}

impl RecordingCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置一条缓存，模拟「上次访问已回填」。
    pub fn preload(&self, code: &str, link: CachedLink) {
        self.lock().insert(code.to_string(), link);
    }

    pub fn contains(&self, code: &str) -> bool {
        self.lock().contains_key(code)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, CachedLink>> {
        self.entries.lock().expect("recording cache mutex poisoned")
    }
}

impl LinkCache for RecordingCache {
    fn get<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<Option<CachedLink>, CacheError>> {
        let hit = self.lock().get(code).cloned();
        Box::pin(async move { Ok(hit) })
    }

    fn put<'a>(
        &'a self,
        code: &'a str,
        link: &'a CachedLink,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), CacheError>> {
        self.preload(code, link.clone());
        Box::pin(async { Ok(()) })
    }

    fn evict<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<(), CacheError>> {
        self.lock().remove(code);
        Box::pin(async { Ok(()) })
    }
}

/// 不做任何事、从不失败的访问计数器。
pub struct NoOpCounter;

impl VisitCounter for NoOpCounter {
    fn pending_of<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async { Ok(0) })
    }

    fn increment<'a>(
        &'a self,
        _code: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async { Ok(1) })
    }

    fn scan_pending<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PendingVisit>, CacheError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn consume<'a>(&'a self, _visit: PendingVisit) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Ok(()) })
    }
}

/// 所有操作都失败的访问计数器。
pub struct BrokenCounter;

impl VisitCounter for BrokenCounter {
    fn pending_of<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn increment<'a>(
        &'a self,
        _code: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<u32, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn scan_pending<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PendingVisit>, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn consume<'a>(&'a self, _visit: PendingVisit) -> BoxFuture<'a, Result<(), CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }
}

/// 内存版票据仓库：签发即登记，兑换一次即失效。
#[derive(Default)]
pub struct InMemoryGateTicket {
    /// 票据 -> 绑定的短码
    issued: Mutex<HashMap<String, String>>,
}

impl InMemoryGateTicket {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前还没被兑换的票据数量。
    pub fn outstanding(&self) -> usize {
        self.issued.lock().expect("ticket mutex poisoned").len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.issued.lock().expect("ticket mutex poisoned")
    }
}

impl GateTicket for InMemoryGateTicket {
    fn issue<'a>(
        &'a self,
        code: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<String, CacheError>> {
        // 令牌本身不需要仿真：只要唯一、可放进 URL 就够了
        let ticket = format!("ticket-{}", self.outstanding() + 1);
        self.lock().insert(ticket.clone(), code.to_string());
        Box::pin(async move { Ok(ticket) })
    }

    fn redeem<'a>(&'a self, ticket: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        let code = self.lock().remove(ticket);
        Box::pin(async move { Ok(code) })
    }
}

/// 所有操作都失败的票据仓库：用来验证「Redis 挂了，中转页还能走」。
pub struct BrokenGateTicket;

impl GateTicket for BrokenGateTicket {
    fn issue<'a>(
        &'a self,
        _code: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<String, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }

    fn redeem<'a>(&'a self, _ticket: &'a str) -> BoxFuture<'a, Result<Option<String>, CacheError>> {
        Box::pin(async { Err(backend_failure()) })
    }
}

/// 在内存里维护待回刷增量的计数器，支持断言自增次数与剩余增量。
#[derive(Default)]
pub struct RecordingCounter {
    pending: Mutex<HashMap<String, u32>>,
    increments: Mutex<HashMap<String, u32>>,
}

impl RecordingCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预置待回刷增量，模拟「上一轮回刷失败留下的积压」。
    pub fn with_pending(code: &str, count: u32) -> Self {
        let counter = Self::new();
        counter.seed(code, count);
        counter
    }

    /// 追加一条待回刷增量，可链式调用。
    pub fn seed(&self, code: &str, count: u32) -> &Self {
        self.pending
            .lock()
            .expect("counter mutex poisoned")
            .insert(code.to_string(), count);
        self
    }

    /// 当前尚未回刷的增量。
    pub fn pending_of_sync(&self, code: &str) -> u32 {
        self.pending
            .lock()
            .expect("counter mutex poisoned")
            .get(code)
            .copied()
            .unwrap_or(0)
    }

    /// 自增被调用了几次。
    pub fn increments_of(&self, code: &str) -> u32 {
        self.increments
            .lock()
            .expect("counter mutex poisoned")
            .get(code)
            .copied()
            .unwrap_or(0)
    }
}

impl VisitCounter for RecordingCounter {
    fn pending_of<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<u32, CacheError>> {
        let count = self.pending_of_sync(code);
        Box::pin(async move { Ok(count) })
    }

    fn increment<'a>(
        &'a self,
        code: &'a str,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<u32, CacheError>> {
        let mut pending = self.pending.lock().expect("counter mutex poisoned");
        let total = pending.entry(code.to_string()).or_insert(0);
        *total += 1;
        let total = *total;
        drop(pending);

        *self
            .increments
            .lock()
            .expect("counter mutex poisoned")
            .entry(code.to_string())
            .or_insert(0) += 1;

        Box::pin(async move { Ok(total) })
    }

    fn scan_pending<'a>(&'a self) -> BoxFuture<'a, Result<Vec<PendingVisit>, CacheError>> {
        let batch: Vec<PendingVisit> = self
            .pending
            .lock()
            .expect("counter mutex poisoned")
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(code, count)| PendingVisit {
                code: code.clone(),
                count: *count,
            })
            .collect();

        Box::pin(async move { Ok(batch) })
    }

    fn consume<'a>(&'a self, visit: PendingVisit) -> BoxFuture<'a, Result<(), CacheError>> {
        let mut pending = self.pending.lock().expect("counter mutex poisoned");
        if let Some(count) = pending.get_mut(&visit.code) {
            *count = count.saturating_sub(visit.count);
            if *count == 0 {
                pending.remove(&visit.code);
            }
        }
        drop(pending);

        Box::pin(async { Ok(()) })
    }
}

/// 按脚本返回短码的生成器：脚本用尽后重复最后一个。
///
/// 这样既能模拟「第一次撞码、第二次成功」，也能模拟「永远撞码」。
pub struct ScriptedCodeGenerator {
    queue: Mutex<VecDeque<String>>,
    fallback: String,
    taken: AtomicUsize,
}

impl ScriptedCodeGenerator {
    pub fn new(codes: &[&str]) -> Self {
        let fallback = codes.last().copied().unwrap_or("fixed").to_string();
        Self {
            queue: Mutex::new(codes.iter().map(|code| code.to_string()).collect()),
            fallback,
            taken: AtomicUsize::new(0),
        }
    }

    /// 已经被取走的短码数量，用于断言碰撞重试次数。
    pub fn taken(&self) -> usize {
        self.taken.load(Ordering::Relaxed)
    }
}

impl ShortCodeGenerator for ScriptedCodeGenerator {
    fn generate(&self, _length: usize) -> String {
        self.taken.fetch_add(1, Ordering::Relaxed);
        let mut queue = self.queue.lock().expect("code generator mutex poisoned");
        queue.pop_front().unwrap_or_else(|| self.fallback.clone())
    }
}

fn repo_failure() -> RepoError {
    RepoError::Backend(Box::new(std::io::Error::other("storage unavailable")))
}

/// 所有操作都失败的仓储，用于验证 5xx 路径。
///
/// 与「查不到记录」不同：[`RepoError::NotFound`] 是确定的业务结论，
/// 而这里模拟的是存储不可用 —— 调用方对两者的处理必须不同。
pub struct BrokenRepository;

impl LinkRepository for BrokenRepository {
    fn insert<'a>(
        &'a self,
        _link: &'a crate::domain::new_link::NewShortLink,
        _code: &'a str,
    ) -> BoxFuture<'a, Result<crate::domain::short_link::ShortLink, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_by_code<'a>(
        &'a self,
        _code: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::domain::short_link::ShortLink>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn add_visit_count<'a>(
        &'a self,
        _code: &'a str,
        _delta: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn list<'a>(
        &'a self,
        _query: &'a LinkQuery,
        _now: chrono::NaiveDateTime,
    ) -> BoxFuture<'a, Result<LinkPage, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn overview(
        &self,
        _window: OverviewWindow,
        _owner: Option<u64>,
    ) -> BoxFuture<'_, Result<LinkOverview, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn delete_by_code<'a>(&'a self, _code: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn list_by_owner(
        &self,
        _owner_id: u64,
        _limit: u32,
    ) -> BoxFuture<'_, Result<Vec<crate::domain::short_link::ShortLink>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn delete_owned<'a>(
        &'a self,
        _owner_id: u64,
        _code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn delete_all_owned(&self, _owner_id: u64) -> BoxFuture<'_, Result<Vec<String>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }
}

impl UserRepository for BrokenRepository {
    fn create<'a>(
        &'a self,
        _new: &'a crate::domain::account::NewUser,
        _password_hash: &'a str,
        _access: crate::domain::account::Access,
    ) -> BoxFuture<'a, Result<crate::domain::account::User, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_by_email<'a>(
        &'a self,
        _email: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::domain::account::User>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_by_id(
        &self,
        _id: u64,
    ) -> BoxFuture<'_, Result<Option<crate::domain::account::User>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_by_ids<'a>(
        &'a self,
        _ids: &'a [u64],
    ) -> BoxFuture<'a, Result<Vec<crate::domain::account::User>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_first_admin(
        &self,
    ) -> BoxFuture<'_, Result<Option<crate::domain::account::User>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn list_users<'a>(
        &'a self,
        _query: &'a UserQuery,
    ) -> BoxFuture<'a, Result<UserPage, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn set_password<'a>(
        &'a self,
        _id: u64,
        _password_hash: &'a str,
        _must_change_password: bool,
    ) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn set_status(
        &self,
        _id: u64,
        _status: crate::domain::account::AccountStatus,
    ) -> BoxFuture<'_, Result<bool, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }
}

impl SessionRepository for BrokenRepository {
    fn create<'a>(
        &'a self,
        _user_id: u64,
        _token_hash: &'a str,
        _expires_at: chrono::NaiveDateTime,
    ) -> BoxFuture<'a, Result<(), RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn find_user<'a>(
        &'a self,
        _token_hash: &'a str,
        _now: chrono::NaiveDateTime,
    ) -> BoxFuture<'a, Result<Option<crate::domain::account::User>, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn delete<'a>(&'a self, _token_hash: &'a str) -> BoxFuture<'a, Result<bool, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn delete_for_user(&self, _user_id: u64) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }

    fn purge_expired(&self, _now: chrono::NaiveDateTime) -> BoxFuture<'_, Result<u64, RepoError>> {
        Box::pin(async { Err(repo_failure()) })
    }
}
