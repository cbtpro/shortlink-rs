//! 口令散列：PBKDF2-HMAC-SHA256。
//!
//! 为什么是 PBKDF2 而不是 Argon2 / bcrypt：这两者需要额外的 crate，本项目在离线环境
//! 里构建，能用的只有 `sha2` + `hmac`（本就是 sqlx 的传递依赖）。PBKDF2 的问题只在
//! 「内存硬度」上不如 Argon2，用它时把迭代次数调够即可，OWASP 对 PBKDF2-SHA256 的
//! 建议值是 600k。
//!
//! 编码格式自带算法与参数：`pbkdf2-sha256$<迭代次数>$<盐 hex>$<散列 hex>`。
//! 这样将来提高迭代次数时，老口令仍能用它自己的参数验证，不需要强制所有人改密码。

use hmac::{Hmac, Mac};
use rand::seq::IndexedRandom;
use rand::Rng;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

const ALGORITHM: &str = "pbkdf2-sha256";
/// 128 位盐，碰撞可以忽略。
const SALT_LEN: usize = 16;
/// SHA-256 输出长度，也是 PBKDF2 的 dkLen。
const HASH_LEN: usize = 32;
/// OWASP 对 PBKDF2-HMAC-SHA256 的建议值。
pub const DEFAULT_ITERATIONS: u32 = 600_000;
/// 下限用于测试与本地开发；生产配置不应低于默认值。
pub const MIN_ITERATIONS: u32 = 1_000;
/// 上限是防御性的：编码串来自数据库，被篡改成十亿次会让一次登录拖死整个线程。
const MAX_ITERATIONS: u32 = 4_000_000;

/// 口令散列端口。用例层只依赖它，测试可以塞一个不计时的替身。
pub trait PasswordHasher: Send + Sync {
    fn hash(&self, password: &str) -> String;

    /// 校验口令。编码串格式不对时返回 `false` 而不是报错 ——
    /// 对上层的语义只有一个：「这个口令不对」。
    fn verify(&self, password: &str, encoded: &str) -> bool;
}

pub struct Pbkdf2Hasher {
    iterations: u32,
}

impl Pbkdf2Hasher {
    /// 迭代次数会被收敛到 `[MIN_ITERATIONS, MAX_ITERATIONS]`：
    /// 配置写成 0 会让散列退化成明文，写成天文数字会让登录卡死。
    pub fn new(iterations: u32) -> Self {
        Self {
            iterations: clamp_iterations(iterations),
        }
    }
}

fn clamp_iterations(raw: u32) -> u32 {
    raw.clamp(MIN_ITERATIONS, MAX_ITERATIONS)
}

impl PasswordHasher for Pbkdf2Hasher {
    fn hash(&self, password: &str) -> String {
        let salt = random_salt();
        let derived = pbkdf2(password.as_bytes(), &salt, self.iterations);

        format!(
            "{ALGORITHM}${}${}${}",
            self.iterations,
            to_hex(&salt),
            to_hex(&derived)
        )
    }

    fn verify(&self, password: &str, encoded: &str) -> bool {
        let Some((iterations, salt, expected)) = parse(encoded) else {
            log::warn!("rejecting malformed password hash");
            return false;
        };

        let actual = pbkdf2(password.as_bytes(), &salt, iterations);
        actual.ct_eq(expected.as_slice()).into()
    }
}

/// 拆解编码串。任何一段不合规都整体判废，不做「尽力而为」的容错。
fn parse(encoded: &str) -> Option<(u32, Vec<u8>, Vec<u8>)> {
    let mut parts = encoded.split('$');
    if parts.next()? != ALGORITHM {
        return None;
    }

    let iterations: u32 = parts.next()?.parse().ok()?;
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&iterations) {
        return None;
    }

    let salt = from_hex(parts.next()?)?;
    let hash = from_hex(parts.next()?)?;
    if parts.next().is_some() || salt.len() != SALT_LEN || hash.len() != HASH_LEN {
        return None;
    }

    Some((iterations, salt, hash))
}

/// PBKDF2，输出长度恰好一个 SHA-256 块，所以只需要算第一块。
///
/// 循环里用 `finalize_reset` 而不是重建 HMAC：重置只把内部状态退回 ipad 之后的位置，
/// 省掉每轮重算一次密钥编排。10 万轮下这是实打实的一半工作量。
fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; HASH_LEN] {
    let mut mac = new_mac(password);
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut block = mac.finalize_reset().into_bytes();

    let mut out = block;
    for _ in 1..iterations {
        mac.update(&block);
        block = mac.finalize_reset().into_bytes();
        for (target, byte) in out.iter_mut().zip(block.iter()) {
            *target ^= byte;
        }
    }

    out.into()
}

fn new_mac(password: &[u8]) -> HmacSha256 {
    // HMAC 接受任意长度的密钥，这里不会失败
    HmacSha256::new_from_slice(password).expect("hmac accepts keys of any length")
}

fn random_salt() -> [u8; SALT_LEN] {
    rand::rng().random()
}

