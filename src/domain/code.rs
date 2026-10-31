//! 短码的生成策略与合法性规则。

use rand::distr::Alphanumeric;
use rand::Rng;

/// 短码最短长度
const MIN_CODE_LENGTH: usize = 1;

/// 短码生成策略。
///
/// 抽成 trait 后，切换为号段、雪花或自定义字典只需新增实现，
/// 调用方无需改动（开闭原则）。
pub trait ShortCodeGenerator: Send + Sync {
    fn generate(&self, length: usize) -> String;
}

/// 随机字母数字短码。
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomCodeGenerator;

impl ShortCodeGenerator for RandomCodeGenerator {
    fn generate(&self, length: usize) -> String {
        let mut rng = rand::rng();
        (0..length)
            .map(|_| rng.sample(Alphanumeric))
            .map(char::from)
            .collect()
    }
}

/// 判断自定义短码是否可用：长度在区间内，且只含 URL 安全字符。
pub fn is_valid_code(code: &str, max_length: usize) -> bool {
    let length = code.chars().count();
    (MIN_CODE_LENGTH..=max_length).contains(&length)
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generated_code_respects_length() {
        let code = RandomCodeGenerator.generate(7);
        assert_eq!(code.len(), 7);
        assert!(is_valid_code(&code, 10));
    }

    #[test]
    fn rejects_overlong_code() {
        assert!(!is_valid_code("01234567891", 10));
    }

    #[test]
    fn rejects_unsafe_characters() {
        assert!(!is_valid_code("abc/def", 10));
        assert!(!is_valid_code("abc def", 10));
    }

    #[test]
    fn accepts_dash_and_underscore() {
        assert!(is_valid_code("a-b_c", 10));
    }

    #[test]
    fn accepts_boundary_lengths() {
        assert!(is_valid_code("a", 10));
        assert!(is_valid_code("0123456789", 10));
    }

    #[test]
    fn honours_caller_supplied_max_length() {
        assert!(is_valid_code("01234", 5));
        assert!(!is_valid_code("012345", 5));
    }

    #[test]
    fn rejects_empty_and_blank_codes() {
        assert!(!is_valid_code("", 10));
        assert!(!is_valid_code(" ", 10));
    }

    #[test]
    fn rejects_non_ascii_codes() {
        assert!(!is_valid_code("中文", 10));
        assert!(!is_valid_code("émoji", 10));
    }

    #[test]
    fn zero_length_generation_yields_invalid_code() {
        let code = RandomCodeGenerator.generate(0);
        assert!(code.is_empty());
        // 生成器不做校验，长度约束由调用方（配置）保证
        assert!(!is_valid_code(&code, 10));
    }

    #[test]
    fn generated_codes_are_practically_unique() {
        let generator = RandomCodeGenerator;
        let codes: HashSet<String> = (0..200).map(|_| generator.generate(7)).collect();
        assert_eq!(codes.len(), 200, "62^7 空间内 200 次取值不应碰撞");
    }
}
