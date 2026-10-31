//! 文件变化监听：轮询 mtime + 大小。
//!
//! 不用 inotify/ReadDirectoryChangesW 是为了零依赖 —— 构建工具本身不该成为
//! 一个新的需要维护的依赖，400ms 的轮询对开发机来说完全无感。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// 永远不该触发重启的目录。
const IGNORED: &[&str] = &[
    "target",
    ".git",
    ".github",
    ".workbuddy",
    "dist",
    "node_modules",
];

#[derive(Debug, PartialEq, Eq, Default)]
pub struct Snapshot(BTreeMap<PathBuf, (SystemTime, u64)>);

impl Snapshot {
    pub fn capture(roots: &[PathBuf]) -> Self {
        let mut entries = BTreeMap::new();
        for root in roots {
            collect(root, root, &mut entries);
        }
        Self(entries)
    }

    /// 与给定快照比较是否已经不同。读不到的状态不参与比较。
    pub fn changed_since(&self, roots: &[PathBuf]) -> bool {
        *self != Self::capture(roots)
    }

    /// 阻塞直到有任何改动。
    pub fn wait_for_change(&mut self, roots: &[PathBuf]) {
        loop {
            std::thread::sleep(POLL_INTERVAL);
            let current = Self::capture(roots);
            if current != *self {
                *self = current;
                return;
            }
        }
    }
}

fn collect(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (SystemTime, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if IGNORED.contains(&name.as_ref()) {
            continue;
        }

        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let path = entry.path();

        if metadata.is_dir() {
            collect(root, &path, out);
            continue;
        }

        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let key = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        out.insert(key, (modified, metadata.len()));
    }
}
