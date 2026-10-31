# shortlink-rs

Rust 短链接服务：`actix-web` + `sqlx`(MySQL) + Redis。

- 创建短链、`302` 跳转
- 自定义短码，或按配置长度自动生成
- 过期时间、访问次数上限、访问密码
- 字段校验分三层：DTO 归一化、领域规则、前端即时反馈，错误落到具体字段
- 可选「中转页」：跳转前先展示目标地址，由访问者确认
- 账号体系：注册 / 登录 / 注销，登录后创建的短链挂在账号下，换设备也能找回
- Redis 承担读缓存与访问计数，后台任务定时回刷 MySQL
- 自带操作界面与管理后台；前端零构建，开发时读磁盘、发布时内嵌进二进制
- 配置分层（base / 环境档 / 本地覆盖 / 环境变量），带校验与启动摘要
- 依赖只指向内层：领域逻辑不依赖 web / redis，测试不依赖中间件

## 快速开始

### 1. 前置

- Rust stable（`rustup default stable`）
- Docker 与 Compose v2（只用于 MySQL / Redis，应用本身跑在宿主机上）

### 2. 一条命令起来

```bash
cargo xtask dev
```

它会依次做四件事：拉起 MySQL / Redis（`docker compose up -d --wait`）、编译、
启动服务、监听源码变化。启动后打开 `http://127.0.0.1:9981/`。

日常开发的两条热更新链路：

| 改了什么 | 怎么生效 |
| --- | --- |
| `src/`、`config/`、`Cargo.toml` | 自动重新编译并重启进程（约几秒） |
| `web/` 下的 HTML / CSS / JS | 不用重启，浏览器自己刷新（轮询 `/__dev/revision`） |

嫌吵就分开做：`cargo xtask dev --no-infra` 只跑应用，`cargo xtask db up` 管中间件。

### 3. 配置

