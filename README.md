# shortlink-rs

Rust 短链接服务：`actix-web` + `sqlx`(MySQL) + Redis。

- 创建短链、`302` 跳转
- 自定义短码，或按配置长度自动生成
- 过期时间、访问次数上限、访问密码
- 可选「中转页」：跳转前先展示目标地址，由访问者确认
- Redis 承担读缓存与访问计数，后台任务定时回刷 MySQL
- 自带操作界面与管理后台，静态资源内嵌进二进制，无需单独部署前端
- 依赖只指向内层：领域逻辑不依赖 web / redis，测试不依赖中间件

## 快速开始

### 1. 前置

- Rust stable（`rustup default stable`）
- Docker 与 Compose v2（只用于 MySQL / Redis，应用本身跑在宿主机上）

### 2. 启动依赖

```bash
docker compose up -d
docker compose ps          # 两个都显示 healthy 才算就绪
```

首次启动会自动执行 `talbe.sql` 建表。**已有库**升级时需执行 `migrations/` 下的增量 SQL
（按文件名顺序；新库跑 `talbe.sql` 即已包含全部变更）。容器、网络、卷的清单见 [本地依赖](#本地依赖docker-compose)。

### 3. 配置 `.env`

```dotenv
SERVER_PORT=9981
DATABASE_URL=mysql://short_link:short_link@127.0.0.1:3306/short_link
REDIS_URL=redis://127.0.0.1:6379/0
RUST_LOG=info
```

`DATABASE_URL` 的账号、密码、库名须与 compose 里的 `MYSQL_USER` / `MYSQL_PASSWORD` / `MYSQL_DATABASE` 一致，默认值已对齐，开箱可用。其余可配项见 [配置项](#配置项)。

### 4. 运行

```bash
cargo run
```

启动日志会打印脱敏后的依赖地址，`.env` 写错时可直接对照：

```
dependencies ready: mysql=mysql://***@127.0.0.1:3306/short_link redis=redis://127.0.0.1:6379/0
```

### 5. 打开界面

浏览器访问 `http://127.0.0.1:9981/`（端口取自 `.env`）即是操作界面：填目标地址、可选自定义短码与高级选项，生成后一键复制。命令行冒烟：

```bash
# 创建
curl -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "custom_code": "demo"}'

# 跳转，期望 302 + Location
curl -i http://127.0.0.1:9981/demo
```

## 接口速览

| 方法 | 路径 | 说明 | 成功状态 |
| --- | --- | --- | --- |
| `GET` | `/` | 操作界面（单页） | `200` |
| `POST` | `/shorten` | 创建短链 | `201` |
| `GET` | `/{code}` | 跳转（开启中转页时先返回确认页 `200`），可选 `?password=` | `302` |
| `GET` | `/recent` | 当前浏览器的最近创建 | `200` |
| `DELETE` | `/recent/{code}` | 删除一条创建记录 | `204` |
| `DELETE` | `/recent` | 清空当前浏览器的创建记录 | `204` |
| `GET` | `/admin` | 管理后台（需 `ADMIN_TOKEN`） | `200` |
| `GET` | `/admin/api/overview` | 概览统计 | `200` |
| `GET` | `/admin/api/links` | 分页列表，支持搜索与状态筛选 | `200` |
| `DELETE` | `/admin/api/links/{code}` | 删除短链并清缓存 | `204` |

后台接口用 `Authorization: Bearer <ADMIN_TOKEN>` 鉴权。`/recent*` 按 `sl_visitor`
匿名 cookie 区分浏览器（HttpOnly + SameSite=Lax），创建记录落在 `link_creations` 表；
`/assets/**` 下的样式与 ES 模块、`/favicon.ico` 是页面资源，注册在 `/{code}` 兜底路由之前。
字段、示例与完整状态码见 [docs/API.md](docs/API.md)。

## 项目结构

按「依赖只指向内层」分层，`main.rs` 是唯一的组合根：

```
src/
├── main.rs          组合根：读配置、装配依赖、启动服务与后台任务
├── config.rs        全部环境变量入口，消除散落的硬编码
├── error.rs         AppError 与 HTTP 状态码的唯一映射点
├── state.rs         应用级共享状态（Arc<Service>）
├── types.rs         跨层类型别名
├── domain/          领域层：实体、校验规则、访问策略（不依赖 web/redis）
│   ├── short_link.rs    ShortLink / CachedLink / AccessPolicy / LinkState
│   ├── new_link.rs      NewShortLink 与校验
│   └── code.rs          短码生成策略与合法性
├── repository/      持久化端口（含列表/统计/删除）+ MySQL 实现（+ 测试用内存实现）
├── cache/           缓存端口 + Redis 实现（内容缓存 / 访问计数 / 失效清理）
├── service/         用例编排：shorten（创建）/ redirect（跳转）/ admin（后台）/ history（最近创建）
├── api/             HTTP 适配：DTO + handler，只做协议转换（assets 内嵌前端资源）
├── tasks/           后台任务：访问计数回刷
└── test_support.rs  测试替身（仅 cargo test 编译）

web/                 前端页面，include_str! 编进二进制，改完需重新构建
├── index.html       首页（创建短链）
├── admin.html       管理后台（概览 / 列表 / 删除）
├── interstitial.html 中转页模板，占位符由 api::interstitial 转义后填充
└── assets/          由 /assets/{path} 单条路由分发（表见 src/api/assets.rs）
    ├── css/
    │   ├── base.css     配色令牌、基础排版与通用控件（各页共用）
    │   ├── home.css     首页样式
    │   ├── admin.css    后台样式
    │   └── interstitial.css 中转页样式
    └── js/
        ├── core/        跨页面基础件：DOM 构造、请求层、格式化、吐司、剪贴板
        ├── features/    业务封装：后台 API 客户端、最近创建客户端
        └── pages/       页面入口，原生 ES Modules（无构建步骤）
```

约定：

- 新增一种存储或缓存，实现 `repository::LinkRepository` / `cache::LinkCache`，在 `main.rs` 换一行装配；
- 新增业务规则（如 IP 白名单）加在 `domain::AccessPolicy`，handler 与仓储都不用动；
- 接口字段调整只改 `api/dto.rs`，不波及领域模型；
- 新增路由在 `api::configure` 挂载。

## 配置项

| 变量 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `DATABASE_URL` | 是 | — | MySQL 连接串 |
| `REDIS_URL` | 是 | — | Redis 连接串 |
| `SERVER_HOST` | 否 | `0.0.0.0` | 监听地址 |
| `SERVER_PORT` | 否 | `8080` | 监听端口；本仓库 `.env` 用 `9981` |
| `PUBLIC_BASE_URL` | 否 | `http://127.0.0.1:$SERVER_PORT` | 返回给用户的短链域名 |
| `DATABASE_MAX_CONNECTIONS` | 否 | `10` | 连接池上限 |
| `CACHE_LINK_TTL_SECONDS` | 否 | `3600` | 短链缓存存活时间 |
| `VISIT_FLUSH_INTERVAL_SECONDS` | 否 | `300` | 访问计数回刷间隔 |
| `SHORT_CODE_LENGTH` | 否 | `7` | 自动生成短码长度（须 ≤ 10） |
| `HISTORY_RECENT_LIMIT` | 否 | `20` | 每个浏览器保留的「最近创建」条数（上限 50） |
| `ADMIN_TOKEN` | 否 | — | 后台口令，≥ 8 字符；**不配置则后台整体关闭**（`/admin*` 一律 404） |
| `RUST_LOG` | 否 | `info` | 日志级别 |

缺必填项时进程启动即失败并指明是哪个变量，不会 panic。

## 本地依赖：Docker Compose

应用不进容器（保留 `cargo watch` 热重载），只把 MySQL 与 Redis 交给 Docker：

```bash
docker compose up -d          # 启动
docker compose ps             # 看状态，两个都是 healthy 才算就绪
docker compose logs -f mysql  # 首次启动看建表是否成功
docker compose down           # 停止（数据保留在卷里）
docker compose down -v        # 停止并删卷 —— 数据与建表记录一起清空
```

### 创建的资源

| 类型 | 名称 | 说明 |
| --- | --- | --- |
| 网络 | `shortlink-net` | bridge 驱动，两容器同网段，可用服务名互访（`mysql:3306`、`redis:6379`） |
| 卷 | `shortlink-mysql-data` | 挂到 `/var/lib/mysql`，删容器不丢数据 |
| 卷 | `shortlink-redis-data` | 挂到 `/data`，开启 AOF（`everysec`） |
| 容器 | `shortlink-mysql` | MySQL 8.4，`utf8mb4` + `+08:00` |
| 容器 | `shortlink-redis` | Redis 7.4 alpine，`noeviction` |

两个端口默认只绑定 `127.0.0.1`，避免局域网直接扫到无密码 Redis。

### 首次初始化

`talbe.sql` 以只读方式挂到 `/docker-entrypoint-initdb.d/10-schema.sql`，**仅在数据卷为空时执行**。改了 schema 要重新初始化：

```bash
docker compose down -v && docker compose up -d
```

### 可调参数

全部通过环境变量覆盖（也可写进 `.env`，Compose 会自动读取）：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `MYSQL_VERSION` | `8.4` | MySQL 镜像版本 |
| `MYSQL_ROOT_PASSWORD` | `root` | root 密码，仅本地开发 |
| `MYSQL_DATABASE` | `short_link` | 初始库名，须与 `DATABASE_URL` 一致 |
| `MYSQL_USER` / `MYSQL_PASSWORD` | `short_link` | 应用账号，同样须与 `DATABASE_URL` 一致 |
| `MYSQL_PORT` / `REDIS_PORT` | `3306` / `6379` | 宿主机端口 |
| `MYSQL_BIND` / `REDIS_BIND` | `127.0.0.1` | 改成 `0.0.0.0` 才对局域网开放 |
| `REDIS_VERSION` | `7.4-alpine` | Redis 镜像版本 |
| `TZ` | `Asia/Shanghai` | 容器时区 |

## 时间与时区

结论：**接口用本地时间，存储用 UTC**。

- 入参 `expire_at` 与出参 `created_at` 都是本地时间：填 `2026-12-31T23:59:59` 就是本地 23:59 失效；
- 数据库与 Redis 里一律存 UTC。sqlx 会给每条连接下发 `time_zone='+00:00'`，所以 `DATETIME` 列的 `CURRENT_TIMESTAMP` 默认值也是 UTC；
- 换算只发生在 `src/api/dto.rs`（`local_to_utc` / `utc_to_local`），领域层与存取层不做任何时区处理；
- 时间列用 `DATETIME` 而非 `TIMESTAMP`：后者会被 sqlx 映射成 `DateTime<Utc>`（与 `NaiveDateTime` 不兼容），且有 2038 上限。

用 SQL 客户端直接查库看到的值比本地时间早 8 小时，属正常现象。存量库若还是 `TIMESTAMP` 列，执行一次：

```sql
ALTER TABLE short_links
  MODIFY `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
  MODIFY `expire_at` DATETIME NULL DEFAULT NULL;
```

## 测试

```bash
cargo test          # 全部用例离线跑，不需要 MySQL / Redis
cargo test domain:: # 只跑领域层
```

分层策略、测试替身与未覆盖范围见 [docs/TESTING.md](docs/TESTING.md)。

## 开发

```bash
cargo install cargo-watch   # 一次性
cargo watch -x run          # 保存即重启
cargo watch -x test         # 保存即跑测试
```

Rust 是静态编译语言，没有真正意义上的热更新，`cargo watch` 只是把「保存 → 重编译 → 重启」自动化。

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 分层与依赖方向、调用链、关键决策与权衡、扩展指引 |
| [docs/API.md](docs/API.md) | 接口字段、响应示例、错误文案、状态码总表 |
| [docs/TESTING.md](docs/TESTING.md) | 三层测试策略、用例分布、测试替身、手工冒烟脚本 |

## 已知限制

- **访问密码是明文比对**，未做哈希；要切换需引入 `PasswordHasher` 端口并迁移存量数据。
- **`ip_limit` / `ua_limit` 只落库，不参与判定**。当前唯一影响是带这两个字段的链接不进缓存。
- 创建短链的公开接口没有鉴权；后台接口由 `ADMIN_TOKEN` 保护，但令牌经请求头明文传输，仅在 HTTPS 或内网环境使用。
- 后台概览里的「总访问」只含已回刷到库的计数，最近几分钟的增量不计入。
- 没有编辑、批量操作与访问日志；界面的「最近创建」按匿名 cookie 记在服务端，换设备或清除 cookie 后不可见，也不代表任何账号。
- 建表脚本文件名 `talbe.sql` 是 `table.sql` 的笔误，暂未重命名；项目也没有 migration 机制，改表只能手写 SQL。
- `.env` 目前被 Git 跟踪且含明文凭据，建议改为 `.env.example` + 本地 `.env`。
