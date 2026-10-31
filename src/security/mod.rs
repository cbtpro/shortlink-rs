//! 凭据处理：口令散列与会话令牌。
//!
//! 两者都只做纯计算，不碰存储，因此可以直接在领域/用例层单测。
//! 生产参数走配置，测试用小迭代数，避免每个用例都等半秒。

pub mod password;
pub mod token;

pub use password::{PasswordHasher, Pbkdf2Hasher};