**默认不需要任何配置文件**，仓库自带 `config/development.env`。要覆盖时新建 `.env.local`
（不入库），模板见 `.env.example`；生产环境用环境变量，详见 [配置](#配置)。

首次启动会自动执行 `talbe.sql` 建表。**已有库**升级时需执行 `migrations/` 下的增量 SQL
（按文件名顺序；新库跑 `talbe.sql` 即已包含全部变更）。容器、网络、卷的清单见 [本地依赖](#本地依赖docker-compose)。

### 4. 冒烟

```bash
curl -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" \
  -d '{"url": "https://example.com", "custom_code": "demo"}'

curl -i http://127.0.0.1:9981/demo      # 期望 302 + Location
curl -s http://127.0.0.1:9981/healthz   # 期望 {"status":"ok",...}
```

启动日志会打印脱敏后的依赖地址与生效的配置档，`.env` 写错时可直接对照：

```
profile=development addr=0.0.0.0:9981 public=http://127.0.0.1:9981 db=mysql://***@127.0.0.1:3306/short_link ...
```

## 接口速览

| 方法 | 路径 | 说明 | 成功状态 |
| --- | --- | --- | --- |
| `GET` | `/` | 操作界面（单页） | `200` |
| `GET` | `/login` | 登录 / 注册页 | `200` |
| `POST` | `/auth/register` | 注册，成功后直接登录 | `201` |
| `POST` | `/auth/login` | 登录 | `200` |
| `POST` | `/auth/logout` | 注销 | `204` |
| `GET` | `/auth/me` | 当前登录用户，未登录返回 `{"user":null}` | `200` |
| `POST` | `/shorten` | 创建短链（登录时归属到账号） | `201` |
| `GET` | `/{code}` | 跳转（开启中转页时先返回确认页 `200`），可选 `?password=` | `302` |
| `GET` | `/recent` | 最近的创建（登录看账号下全部，未登录看本浏览器） | `200` |
| `DELETE` | `/recent/{code}` | 删除一条（登录即短链失效，未登录只移出清单） | `204` |
| `DELETE` | `/recent` | 清空（仅未登录身份可用） | `204` |
| `GET` | `/admin` | 管理后台（需 `ADMIN_TOKEN`） | `200` |
| `GET` | `/admin/api/overview` | 概览统计 | `200` |
| `GET` | `/admin/api/links` | 分页列表，支持搜索与状态筛选 | `200` |
| `DELETE` | `/admin/api/links/{code}` | 删除短链并清缓存 | `204` |
| `GET` | `/app-config.js` | 前端运行时配置（站点名、接口前缀、环境） | `200` |
| `GET` | `/healthz` | 存活检查，不碰依赖 | `200` |
| `GET` | `/readyz` | 就绪检查，探测 MySQL 与 Redis | `200` / `503` |
| `GET` | `/__dev/revision` | 资源版本号，供浏览器轮询（仅开发模式） | `200` / `404` |

会话用 `sl_session` cookie（HttpOnly + SameSite=Lax，库中只存令牌的 SHA-256 指纹）。
`/recent*` 与 `POST /shorten` 的行为按身份分流：**登录**时看 `short_links.user_id`，
删除即短链失效；**未登录**时退回 `sl_visitor` 匿名 cookie + `link_creations` 表，
删除只是移出自己的清单。后台接口用 `Authorization: Bearer <ADMIN_TOKEN>` 鉴权。
`/assets/**` 下的样式与 ES 模块、`/favicon.ico` 是页面资源，
注册在 `/{code}` 兜底路由之前。字段、示例与完整状态码见 [docs/API.md](docs/API.md)。

## 项目结构

按「依赖只指向内层」分层，`bootstrap.rs` 是唯一的组合根：

```
Cargo.toml            工作区：应用 + xtask
build.rs              构建期扫描 web/，生成资源清单与内容指纹
config/
├── base.env          所有环境共享的默认值，编译进二进制
├── development.env   开发档：前端直读磁盘 + 热刷新 + debug 日志
├── test.env          测试档
└── production.env    生产档骨架（敏感项留空，由环境变量注入）
xtask/src/            构建工具：dev / build / release / check / fmt / db
src/
├── main.rs          进程入口，只转发给 bootstrap
├── bootstrap.rs     组合根：装配依赖、启动服务、日志与端口错误处理
├── settings/        分层配置：Profile、多层 env 合并、校验、脱敏摘要
│   └── env_file.rs     .env 解析与按层覆盖（纯函数，可单测）
├── assets/          前端资源：disk / embedded 两种来源、ETag、版本号
├── error.rs         AppError 与 HTTP 状态码的唯一映射点
├── health.rs        /readyz 的依赖探活
├── state.rs         应用级共享状态（Arc<Service> + 配置快照）
├── types.rs         跨层类型别名
├── domain/          领域层：实体、校验规则、访问策略（不依赖 web/redis）
│   ├── short_link.rs    ShortLink / CachedLink / AccessPolicy / LinkState
│   ├── new_link.rs      NewShortLink 与校验
│   ├── account.rs       User / NewUser 与邮箱、口令、昵称规则
│   └── code.rs          短码生成策略与合法性
├── security/        凭据原语：PBKDF2 口令散列、会话令牌生成与指纹
├── repository/      持久化端口（链接 / 用户 / 会话）+ MySQL 实现（+ 测试用内存实现）
├── cache/           缓存端口 + Redis 实现（内容缓存 / 访问计数 / 失效清理）
├── service/         用例编排：shorten / redirect / admin / history / auth / my_links
├── api/             HTTP 适配：DTO + handler，只做协议转换
│   ├── assets.rs       页面与 /assets/** 分发（路径校验与 ETag 在 assets 层）
│   ├── auth.rs         /auth/*，注册、登录、注销与当前用户
│   ├── session.rs      sl_session cookie 读写与 MaybeAuthUser 提取器
│   ├── app_config.rs   /app-config.js，下发给前端的运行时配置
│   ├── dev.rs          /__dev/*，浏览器自动刷新用，未开启时 404
│   └── health.rs       /healthz、/readyz
├── tasks/           后台任务：访问计数回刷、过期会话清理
└── test_support.rs  测试替身（仅 cargo test 编译）

web/                 前端源码。开发时逐请求读磁盘，发布时编进二进制
├── index.html       首页（创建短链）
├── login.html       登录 / 注册页
├── admin.html       管理后台（概览 / 列表 / 删除）
├── interstitial.html 中转页模板，占位符由 api::interstitial 转义后填充
└── assets/
    ├── css/         base（配色与通用控件）/ home / admin / interstitial / auth
    └── js/
        ├── core/        跨页面基础件：DOM、请求层、运行时配置、会话、格式化、吐司、剪贴板
        ├── dev/         仅开发期加载：live-reload.js
        ├── features/    业务封装：后台 API 客户端、最近创建客户端
        └── pages/       页面入口，原生 ES Modules（无构建步骤）
```

约定：

- 新增一种存储或缓存，实现 `repository::LinkRepository` / `cache::LinkCache`，在 `bootstrap.rs` 换一行装配；
- 新增一种身份来源（如 OAuth），实现 `UserRepository` 与 `SessionRepository`，复用 `AuthService` 的用例；
- 新增业务规则（如 IP 白名单）加在 `domain::AccessPolicy`，handler 与仓储都不用动；
- 接口字段调整只改 `api/dto.rs`，不波及领域模型；
- 新增路由在 `api::configure` 挂载，注意 `/{code}` 必须最后注册；
- 新增前端文件不用改任何 Rust 代码：`build.rs` 会扫描进清单，`cargo xtask dev` 下直接生效；
- 新增配置项：在 `config/base.env` 给默认值，在 `settings::Config` 加字段与校验，然后 `summary()` 里补一行。

## 配置

配置按层叠加，**后面的覆盖前面的**：

| 层 | 位置 | 典型用途 |
| --- | --- | --- |
| 1 | `config/base.env`（编译进二进制） | 保证零配置也能启动 |
| 2 | `config/{APP_ENV}.env` | 各环境差异：端口、日志、资源模式 |
| 3 | `.env` | 兼容旧习惯，存在才读 |
| 4 | `.env.local` | 个人覆盖与本地密钥，**不入库** |
| 5 | 进程环境变量 | 部署时只认这一层，优先级最高 |

`APP_ENV` 取 `development`（默认）/ `test` / `production`，决定加载哪份档。
配置目录按 `SHORTLINK_CONFIG_DIR` → `./config` → 可执行文件同级 `config/` 的顺序查找；
都找不到时只用编译进二进制的那层，所以**发布单个可执行文件也能启动**。

### 配置项

| 变量 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `APP_ENV` | 否 | `development` | 运行档：`development` / `test` / `production` |
| `DATABASE_URL` | 是 | 本地 compose 默认值 | MySQL 连接串 |
| `REDIS_URL` | 是 | 本地 compose 默认值 | Redis 连接串 |
| `SERVER_HOST` | 否 | `0.0.0.0` | 监听地址 |
| `SERVER_PORT` | 否 | `8080`（开发档 `9981`） | 监听端口 |
| `PUBLIC_BASE_URL` | 否 | `http://127.0.0.1:$SERVER_PORT` | 返回给用户的短链前缀；其路径部分会成为前端 `apiBase` |
| `DATABASE_MAX_CONNECTIONS` | 否 | `10` | 连接池上限 |
| `CACHE_LINK_TTL_SECONDS` | 否 | `3600` | 短链缓存存活时间 |
| `VISIT_FLUSH_INTERVAL_SECONDS` | 否 | `300` | 访问计数回刷间隔 |
| `SHORT_CODE_LENGTH` | 否 | `7` | 自动生成短码长度（1–10） |
| `HISTORY_RECENT_LIMIT` | 否 | `20` | 每个账号 / 浏览器保留的「最近创建」条数 |
| `SESSION_TTL_DAYS` | 否 | `30` | 登录会话有效期（天，≥ 1）。到期即失效，不滑动续期 |
| `PASSWORD_HASH_ITERATIONS` | 否 | `600000`（开发档 `100000`） | PBKDF2 迭代数。散列串自带参数，调高不影响老账号登录 |
| `ADMIN_TOKEN` | 否 | 开发档 `dev-admin-token` | 后台口令，≥ 8 字符；**不配置则后台整体关闭**（`/admin*` 一律 404） |
| `ASSETS_MODE` | 否 | `auto` | `auto` / `disk` / `embedded`；auto 在开发档取 disk、其余取 embedded |
| `ASSETS_ROOT` | 否 | `web` | disk 模式下的资源根目录 |
| `LIVE_RELOAD` | 否 | 开发档开 | 注入浏览器自动刷新脚本；资源内嵌时强制关闭 |
| `SITE_NAME` | 否 | `shortlink` | 前端展示用站点名，经 `/app-config.js` 下发 |
| `RUST_LOG` | 否 | `info`（开发档 `debug`） | 日志级别，可带模块过滤 |
| `SHORTLINK_CONFIG_DIR` | 否 | 自动探测 | 显式指定配置目录 |

缺必填项、数值越界、口令过短都会在启动时失败并指明是哪一项，不会带着坏配置跑起来。
启动日志第一行是脱敏后的配置摘要，出问题先看它。

## 本地依赖：Docker Compose

应用不进容器（保留热重启），只把 MySQL 与 Redis 交给 Docker：

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

## 账号与会话

不引入 JWT，用**不透明令牌 + 服务端会话表**：注销要能立刻生效，而 JWT 想做到这点
就得再维护一份黑名单，等于把会话表换个形式存回来。

| 关注点 | 做法 |
| --- | --- |
| 令牌生成 | 32 字节 CSPRNG，十六进制 64 位 |
| 令牌存储 | 库里只存 SHA-256 指纹；明文只在响应 cookie 里出现一次 |
| 口令散列 | PBKDF2-HMAC-SHA256，编码串 `pbkdf2-sha256$<iter>$<salt>$<hash>` 自带算法与参数 |
| 比较 | `subtle::ConstantTimeEq`，长度不等也走同一路径 |
| 防账号枚举 | 邮箱不存在时也跑一次等价代价的校验（decoy hash），错误文案统一 |
| Cookie | `sl_session`，`HttpOnly` + `SameSite=Lax` + `path=/` + 绝对过期；`PUBLIC_BASE_URL` 为 https 时加 `Secure` |
| 阻塞隔离 | 散列与校验走 `spawn_blocking`，不占 actix worker 线程 |
| 清理 | 后台任务每 6 小时删一次过期会话；`user_sessions` 外键随用户级联删除 |

会话有效期是**绝对过期**，不做滑动续期：长期开着的标签页该到期就到期，重新登录一次即可。

## 字段校验

校验拆成三层，各管各的：

| 层 | 负责 | 位置 |
| --- | --- | --- |
| 归一化 | 去首尾空格、空串归 `None`、负数直接拒绝 | `api::dto` 的 `TryFrom` |
| 合法性 | 长度、范围、字符集、时间先后 | `domain::new_link` / `domain::account` |
| 即时反馈 | 同一套规则的中文提示，把错误定位到具体字段 | `web/assets/js/pages/home.js` |

前端那层只负责「少一次往返」和「指出是哪一栏」；**后端才是权威**，
绕过页面直接调接口同样拦得住。三层顺序也刻意对齐（`url → code → expire → max_visits → password`），
一次只报一个问题，用户不用改一个报一个。

| 字段 | 规则 |
| --- | --- |
| `url` | 1–2048 字符，`http(s)://` 开头，中间不能有空白 |
| `custom_code` | 1–10 位 `[A-Za-z0-9_-]`，去首尾空格后为空则自动生成 |
| `expire_at` | 必须晚于当前时间（等于此刻也算过期，与跳转判定一致） |
| `max_visits` | 1–1 亿的整数；`0` 与负数都拒（`0` 等于建出来就 410） |
| `password` | 1–64 位；不含空白/引号/反引号/反斜杠/分号/注释符；不含 SQL 关键字（整词） |
| `ip_limit` / `ua_limit` | JSON 对象或数组，序列化后 ≤ 4096 字节（内容暂无语义） |
| 昵称 | ≤ 32 字符，不含控制字符，全空回退到邮箱前缀 |
| 邮箱 | 保守语法校验 + 归一化为小写 |

> 「密码不能含 SQL 关键字」**不是注入防线** —— 存取全部走参数化查询。
> 它挡的是把一段 SQL 当密码粘进来这类异常输入，匹配按整词，`candy` / `order` 不会误伤。

## 测试

```bash
cargo test          # 全部用例离线跑，不需要 MySQL / Redis
cargo test domain:: # 只跑领域层
```

分层策略、测试替身与未覆盖范围见 [docs/TESTING.md](docs/TESTING.md)。

## 开发

```bash
cargo xtask dev            # 构建 + 启动 + 监听（默认先拉起 MySQL / Redis）
cargo xtask dev --port 9982 --no-infra
cargo xtask check          # fmt --check + clippy -D warnings + test
cargo xtask fmt            # 格式化
cargo xtask build          # 只编译
cargo xtask db up|down|ps|logs
```

`dev` 的监听范围是 `src/`、`config/`、`Cargo.toml`、`build.rs` 与 `xtask/`：文件一变就重建，
构建失败时保留旧进程并等你修完（不会把服务打死）。前端不在监听范围内 —— 开发档下资源是
逐请求读磁盘的，改完刷新即可，页面还会自己检测版本变化并重载。

Rust 是静态编译语言，后端没有真正意义上的热更新，`cargo xtask dev` 做的是把
「保存 → 重编译 → 重启」自动化；前端的「热」则是真热：不重启进程、不重新编译。

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | 开发环境：目录约定、配置分层、热更新原理、xtask 命令、排错 |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 分层与依赖方向、调用链、关键决策与权衡、扩展指引 |
| [docs/API.md](docs/API.md) | 接口字段、响应示例、错误文案、状态码总表 |
| [docs/TESTING.md](docs/TESTING.md) | 三层测试策略、用例分布、测试替身、手工冒烟脚本 |

## 已知限制

- **访问密码是明文比对**，未做哈希（区别于账号口令）；要统一需迁移存量链接。
- **`ip_limit` / `ua_limit` 只落库，不参与判定**。当前唯一影响是带这两个字段的链接不进缓存。
- 创建短链的公开接口不要求登录：任何人都能建链，只是登录后才归属到账号。后台接口由
  `ADMIN_TOKEN` 保护，但令牌经请求头明文传输，仅在 HTTPS 或内网环境使用。
- 没有邮箱验证、找回密码与改密码入口：注册时填什么就是什么，忘记口令只能由管理员在库里处理。
- 没有按账号的配额与频率限制，注册接口可被批量调用。
- 匿名链接（未登录时创建）任何人都不能凭账号认领，包括后来的自己。
- 后台概览里的「总访问」只含已回刷到库的计数，最近几分钟的增量不计入。
- 没有编辑、批量操作与访问日志；未登录时界面的「最近创建」按匿名 cookie 记在服务端，
  换设备或清除 cookie 后不可见。
- 建表脚本文件名 `talbe.sql` 是 `table.sql` 的笔误，暂未重命名；表结构变更靠 `migrations/` 下手工维护的增量 SQL，没有自动迁移机制。
- 前端没有打包步骤：ES Modules 直接由浏览器加载，文件多时会有一串请求。要上 HTTP/2 或做体积优化，得引入真正的构建链。
