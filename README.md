# shortlink-rs

```shell
cargo build
cargo run
cargo run --bin shortlink-rs
cargo run --release
cargo run --bin shortlink-rs --release
cargo build --release
cargo run --release --bin shortlink-rs
```

安装 cargo-edit 工具

```shell
# 安装cargo-edit工具
cargo install cargo-edit
```

使用 cargo add 添加依赖

```shell
# 基础依赖
# Actix-Web 框架
cargo add actix-web@4

# SQLx (MySQL 支持 + Tokio 运行时)
cargo add sqlx --features mysql,runtime-tokio-native-tls

# Redis 客户端 (Tokio 兼容)
cargo add redis --features tokio-comp

# 环境变量管理
cargo add dotenvy --no-default-features

# 异步运行时
cargo add tokio --features full

# 可选序列化工具
cargo add serde --features derive
cargo add serde_json
```

```
# 访问短链接
curl -i http://127.0.0.1:9981/oBNN5a6xa6FQOFJz

# 获取短链接
curl -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com"}'
```
