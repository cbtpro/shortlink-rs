//! 进程入口。真正的事情都在 `bootstrap` 里做，这里只保留模块声明。

mod api;
mod assets;
mod bootstrap;
mod cache;
mod domain;
mod error;
mod health;
mod repository;
mod security;
mod service;
mod settings;
mod state;
mod tasks;
mod types;

#[cfg(test)]
mod test_support;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::run().await
}
