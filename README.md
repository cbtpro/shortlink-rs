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
  -d '{  "url": "http://127.0.0.1:10000/demo/all",  "custom_code": "mycode123",  "expire_at": "2025-12-31T23:59:59",  "max_visits": 100,  "password": "123456",  "ip_limit": { "whitelist": ["1.2.3.4"] },"ua_limit": { "blacklist": ["curl", "wget"]}}'
```

在使用 `rustcover`（你可能是指 `cargo watch` + `tarpaulin` 或其他调试工具组合）进行调试或代码覆盖率检测时，Rust 代码\*\*默认是不会热更新（Hot Reload）\*\*的，因为 Rust 是静态编译语言，每次代码修改都需要重新编译。

---

### 热重载开发体验

#### `cargo-watch`

这个工具可以在你保存文件时自动运行指定命令。

#### 安装：

```bash
cargo install cargo-watch
```

#### 使用方式示例：

```bash
cargo watch -x run
```

每次保存代码，`cargo watch` 会自动执行 `cargo run`。

你也可以用于测试或覆盖率工具：

```bash
cargo watch -x test
cargo watch -x "tarpaulin --out Html"
```
