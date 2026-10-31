//! 前端静态资源：开发读磁盘、生产读内嵌，对上层只暴露「按路径取内容」。
//!
//! 两种模式的行为差异刻意收在“从哪读”这一件事上：
//! 路径校验、Content-Type、ETag 规则完全相同，避免出现“本地好看、线上 404”。

use anyhow::{Context, Result};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::settings::AssetsMode;

/// 构建期生成的资源清单：`EMBEDDED: &[(&str, &[u8])]` 与 `ASSET_STAMP`。
mod manifest {
    include!(concat!(env!("OUT_DIR"), "/assets_manifest.rs"));
}

pub(crate) use manifest::ASSET_STAMP;

const HTML: &str = "text/html; charset=utf-8";
/// 首页用的自动刷新脚本，仅在 `live_reload` 打开时注入。
const LIVE_RELOAD_PATH: &str = "/__dev/live-reload.js";

/// 一次资源读取的结果。`body` 在开发模式下由文件内容拥有，
/// 内嵌模式下借用二进制里的只读切片。
pub struct Asset {
    pub body: Cow<'static, [u8]>,
    pub content_type: &'static str,
    pub etag: String,
}

impl Asset {
    pub fn as_text(&self) -> Option<Cow<'static, str>> {
        match &self.body {
            Cow::Borrowed(bytes) => std::str::from_utf8(bytes).ok().map(Cow::Borrowed),
            Cow::Owned(bytes) => String::from_utf8(bytes.clone()).ok().map(Cow::Owned),
        }
    }
}

/// 资源服务。构造时完成一次校验，运行期只做读取。
pub struct AssetServer {
    source: Source,
    live_reload: bool,
}

enum Source {
    Embedded,
    Disk(PathBuf),
}

impl AssetServer {
    /// 校验并建立服务。开发模式下资源目录必须存在且页面齐全 ——
    /// 宁可启动失败，也不要上线后每个页面都 404 才被发现。
    pub fn new(mode: AssetsMode, root: PathBuf, live_reload: bool) -> Result<Self> {
        let source = match mode {
            AssetsMode::Embedded => Source::Embedded,
            AssetsMode::Disk => {
                let root = root.canonicalize().with_context(|| {
                    format!(
                        "assets root `{}` is not readable, check ASSETS_ROOT",
                        root.display()
                    )
                })?;
                for required in [
                    "index.html",
                    "admin.html",
                    "login.html",
                    "interstitial.html",
                ] {
                    let path = root.join(required);
                    if !path.is_file() {
                        return Err(anyhow::anyhow!("missing asset {}", path.display()));
                    }
                }
                Source::Disk(root)
            }
        };

        Ok(Self {
            source,
            live_reload: live_reload && mode == AssetsMode::Disk,
        })
    }

    pub fn live_reload(&self) -> bool {
        self.live_reload
    }

    pub fn mode(&self) -> AssetsMode {
        match self.source {
            Source::Embedded => AssetsMode::Embedded,
            Source::Disk(_) => AssetsMode::Disk,
        }
    }

    /// 读资源。路径不合法、扩展名不认识、文件不存在都返回 `None`。
    pub fn get(&self, path: &str) -> Option<Asset> {
        let relative = sanitize(path)?;
        let content_type = mime_for(&relative)?;

        match &self.source {
            Source::Embedded => {
                let (_, bytes) = manifest::EMBEDDED
                    .iter()
                    .find(|(name, _)| *name == relative.as_str())?;
                Some(Asset {
                    body: Cow::Borrowed(bytes),
                    content_type,
                    etag: format!("\"{}-{:x}\"", ASSET_STAMP, bytes.len()),
                })
            }
            Source::Disk(root) => {
                let full = root.join(&relative);
                let metadata = std::fs::metadata(&full).ok()?;
                if !metadata.is_file() {
                    return None;
                }
                // 开发模式文件都很小，直接同步读；换来的是改完即生效
                let bytes = std::fs::read(&full).ok()?;
                Some(Asset {
                    body: Cow::Owned(bytes),
                    content_type,
                    etag: disk_etag(&metadata, relative.len()),
                })
            }
        }
    }

