use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

/// 短链接数据模型，对应 MySQL 表 `short_links`
#[derive(Clone, Debug, Serialize, Deserialize, FromRow)]
pub struct ShortLink {
    /// 主键 ID，自增
    pub id: u64,

    /// 短链接代码，唯一标识，例如 "abc123"
    pub code: String,

    /// 原始长链接地址
    pub long_url: String,

    /// 创建时间，自动设置为当前时间
    pub created_at: DateTime<Utc>,

    /// 可选：过期时间，超过该时间链接失效
    pub expire_at: Option<DateTime<Utc>>,

    /// 可选：最大访问次数，超过限制后链接失效
    pub max_visits: Option<u32>,

    /// 当前已访问次数
    pub visit_count: u32,

    /// 可选：访问密码，受保护的短链接可设置密码
    pub password: Option<String>,

    /// 可选：IP 限制规则，JSON 格式
    pub ip_limit: Option<Value>,

    /// 可选：User-Agent 限制规则，JSON 格式
    pub ua_limit: Option<Value>,
}
