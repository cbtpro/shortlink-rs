use std::future::Future;
use std::pin::Pin;

/// `async fn in trait` 不是 dyn 兼容的，端口层的异步方法统一用这个别名。
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
