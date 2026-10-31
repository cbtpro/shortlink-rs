//! 领域层：实体与业务规则。除 `FromRow` 外不依赖 web / redis / sqlx。

pub mod account;
pub mod code;
pub mod new_link;
pub mod short_link;