/// 生成一次性口令，用于管理员初始密码与重置密码。
///
/// 字母表刻意去掉 `0/O`、`1/l/I` —— 这个口令要用户手抄一遍再登录，
/// 分不清的字符只会换来一次失败尝试。52 + 8 = 60 个符号，20 位约 118 bit，
/// 在「改密前有效、改密后作废」的窗口里足够。
const PASSWORD_ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn random_password(length: usize) -> String {
    let mut rng = rand::rng();
    (0..length.clamp(8, 64))
        .map(|_| {
            let index = PASSWORD_ALPHABET
                .choose(&mut rng)
                .copied()
                .expect("alphabet is never empty");
            char::from(index)
        })
        .collect()
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }

    let bytes = text.as_bytes();
    (0..bytes.len() / 2)
        .map(|index| {
            let high = char::from(bytes[index * 2]).to_digit(16)?;
            let low = char::from(bytes[index * 2 + 1]).to_digit(16)?;
            Some((high * 16 + low) as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用低迭代，验证的是算法而不是耗时。
    fn fast() -> Pbkdf2Hasher {
        Pbkdf2Hasher::new(MIN_ITERATIONS)
    }

    #[test]
    fn hash_round_trips() {
        let hasher = fast();
        let encoded = hasher.hash("correct horse battery staple");

        assert!(hasher.verify("correct horse battery staple", &encoded));
        assert!(!hasher.verify("correct horse battery stapl", &encoded));
        assert!(!hasher.verify("", &encoded));
    }

    #[test]
    fn encoded_string_carries_its_own_parameters() {
        let hasher = fast();
        let encoded = hasher.hash("secret");

        assert!(encoded.starts_with("pbkdf2-sha256$1000$"));
        let parts: Vec<&str> = encoded.split('$').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[2].len(), SALT_LEN * 2);
        assert_eq!(parts[3].len(), HASH_LEN * 2);
    }

    #[test]
    fn same_password_hashes_differently_each_time() {
        // 盐是随机的，两次散列结果必须不同，否则一眼能看出哪些人同密码
        let hasher = fast();
        assert_ne!(hasher.hash("secret"), hasher.hash("secret"));
    }

    #[test]
    fn accepts_hash_produced_with_other_iterations() {
        // 调高迭代次数后，老口令仍要能验证通过
        let old = Pbkdf2Hasher::new(MIN_ITERATIONS).hash("secret");
        assert!(fast().verify("secret", &old));
        assert!(Pbkdf2Hasher::new(MIN_ITERATIONS * 2).verify("secret", &old));
    }

    #[test]
    fn rejects_malformed_hashes() {
        let hasher = fast();
        for broken in [
            "",
            "secret",
            "pbkdf2-sha256$abc$00$00",
            "pbkdf2-sha256$1000$not-hex$00",
            "pbkdf2-sha256$1000$00ff$00ff",
            "bcrypt$1000$00ff$00ff",
            "pbkdf2-sha256$1000$00000000000000000000000000000000$00ff$extra",
            // 迭代次数为 0 会让散列退化成无迭代，必须拒绝
            "pbkdf2-sha256$0$00000000000000000000000000000000$0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert!(!hasher.verify("secret", broken), "应拒绝：{broken}");
        }
    }

    #[test]
    fn clamps_iterations_to_a_sane_range() {
        // 单独测收敛函数：真拿 4M 次迭代去散列会让这个用例跑上十几秒
        assert_eq!(clamp_iterations(0), MIN_ITERATIONS);
        assert_eq!(clamp_iterations(u32::MAX), MAX_ITERATIONS);
        assert_eq!(clamp_iterations(DEFAULT_ITERATIONS), DEFAULT_ITERATIONS);
    }

    #[test]
    fn default_iterations_follow_owasp() {
        // OWASP 对 PBKDF2-HMAC-SHA256 的建议值；调低等于给自己减轻防护
        const OWASP_MINIMUM: u32 = 600_000;
        assert_eq!(DEFAULT_ITERATIONS.max(OWASP_MINIMUM), DEFAULT_ITERATIONS);
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00, 0x0f, 0xa5, 0xff];
        assert_eq!(to_hex(&bytes), "000fa5ff");
        assert_eq!(from_hex("000fa5ff").unwrap(), bytes);
        assert!(from_hex("000fa5f").is_none(), "奇数长度不合法");
        assert!(from_hex("zz").is_none());
    }

    #[test]
    fn pbkdf2_matches_published_vector() {
        // RFC 6070 风格的已知答案：P="password", S="salt", c=1, dkLen=32
        let derived = pbkdf2(b"password", b"salt", 1);
        assert_eq!(
            to_hex(&derived),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
    }

    #[test]
    fn random_password_is_long_and_unambiguous() {
        let first = random_password(20);
        let second = random_password(20);

        assert_eq!(first.chars().count(), 20);
        assert_ne!(first, second, "两次生成不能相同");
        for ch in first.chars() {
            assert!(
                PASSWORD_ALPHABET.contains(&(ch as u8)),
                "{ch} 不在可用字符集里"
            );
            assert!(!"0O1lI".contains(ch), "易混字符不该出现在要手抄的口令里");
        }
    }

    #[test]
    fn random_password_keeps_a_usable_length() {
        // 传 0 会生成空串，直接绕口令规则；此处收敛到下限
        assert_eq!(random_password(0).chars().count(), 8);
        assert_eq!(random_password(999).chars().count(), 64);
    }
}
