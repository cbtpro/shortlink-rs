//! 仓储端口。

use std::fmt;

use chrono::NaiveDateTime;

use crate::domain::account::{NewUser, User};
use crate::domain::new_link::NewShortLink;
use crate::domain::short_link::ShortLink;
use crate::types::BoxFuture;

pub mod mysql;

#[cfg(test)]
pub mod memory;

/// 仓储错误。不暴露 `sqlx::Error`，避免存储细节渗透到上层。
#[derive(Debug)]
pub enum RepoError {
    DuplicateCode,
    DuplicateEmail,
    NotFound,
    Backend(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoError::DuplicateCode => write!(f, "short code already exists"),
            RepoError::DuplicateEmail => write!(f, "email is already registered"),
            RepoError::NotFound => write!(f, "short link not found"),
            RepoError::Backend(err) => write!(f, "storage failure: {err}"),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RepoError::Backend(err) => Some(err.as_ref()),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for RepoError {
    fn from(err: sqlx::Error) -> Self {
        RepoError::Backend(Box::new(err))
    }
}

/// 后台列表的状态筛选。
///
/// 与 [`crate::domain::short_link::LinkState`] 的取值刻意多一个 `All`：
/// 筛选条件是「查询意图」，状态是「单条记录的结论」，两者不该共用一个枚举。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkFilter {
    #[default]
    All,
    Active,
    Expired,
    Exhausted,
}

/// 后台列表的查询条件。
#[derive(Clone, Debug)]
pub struct LinkQuery {
    /// 从 1 开始
    pub page: u32,
    pub page_size: u32,
    pub keyword: Option<String>,
    pub filter: LinkFilter,
}

/// 一页结果。
#[derive(Clone, Debug)]
pub struct LinkPage {
    pub items: Vec<ShortLink>,
    /// 满足条件的总条数（不受分页影响）
    pub total: u64,
}

/// 概览统计的时间窗口。
///
/// 三个时间点都由调用方算好再传进来：仓储只负责比较，不自己取时钟，
/// 否则测试无法固定时间，同一次请求里的几个统计也可能跨过零点。
#[derive(Clone, Copy, Debug)]
pub struct OverviewWindow {
    /// 判定「过期」的基准时刻（UTC）
    pub now: NaiveDateTime,
    /// 「今日新增」的起点（UTC）
    pub today_start: NaiveDateTime,
    /// 「即将过期」的上界（UTC）
    pub expiring_before: NaiveDateTime,
}

/// 概览统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkOverview {
    pub total_links: u64,
    pub total_visits: u64,
    pub created_today: u64,
    pub expiring_soon: u64,
}

/// 短链接仓储。
pub trait LinkRepository: Send + Sync {
    /// 写入新链接。短码冲突时返回 [`RepoError::DuplicateCode`]，
    /// 由调用方决定重试还是报错 —— 唯一约束是最终裁判，不再先查后插。
    fn insert<'a>(
        &'a self,
        link: &'a NewShortLink,
        code: &'a str,
    ) -> BoxFuture<'a, Result<ShortLink, RepoError>>;

    fn find_by_code<'a>(
        &'a self,
        code: &'a str,
    ) -> BoxFuture<'a, Result<Option<ShortLink>, RepoError>>;

    /// 累加访问增量，供后台回刷任务使用。
    fn add_visit_count<'a>(
        &'a self,
        code: &'a str,
        delta: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>>;

    /// 分页查询，按创建时间倒序。
    ///
    /// `now` 由调用方传入而不是在此取时钟：分页与状态筛选必须基于同一个时间点，
    /// 否则翻页过程中会看到记录在「有效/已过期」之间跳动。
    fn list<'a>(
        &'a self,
        query: &'a LinkQuery,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<LinkPage, RepoError>>;

    /// 概览统计。
    fn overview(&self, window: OverviewWindow) -> BoxFuture<'_, Result<LinkOverview, RepoError>>;

    /// 按短码删除，返回是否真的删掉了记录。
    fn delete_by_code<'a>(&'a self, code: &'a str) -> BoxFuture<'a, Result<bool, RepoError>>;

    /// 某个用户名下的链接，按创建时间倒序。
    fn list_by_owner(
        &self,
        owner_id: u64,
        limit: u32,
    ) -> BoxFuture<'_, Result<Vec<ShortLink>, RepoError>>;

    /// 删除某人名下的一条链接。
    ///
    /// 归属条件写在 SQL 里而不是「先查再删」：后者在两个请求之间留下窗口，
    /// 也让「不是我的」和「不存在」需要用两种错误表达。
    fn delete_owned<'a>(
        &'a self,
        owner_id: u64,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>>;

    /// 删除某人名下的全部链接，返回被删掉的短码（调用方据此清缓存）。
    fn delete_all_owned(&self, owner_id: u64) -> BoxFuture<'_, Result<Vec<String>, RepoError>>;
}

