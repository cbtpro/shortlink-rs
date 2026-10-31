//! `.env` 风格配置文件的解析与分层合并。
//!
//! 只做两件事：把文本读成键值对、按层叠出最终结果。不碰进程环境变量，
//! 这样合并逻辑可以脱离全局状态单独测试。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 配置文件里的一个键值对，保持文件中的书写顺序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

/// 一层配置。保留名字与来源路径，出问题时能直接说清「这个值是谁给的」。
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: String,
    pub path: Option<PathBuf>,
    pub entries: Vec<Entry>,
}

impl Layer {
    pub fn parse_str(name: impl Into<String>, content: &str) -> Self {
        Self {
            name: name.into(),
            path: None,
            entries: parse(content),
        }
    }

    /// 读取一层磁盘配置。文件不存在由调用方决定算不算错误。
    pub fn read(name: impl Into<String>, path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self {
            name: name.into(),
            path: Some(path.to_path_buf()),
            entries: parse(&content),
        })
    }
}

/// 解析配置文本。
///
/// 支持空行、`#` 整行注释、`KEY=VALUE`、以及成对引号包裹的值。
/// 行内 `#` 不当注释 —— 连接串和密码里出现 `#` 是合法的。
pub fn parse(content: &str) -> Vec<Entry> {
    content
        .lines()
        .filter_map(|raw| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some(Entry {
                key: key.to_string(),
                value: unquote(value.trim()).to_string(),
            })
        })
        .collect()
}

/// 从低到高叠加各层，后面的覆盖前面的。
pub fn resolve(layers: &[Layer]) -> BTreeMap<String, String> {
    let mut resolved = BTreeMap::new();
    for layer in layers {
        for entry in &layer.entries {
            resolved.insert(entry.key.clone(), entry.value.clone());
        }
    }
    resolved
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(content: &str) -> Vec<(String, String)> {
        parse(content)
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect()
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let parsed = keys(
            "\n# 注释\n   \nSERVER_PORT=9981\n  # 缩进的注释\r\nREDIS_URL=redis://127.0.0.1:6379/0\n",
        );

        assert_eq!(
            parsed,
            vec![
                ("SERVER_PORT".to_string(), "9981".to_string()),
                (
                    "REDIS_URL".to_string(),
                    "redis://127.0.0.1:6379/0".to_string()
                ),
            ]
        );
    }

    #[test]
    fn keeps_hash_inside_value() {
        // 密码里带 # 很常见，不能当注释截断
        assert_eq!(
            keys("PASSWORD=\"a#b#c\""),
            vec![("PASSWORD".to_string(), "a#b#c".to_string())]
        );
    }

    #[test]
    fn strips_matching_quotes_only() {
        assert_eq!(keys("A=\"x\""), vec![("A".to_string(), "x".to_string())]);
        assert_eq!(keys("B='y'"), vec![("B".to_string(), "y".to_string())]);
        // 只有一侧有引号时原样保留，避免误伤
        assert_eq!(keys("C=\"z"), vec![("C".to_string(), "\"z".to_string())]);
        assert_eq!(keys("D=\"\""), vec![("D".to_string(), String::new())]);
    }

    #[test]
    fn ignores_lines_without_assignment() {
        assert!(keys("JUST_A_WORD\n=no-key\n==\n").is_empty());
    }

    #[test]
    fn later_layers_win() {
        let layers = [
            Layer::parse_str("base", "PORT=8080\nHOST=0.0.0.0\n"),
            Layer::parse_str("profile", "PORT=9981\n"),
            Layer::parse_str("local", "PORT=12000\nEXTRA=1\n"),
        ];

        let resolved = resolve(&layers);

        assert_eq!(resolved["PORT"], "12000");
        assert_eq!(resolved["HOST"], "0.0.0.0", "未覆盖的键保留低层取值");
        assert_eq!(resolved["EXTRA"], "1");
    }

    #[test]
    fn value_may_contain_equals_sign() {
        assert_eq!(
            keys("DSN=mysql://u:p@h/db?a=1&b=2"),
            vec![("DSN".to_string(), "mysql://u:p@h/db?a=1&b=2".to_string())]
        );
    }
}