    /// 取页面并注入开发期刷新脚本。非 HTML 或注入失败时原样返回。
    pub fn page(&self, path: &str) -> Option<String> {
        let asset = self.get(path)?;
        let mut html = asset.as_text()?.into_owned();

        if self.live_reload {
            Html::inject_before_body_end(
                &mut html,
                &format!("<script type=\"module\" src=\"{LIVE_RELOAD_PATH}\"></script>"),
            );
        }

        Some(html)
    }

    /// 资源版本号，用于浏览器轮询判断「是否该刷新」。
    ///
    /// 内嵌模式直接给构建指纹；开发模式按目录聚合 mtime 与大小，
    /// 不读内容，轮询开销可以忽略。
    pub fn revision(&self) -> String {
        match &self.source {
            Source::Embedded => ASSET_STAMP.to_string(),
            Source::Disk(root) => {
                let mut hasher = Fnv::new();
                let mut files = Vec::new();
                collect_files(root, root, &mut files);
                files.sort();

                for (relative, mtime, len) in files {
                    hasher.write(relative.as_bytes());
                    hasher.write(&mtime.to_le_bytes());
                    hasher.write(&len.to_le_bytes());
                }

                format!("{:016x}", hasher.finish())
            }
        }
    }
}

/// 路径规范化，同时充当安全检查。
fn sanitize(path: &str) -> Option<String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return None;
    }

    let mut parts = Vec::new();
    for part in path.split('/') {
        // 空段（`a//b`）、当前目录、上级目录、隐藏文件一律拒绝
        if part.is_empty() || part.starts_with('.') {
            return None;
        }
        parts.push(part);
    }

    Some(parts.join("/"))
}

fn mime_for(path: &str) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();

    Some(match extension.as_str() {
        "html" | "htm" => HTML,
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => return None,
    })
}

fn disk_etag(metadata: &std::fs::Metadata, path_len: usize) -> String {
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);

    format!("\"{mtime:x}-{:x}-{path_len:x}\"", metadata.len())
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, u128, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };

        if metadata.is_dir() {
            collect_files(root, &path, out);
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);

        out.push((relative, mtime, metadata.len()));
    }
}

/// 在 `</body>` 前插入标签；找不到就退回追加到末尾。
struct Html;

impl Html {
    fn inject_before_body_end(html: &mut String, tag: &str) {
        match html.rfind("</body>") {
            Some(index) => html.insert_str(index, tag),
            None => html.push_str(tag),
        }
    }
}