/// 访客的创建记录。
///
/// 与 [`LinkRepository`] 分开：那是「短链是什么」，这是「谁建过什么」，
/// 两者生命周期也不同（历史可以单独清空，短链不行）。
pub trait CreationHistory: Send + Sync {
    /// 记一笔创建。同一访客对同一短码重复创建时刷新时间，
    /// 只保留最近 `keep` 条，超出的按时间淘汰。
    fn record<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
        keep: u32,
    ) -> BoxFuture<'a, Result<(), RepoError>>;

    /// 该访客最近的创建，按时间倒序。短链已被删除的条目不会出现。
    fn recent<'a>(
        &'a self,
        visitor: &'a str,
        limit: u32,
    ) -> BoxFuture<'a, Result<Vec<ShortLink>, RepoError>>;

    /// 删掉单条记录，返回是否命中。
    fn forget<'a>(
        &'a self,
        visitor: &'a str,
        code: &'a str,
    ) -> BoxFuture<'a, Result<bool, RepoError>>;

    /// 清空该访客的全部记录，返回删除条数。
    fn clear<'a>(&'a self, visitor: &'a str) -> BoxFuture<'a, Result<u64, RepoError>>;
}

/// 账号仓储。
///
/// 与短链仓储分开：两者的唯一约束、查询维度、生命周期都不一样，
/// 合成一个 trait 只会让每个实现都背上一半用不到的方法。
pub trait UserRepository: Send + Sync {
    /// 写入新用户。邮箱重复返回 [`RepoError::DuplicateEmail`]，
    /// 由唯一索引裁决，不做「先查再插」。
    fn create<'a>(
        &'a self,
        new: &'a NewUser,
        password_hash: &'a str,
    ) -> BoxFuture<'a, Result<User, RepoError>>;

    fn find_by_email<'a>(
        &'a self,
        email: &'a str,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>>;
}

/// 会话仓储。
///
/// 只认令牌指纹，不认明文：明文只在签发那一刻存在于响应里。
pub trait SessionRepository: Send + Sync {
    fn create<'a>(
        &'a self,
        user_id: u64,
        token_hash: &'a str,
        expires_at: NaiveDateTime,
    ) -> BoxFuture<'a, Result<(), RepoError>>;

    /// 按指纹取出仍然有效的会话对应的用户。已过期的视为不存在。
    fn find_user<'a>(
        &'a self,
        token_hash: &'a str,
        now: NaiveDateTime,
    ) -> BoxFuture<'a, Result<Option<User>, RepoError>>;

    /// 注销单个会话，返回是否命中。
    fn delete<'a>(&'a self, token_hash: &'a str) -> BoxFuture<'a, Result<bool, RepoError>>;

    /// 清理过期会话，返回删除条数。由后台任务定期调用。
    fn purge_expired(&self, now: NaiveDateTime) -> BoxFuture<'_, Result<u64, RepoError>>;
}
