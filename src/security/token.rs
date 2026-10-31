//! 会话令牌。
//!
//! 令牌是一串 256 位随机数，只在这一刻存在于响应里：数据库存的是它的 SHA-256 指纹，
//! 所以即使库被拖走，也拿不到能用的会话。指纹不需要盐 —— 令牌本身已经是高熵随机值，
//! 不存在被彩虹表反查的问题。

use rand::Rng;
use sha2::{Digest, Sha256};

use super::password::to_hex;

/// 256 位熵，远超暴力猜测的可行范围。
const TOKEN_BYTES: usize = 32;
/// 十六进制表示的长度，也是 cookie 里出现的字符数。
pub const TOKEN_LEN: usize = TOKEN_BYTES * 2;

/// 签发一个新令牌（明文，仅返回给调用方写进 cookie）。
pub fn mint() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::rng().random();
    to_hex(&bytes)
}

/// 令牌指纹，入库与查询都用它。
pub fn fingerprint(token: &str) -> String {
    to_hex(&Sha256::digest(token.as_bytes()))
}

/// 格式校验。不合法的一律当作「没有令牌」，避免任意字符串被拿去查库。
pub fn is_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_lowercase_hex_of_fixed_length() {
        let token = mint();
        assert_eq!(token.len(), TOKEN_LEN);
        assert!(is_well_formed(&token));
    }

    #[test]
    fn mints_distinct_tokens() {
        let tokens: std::collections::HashSet<String> = (0..64).map(|_| mint()).collect();
        assert_eq!(tokens.len(), 64);
    }

    #[test]
    fn fingerprint_is_stable_and_hides_the_token() {
        let token = mint();
        assert_eq!(fingerprint(&token), fingerprint(&token));
        assert_eq!(fingerprint(&token).len(), 64);
        assert!(!fingerprint(&token).contains(&token));
        assert_ne!(fingerprint(&token), fingerprint(&mint()));
    }

    #[test]
    fn rejects_tampered_tokens() {
        assert!(!is_well_formed(""));
        assert!(!is_well_formed("0123456789ABCDEF"));
        assert!(!is_well_formed(&"g".repeat(TOKEN_LEN)));
        assert!(!is_well_formed(&"a".repeat(TOKEN_LEN - 1)));
        assert!(!is_well_formed(&"a".repeat(TOKEN_LEN + 1)));
    }
}