/// FNV-1a，与 build.rs 里的实现保持一致的分段语义。
struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
        self.0 ^= 0xff;
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded() -> AssetServer {
        AssetServer::new(AssetsMode::Embedded, PathBuf::from("web"), false).unwrap()
    }

    #[test]
    fn bundles_pages_and_assets() {
        let assets = embedded();

        for page in [
            "index.html",
            "admin.html",
            "login.html",
            "interstitial.html",
        ] {
            let asset = assets
                .get(page)
                .unwrap_or_else(|| panic!("{page} 没打进二进制"));
            assert_eq!(asset.content_type, HTML);
            assert!(!asset.body.is_empty());
        }

        for asset in [
            "assets/css/base.css",
            "assets/js/core/dom.js",
            "assets/js/pages/home.js",
        ] {
            assert!(assets.get(asset).is_some(), "{asset} 缺失");
        }
    }

    #[test]
    fn every_referenced_asset_is_bundled() {
        // 页面里写了但没打包 = 线上 404，只有点开才发现
        let assets = embedded();

        for page in [
            "index.html",
            "admin.html",
            "login.html",
            "interstitial.html",
        ] {
            let html = assets.get(page).unwrap().as_text().unwrap().into_owned();
            for path in referenced(&html) {
                assert!(
                    assets.get(&path).is_some(),
                    "{page} 引用了未打包的资源：{path}"
                );
            }
        }
    }

    fn referenced(html: &str) -> Vec<String> {
        html.match_indices("assets/")
            .filter_map(|(start, _)| {
                let rest = &html[start..];
                let end = rest.find(['"', '?'])?;
                Some(rest[..end].to_string())
            })
            .collect()
    }

    #[test]
    fn assigns_content_types_by_extension() {
        assert_eq!(mime_for("a.css"), Some("text/css; charset=utf-8"));
        assert_eq!(mime_for("a.js"), Some("text/javascript; charset=utf-8"));
        assert_eq!(mime_for("a.svg"), Some("image/svg+xml"));
        assert_eq!(mime_for("a.bin"), None);
        assert_eq!(mime_for("noext"), None);
    }

    #[test]
    fn rejects_traversal_and_hidden_files() {
        for path in [
            "../Cargo.toml",
            "css/../../etc/passwd",
            ".env",
            "/etc/passwd",
            "css\\base.css",
            "a//b.css",
            "",
        ] {
            assert!(sanitize(path).is_none(), "{path} 应被拒绝");
        }
        assert_eq!(sanitize("css/base.css").as_deref(), Some("css/base.css"));
    }

    #[test]
    fn etag_is_stable_within_a_build() {
        let assets = embedded();
        let first = assets.get("index.html").unwrap().etag;
        let second = assets.get("index.html").unwrap().etag;

        assert_eq!(first, second);
    }

    #[test]
    fn injects_live_reload_only_when_enabled() {
        let mut html = String::from("<html><body>hi</body></html>");
        Html::inject_before_body_end(&mut html, "<script></script>");
        assert!(html.ends_with("<script></script></body></html>"));

        let plain = embedded();
        assert!(!plain
            .page("index.html")
            .unwrap()
            .contains(live_reload_marker()));
    }

    fn live_reload_marker() -> &'static str {
        LIVE_RELOAD_PATH
    }

    #[test]
    fn injects_when_body_tag_is_missing() {
        let mut html = String::from("<p>fragment</p>");
        Html::inject_before_body_end(&mut html, "<script></script>");
        assert_eq!(html, "<p>fragment</p><script></script>");
    }

    #[test]
    fn disk_mode_reads_files_at_request_time() {
        let root = std::env::temp_dir().join(format!("shortlink-assets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("css")).unwrap();
        for name in [
            "index.html",
            "admin.html",
            "login.html",
            "interstitial.html",
        ] {
            std::fs::write(root.join(name), "<html><body></body></html>").unwrap();
        }
        std::fs::write(root.join("css/base.css"), "body{color:red}").unwrap();

        let assets = AssetServer::new(AssetsMode::Disk, root.clone(), true).unwrap();
        assert!(!assets.page("index.html").unwrap().is_empty());
        assert!(assets
            .page("index.html")
            .unwrap()
            .contains(LIVE_RELOAD_PATH));

        // 改文件后立刻能读到新内容，不需要重建
        std::fs::write(root.join("css/base.css"), "body{color:blue}").unwrap();
        let body = assets.get("css/base.css").unwrap();
        assert_eq!(body.as_text().unwrap(), "body{color:blue}");

        let before = assets.revision();
        std::fs::write(root.join("css/base.css"), "body{color:green}").unwrap();
        assert_ne!(before, assets.revision(), "改文件后版本号必须变化");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_pages_fail_fast() {
        let root = std::env::temp_dir().join(format!("shortlink-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let err = AssetServer::new(AssetsMode::Disk, root.clone(), false)
            .err()
            .expect("缺页面应该拒绝启动");
        assert!(err.to_string().contains("index.html"));

        std::fs::remove_dir_all(&root).ok();
    }
}
