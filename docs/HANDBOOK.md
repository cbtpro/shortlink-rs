# 开发手册

从一台空机器到「跑起来、改得动、查得到」，按顺序做完下面这些就够了。

这份文档讲**怎么做**。背后的取舍见 [ARCHITECTURE.md](ARCHITECTURE.md)，
热更新与配置分层的原理见 [DEVELOPMENT.md](DEVELOPMENT.md)，接口字段见 [API.md](API.md)，
测试策略见 [TESTING.md](TESTING.md)。

---

## 0. 先看这张清单

| 步骤 | 命令 | 成功标志 |
| --- | --- | --- |
| 1. 装工具链 | `rustc --version`、`docker compose version` | 两条都有输出 |
| 2. 拿代码 | `git clone …` | `ls config/` 有 4 个 `.env` |
| 3. 起中间件 | `cargo xtask db up` | `docker compose ps` 两个 `healthy` |
| 4. 启动应用 | `cargo xtask dev` | 日志出现 `profile=development addr=0.0.0.0:9981` |
| 5. 验收 | `curl /healthz`、`curl /readyz` | `200 ok` / `200 ready` |
| 6. 开页面 | 浏览器打开 `http://127.0.0.1:9981/` | 能注册、能建链、能跳转 |

**不需要**任何配置文件就能跑：`config/base.env` 与 `config/development.env` 已经给了可用默认值，
并且会被编进二进制。要改的是个别项，见 [配置](#4-配置)。

---

## 1. 环境准备

### 1.1 必需

| 组件 | 版本要求 | 用途 | 安装 |
| --- | --- | --- | --- |
| Rust | stable（用 rustup 装） | 编译应用与 `xtask` | <https://rustup.rs> |
| Docker + Compose v2 | Docker 20+ | 只跑 MySQL 与 Redis | Docker Desktop / `docker-ce` |
| git | 任意 | 取代码 | 系统包管理器 |

`cargo xtask` 是仓库自带的构建工具（一个 Rust crate），不依赖 `make`、`bash` 或 Node，
Windows 也能直接跑。

> **不要用发行版仓库里的 `rustc`**。`sqlx 0.8` 与 `actix-web 4` 需要较新的编译器，
> Ubuntu/Debian 自带的版本通常太旧，报错会是一堆看不懂的 trait 不匹配。
> 统一用 rustup：`rustup update stable`。

**检查**：

```bash
rustc --version      # 期望 1.80 以上
cargo --version
docker --version
docker compose version   # 必须是 v2（docker-compose 老版本不支持 --wait）
git --version
```

`cargo xtask db up` 内部执行 `docker compose up -d --wait`；离线的旧版 Compose 不支持
`--wait` 时它会降级成后台启动，并提示你稍后用 `docker compose ps` 确认健康状态。
`xtask` 只认 `docker compose`（v2 插件），只装了 v1 的 `docker-compose` 时它会提示跳过中间件启动
—— 那种情况请手动 `docker compose up -d` 或升级 Compose。

### 1.2 可选

| 工具 | 用途 |
| --- | --- |
| `curl` | 冒烟、抓接口；Windows 10+ 自带 |
| MySQL 客户端（`mysql` / DBeaver / TablePlus） | 直接看表 |
| `redis-cli` | 看缓存与访问计数（容器里也有） |
| Node 18+ | 只用于临时的浏览器排查脚本，**运行时不依赖** |

### 1.3 平台差异

**Windows（Git Bash）**

- 本手册的命令都是 POSIX 语法，用 Git Bash 或 WSL 执行；
- 如果系统装过代理，`curl` 访问 `127.0.0.1` 会走代理导致连不上，加 `--noproxy '*'`；
- Docker Desktop 要先启动，托盘图标变绿再执行 `docker compose`；
- 不要用记事本把 `web/` 下的文件存成 GBK，页面会挂；保存为 UTF-8。

**macOS / Linux**

- Docker 需要当前用户在 `docker` 组里，否则每条命令都要 `sudo`（不推荐）；
- macOS 上用 Docker Desktop 或 colima，`docker compose` 可用即可。

---

## 2. 首次启动

### 2.1 取代码

```bash
git clone git@github.com:cbtpro/shortlink-rs.git
cd shortlink-rs
ls config/            # base.env  development.env  test.env  production.env
```

`config/` 必须在仓库根目录：配置查找顺序是 `SHORTLINK_CONFIG_DIR` → `./config` →
可执行文件同级的 `config/`，找不到时只用编译进二进制的那层。

### 2.2 起中间件

```bash
cargo xtask db up
```

等价于 `docker compose up -d --wait`。它会把 MySQL 与 Redis 拉到 `healthy` 才返回。

首次启动时 MySQL 会自动执行 `talbe.sql` 建表 —— **只在数据卷为空时执行一次**。
之后改 schema 要么手动跑 `migrations/`，要么删卷重建（见 [数据库](#5-数据库)）。

**成功标志**：

```bash
docker compose ps
# shortlink-mysql   ... (healthy)
# shortlink-redis   ... (healthy)
```

两个都是 `healthy` 才算就绪。`starting` 就再等十几秒，MySQL 首次初始化要跑建表脚本。

不习惯用 xtask 也可以直接 `docker compose up -d`，两者等价。

### 2.3 启动应用

```bash
cargo xtask dev
```

它做四件事：起中间件（失败只警告，库可能在远端）→ `cargo build` → 启动进程（日志打到当前终端）
→ 进入监听循环。**首次编译要几分钟**，之后增量编译在几秒内。

不想让它管中间件：`cargo xtask dev --no-infra`。想换端口：`cargo xtask dev --port 9982`
（只改监听端口，生成的短链前缀仍取 `PUBLIC_BASE_URL`；长期用这个端口就把
`SERVER_PORT` 与 `PUBLIC_BASE_URL` 一起写进 `.env.local`）。

**成功标志**（启动日志第一行是脱敏后的配置摘要）：

```
profile=development addr=0.0.0.0:9981 public=http://127.0.0.1:9981 db=mysql://***@127.0.0.1:3306/short_link assets=disk live_reload=on admin=admin@shortlink.local password_file=data/admin-initial-password.txt session_ttl=30d password_iterations=100000

  首页    http://127.0.0.1:9981/
  后台    http://127.0.0.1:9981/admin
  健康    http://127.0.0.1:9981/healthz
```

看到 `profile=` 这一行就说明配置读到了、依赖连上了。这一行是排查问题的第一现场，
每个字段的含义见 [配置摘要怎么读](#44-启动摘要怎么读)。

### 2.4 验收

三条命令，覆盖「进程活着 / 依赖通 / 业务能用」：

```bash
# 1) 存活：不碰依赖，进程在就是 200
curl -s http://127.0.0.1:9981/healthz
# {"status":"ok","profile":"development","assets":"disk","stamp":"..."}

# 2) 就绪：探测 MySQL 与 Redis
curl -s http://127.0.0.1:9981/readyz
# {"status":"ready"}          失败时：{"status":"degraded","failed":["mysql"]}

# 3) 建一条短链并跳转
curl -s -X POST http://127.0.0.1:9981/shorten \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","custom_code":"demo"}'
curl -s -o /dev/null -w '%{http_code} %{redirect_url}\n' http://127.0.0.1:9981/demo
# 302 https://example.com/
```

Windows 上加 `--noproxy '*'`。

再走一遍带账号的完整链路（cookie 存在 `cookies.txt`）：

```bash
# 注册，成功后直接登录，响应里 Set-Cookie: sl_session=...
curl -s -i -c cookies.txt -X POST http://127.0.0.1:9981/auth/register \
  -H 'Content-Type: application/json' \
  -d '{"email":"dev@example.com","password":"secret123","display_name":"开发"}'

# 当前用户
curl -s -b cookies.txt http://127.0.0.1:9981/auth/me
# {"user":{"id":1,"email":"dev@example.com","display_name":"开发","created_at":"..."}}

# 带身份建链，这条短链归属于这个账号
curl -s -b cookies.txt -X POST http://127.0.0.1:9981/shorten \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com/mine","custom_code":"mine"}'

# 我的链接
curl -s -b cookies.txt http://127.0.0.1:9981/recent
```

浏览器打开 `http://127.0.0.1:9981/`，页首应显示昵称与「退出」，列表标题是「我的链接」。
这些都对，环境就算配好了。

---

## 3. 目录速览

改代码前先认门，完整分层见 [项目结构](../README.md#项目结构)。

```
config/          分层配置，改默认值从 base.env 开始
src/
  bootstrap.rs   组合根：所有依赖在这里装配，换实现只改这一处
  domain/        规则：短链、账号、短码。不依赖 web / redis / sqlx
  repository/    持久化端口 + MySQL 实现（+ 测试用内存实现）
  cache/         缓存端口 + Redis 实现
  service/       用例编排，业务顺序都在这里
  api/           HTTP 适配：DTO + handler，只做协议转换
  settings/      配置合并与校验
  assets/        前端资源的 disk / embedded 两种来源
web/             前端源码，零构建
web-test/        前端单元测试（node --test + jsdom），不编进二进制
xtask/           构建工具：dev / build / release / check / web-test / fmt / db
migrations/      存量库的增量 SQL，按文件名顺序执行
talbe.sql        全量建表（新库用；文件名是 table 的笔误，暂未重命名）
```

依赖方向单向向内：`api → service → domain`，`repository` / `cache` 实现内层定义的端口。
改 handler 时不要顺手写业务规则，规则属于 `domain`。

---

## 4. 配置

### 4.1 什么时候需要改

| 场景 | 改哪里 |
| --- | --- |
| 换端口、换数据库、改管理员邮箱 | `.env.local` |
| 团队共享的开发默认值 | `config/development.env` |
| 所有环境都要的默认值 | `config/base.env` |
| 生产部署 | 进程环境变量（只认这一层） |

多数时候**什么都不用改**。仓库自带的开发档已经能跑。

### 4.2 五层，后面的盖前面的

| 层 | 位置 | 是否入库 | 用途 |
| --- | --- | --- | --- |
| 1 | `config/base.env` | 是（编进二进制） | 零配置可启动的默认值 |
| 2 | `config/{APP_ENV}.env` | 是 | 环境差异：端口、日志、资源模式 |
| 3 | `.env` | 否 | 兼容旧习惯，存在才读 |
| 4 | `.env.local` | 否 | 个人覆盖与本地密钥 |
| 5 | 进程环境变量 | — | 部署时最高优先级 |

`APP_ENV` 取 `development`（默认）/ `test` / `production`。

**动手改之前先看现成的值**：

```bash
cat config/base.env
cat config/development.env
```

### 4.3 常见改法

**换端口**

```bash
cp .env.example .env.local
# 写入：
# SERVER_PORT=9982
# PUBLIC_BASE_URL=http://127.0.0.1:9982
```

`PUBLIC_BASE_URL` 决定返回给用户的短链前缀，端口变了它也要跟着变，否则生成的短链指向旧端口。

**连远端数据库**

```bash
# .env.local
DATABASE_URL=mysql://user:pass@10.0.0.5:3306/short_link
REDIS_URL=redis://10.0.0.5:6379/0
```

远端库要自己先建表：跑一遍 `talbe.sql`，再按顺序跑 `migrations/`。

**第一次进后台**

后台没有单独的访问口令，用的是登录账号。首次启动时若库里没有管理员，服务会用
`ADMIN_EMAIL`（默认 `admin@shortlink.local`）建一个，并把随机口令写进
`data/admin-initial-password.txt`：

```bash
cat data/admin-initial-password.txt
# email=admin@shortlink.local
# password=RLk6Lme3jutsKKavuXeh
```

拿它登录 `/login`（登录后会跳到后台），页面会先要求改密码 —— 改完这个文件立即删除，
旧口令同时作废。生产环境把这个文件放到数据盘上更好：

```bash
ADMIN_PASSWORD_FILE=/var/lib/shortlink/admin-initial-password.txt
ADMIN_EMAIL=admin@short.example.com
```

**忘了管理员口令怎么办**：库里已经有管理员，重启不会再生成新口令。两种救法：

```sql
-- 1) 把管理员角色交给一个你还能登录的账号（没有「提升为管理员」的界面入口）
UPDATE users SET role = 'admin' WHERE email = 'me@example.com';

-- 2) 彻底重来：删掉管理员那行，重启服务，会重新生成一次性口令
DELETE FROM users WHERE email = 'admin@shortlink.local';
```

删之前记得先处理挂在它名下的短链（`short_links.user_id` 是级联删除）。

**反代到子路径**（例如 `https://example.com/s/`）

```bash
PUBLIC_BASE_URL=https://example.com/s
```

短链前缀与前端 `apiBase` 都取自它的路径部分。反代侧记得把 `/s` 前缀去掉再转发给应用。

**口令散列迭代数**

生产默认 `PASSWORD_HASH_ITERATIONS=600000`（OWASP 建议），开发档压到 `100000`，
本地一次登录几十毫秒。散列串自带迭代数，调高不会让老账号登不上，见
[DEVELOPMENT.md](DEVELOPMENT.md#为什么开发档的迭代数是-10-万)。

**日志**

```bash
RUST_LOG=debug                    # 全量
RUST_LOG=info,sqlx=warn           # 压掉 sqlx 的刷屏
```

**完整配置项**见 [README 的配置章](../README.md#配置)，共 20 余项；
启动时会逐项校验，缺必填、数值越界、口令过短都直接失败并指明是哪一项。

### 4.4 启动摘要怎么读

```
profile=development addr=0.0.0.0:9981 public=http://127.0.0.1:9981
db=mysql://***@127.0.0.1:3306/short_link assets=disk live_reload=on
admin=admin@shortlink.local password_file=data/admin-initial-password.txt
session_ttl=30d password_iterations=100000
```

| 字段 | 含义 | 不对时会怎样 |
| --- | --- | --- |
| `profile` | 加载了哪份环境档 | 不是 `development` 说明 `APP_ENV` 被覆盖了 |
| `addr` | 实际监听地址 | 和预期不符查 `SERVER_HOST` / `SERVER_PORT` |
| `public` | 生成的短链前缀 | 与浏览器地址不一致时，复制的短链打不开 |
| `db` | 连接串（凭据已隐去） | 库名或主机错了，启动或首次查询就失败 |
| `assets` | `disk` 逐请求读盘 / `embedded` 编译期内嵌 | 开发档应为 `disk`，否则改前端不生效 |
| `live_reload` | 浏览器自动刷新 | `off` 时改前端要手动刷新 |
| `admin` | 初始化管理员邮箱 | 与登录时用的邮箱不一致，说明你以为的账号是另一个 |
| `password_file` | 一次性口令文件路径 | 文件不存在通常表示口令已经改过（改完即删） |
| `session_ttl` | 登录有效期 | 改了要重启生效 |
| `password_iterations` | PBKDF2 迭代数 | 过大表现为登录慢，见排错表 |

想知道某个值是哪一层给的：`RUST_LOG=debug cargo xtask dev`，会打出每层配置文件的实际路径与命中情况。

### 4.5 纪律

- `.env` 与 `.env.local` **不入库**（`.gitignore` 已覆盖），模板是 `.env.example`；
- `config/` 下的文件会**编进二进制**，别往里写真实密码；
- 新增配置项三步：`config/base.env` 给默认值 → `settings::Config` 加字段与校验 →
  `summary()` 补一行。漏了第三步，出问题时日志里看不到它。

---

## 5. 数据库

### 5.1 新库

不用管。`docker compose up` 首次启动时 MySQL 会把 `talbe.sql` 挂进
`/docker-entrypoint-initdb.d/10-schema.sql` 并执行一次。

### 5.2 已有的库（升级）

`talbe.sql` 只对新库有效，存量库要按文件名顺序执行 `migrations/` 下的增量 SQL：

```bash
ls migrations/          # 2026-11-01-add-interstitial.sql  2026-11-02-add-users.sql

docker compose exec -T mysql mysql -uroot -proot short_link \
  < migrations/2026-11-01-add-interstitial.sql

docker compose exec -T mysql mysql -uroot -proot short_link \
  < migrations/2026-11-02-add-users.sql
```

- `-T` 关掉伪终端，否则重定向会报 "the input device is not a TTY"；
- 库名与账号密码要和 `docker-compose.yml` 里的 `MYSQL_DATABASE` / `MYSQL_USER` 一致；
- 这些脚本没有版本记录表，**执行过哪些要自己记**（或直接比对表结构）。

### 5.3 核对

```bash
docker compose exec -T mysql mysql -uroot -proot short_link -e 'SHOW TABLES;'
# short_links
# link_creations   匿名访客的创建记录
# users
# user_sessions

docker compose exec -T mysql mysql -uroot -proot short_link -e 'DESCRIBE short_links;'
# 应有 user_id 列（bigint unsigned NULL，带索引）
```

### 5.4 重置

```bash
cargo xtask db reset --yes     # 删卷重建，数据全丢；--yes 不能省
cargo xtask db down            # 只停容器，数据保留
```

`docker compose down -v` 效果一样。重置后首次启动会重新执行 `talbe.sql`。

### 5.5 时区

**接口用本地时间，存储用 UTC。** 直接用 SQL 客户端查库，看到的值比本地时间早 8 小时，
这是正常的。换算只发生在 `src/api/dto.rs`，领域层与存取层不做时区处理。

---

## 6. 日常开发

### 6.1 改了什么，怎么生效

| 改动位置 | 生效方式 | 耗时 |
| --- | --- | --- |
| `src/`、`config/`、`Cargo.toml`、`build.rs`、`xtask/` | 自动重新编译 → 重启进程 | 几秒（增量） |
| `web/` 下的 HTML / CSS / JS | 不重启不编译，浏览器自己刷新 | ≤ 800ms |
| `talbe.sql`、`migrations/` | **不会自动生效**，要手动执行 | — |

编译失败时旧进程继续服务，修完保存它自己接着来 —— 服务不会被打死。

### 6.2 加一个页面

1. 在 `web/` 放 HTML，`web/assets/` 放配套 CSS/JS；
2. 需要路由的话在 `src/api/assets.rs` 加一个 handler（照 `login_page` 写），
   并在 `src/api/mod.rs` 的 `configure` 里注册；
3. **不用改构建脚本**：`build.rs` 扫描 `web/` 生成清单，新文件自动进包；
4. 服务端渲染的模板页（如 `notice.html`）要同步加进 `src/assets/mod.rs`
   的必检清单 —— 那份清单决定「磁盘模式下缺文件要不要拒绝启动」。

注意 `/{code}` 是兜底路由，必须最后注册，否则会把页面路径当成短码。

### 6.3 加一个接口

```
api/dto.rs        请求 / 响应结构与归一化（trim、类型转换）
  ↓
service/          用例编排：顺序、降级、缓存失效
  ↓
domain/           规则：什么算合法、什么算过期
  ↓
repository/       端口 + MySQL / 内存两种实现
```

四步走完再在 `api/mod.rs` 的 `configure` 挂载。新增的错误变体加到 `error.rs`，
那里是状态码的唯一映射点；别在 handler 里手写 `HttpResponse::BadRequest`。

### 6.4 加一张表

1. 改 `talbe.sql`（新库用）；
2. 在 `migrations/` 加一份 `YYYY-MM-DD-<说明>.sql`（存量库用），只写增量；
3. `repository` 的端口与两种实现都要跟上（内存实现供测试用）。

### 6.5 提交前

```bash
cargo xtask fmt      # 格式化
cargo xtask check    # fmt --check + clippy -D warnings + test + web-test
```

`check` 全绿才提交。Rust 测试不需要 MySQL / Redis，全部用内存替身，离线可跑：

```bash
cargo test              # 全部
cargo test domain::     # 只跑领域层
```

前端那一段（`web-test/`，node --test + jsdom）需要先装一次依赖，
之后 `check` 会自动带上；没装时只跳过并提示，不会误报成功：

```bash
cd web-test && npm install     # 一次就够
cargo xtask web-test           # 缺依赖会直接失败，适合确认「真跑过了」
```

---

## 7. 调试

### 7.1 三个健康端点

| 端点 | 语义 | 什么时候用 |
| --- | --- | --- |
| `/healthz` | 进程活着就算 200，不碰依赖 | 判断「服务有没有起来」 |
| `/readyz` | 探测 MySQL 与 Redis，失败返回 503 | 判断「依赖通不通」，响应体的 `failed` 指明是谁 |
| `/__dev/revision` | 资源版本号，仅开发档 | 判断「前端文件改动有没有被看到」；生产返回 404 |

### 7.2 直接看依赖

```bash
# 库里到底有什么
docker compose exec -T mysql mysql -uroot -proot short_link \
  -e 'SELECT code, user_id, visit_count, expire_at FROM short_links ORDER BY id DESC LIMIT 10;'

# 缓存与访问计数（内容缓存 short:v4:code:<短码>，计数 short:v3:visits:<短码>）
docker compose exec -T redis redis-cli keys 'short:*'
docker compose exec -T redis redis-cli get 'short:v3:visits:demo'
# 中转页「继续访问」的票据，10 分钟自动过期，一般不用管
docker compose exec -T redis redis-cli keys 'short:gate:*'
```

访问计数**先写 Redis、后台每 60 秒回刷**（`VISIT_FLUSH_INTERVAL_SECONDS`），
进程启动时还会先补刷一次。所以库里刚建链时 `visit_count` 是 0，下一秒就有了。
后台列表显示的是「库里的值 + Redis 里未回刷的增量」，点完链接回去看就是最新数字；
概览的「总访问」来自库里的汇总，最多落后一个回刷周期。

**开了中转页的链接**：打开确认页不计数，点「继续访问」才计一次 —— 想看效果就
盯 `short:gate:*`，点一下少一条，`short:v3:visits:<短码>` 加一。

### 7.3 前端

- 页面与 `/assets/**` 都带 ETag，改完刷新就拿到新的，不需要清缓存；
- HTML 是 `no-store`，不会被缓存成旧版本；
- 改 CSS/JS 后页面会自己重载（轮询 `/__dev/revision`），控制台能看到 `[live-reload]` 字样；
- 浏览器控制台报 404 的模块，检查文件名大小写 —— Linux 上区分大小写，Windows 不区分，
  本地能跑、部署挂掉的常见原因就是这个。

### 7.4 抓一次完整会话

```bash
BASE=http://127.0.0.1:9981
JAR=$(mktemp)

curl -s -c "$JAR" -X POST $BASE/auth/register -H 'Content-Type: application/json' \
  -d '{"email":"t1@example.com","password":"secret123"}'            # 201
curl -s -b "$JAR" $BASE/auth/me                                     # {"user":{...}}
curl -s -b "$JAR" -X POST $BASE/shorten -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com/x","custom_code":"tx1"}'          # 201
curl -s -o /dev/null -w '%{http_code}\n' $BASE/tx1                  # 302
curl -s -b "$JAR" -X DELETE $BASE/recent/tx1                        # 204，短链随之失效
curl -s -o /dev/null -w '%{http_code}\n' $BASE/tx1                  # 404
curl -s -b "$JAR" -X POST $BASE/auth/logout                         # 204
```

Windows 上给每条 `curl` 加 `--noproxy '*'`。

---

## 8. 排错

| 现象 | 原因与处理 |
| --- | --- |
| `failed to connect to the docker API … check if the daemon is running` | Docker Desktop / `dockerd` 没启动。Windows 上先开 Docker Desktop，等托盘图标变绿 |
| `9981 is already in use` | 上个实例没退干净：`netstat -ano \| findstr :9981` 找 PID 结束，或 `--port` 换端口 |
| 启动即报缺 `DATABASE_URL` | 配置层没读到：确认 `APP_ENV`、`config/` 在仓库根目录，或 `SHORTLINK_CONFIG_DIR` 指过去 |
| `readyz` 返回 503 | 看 `failed` 字段是 `mysql` 还是 `redis`；`docker compose ps` 是否 `healthy` |
| MySQL 容器一直 `starting` | 首次初始化在跑 `talbe.sql`，等 30–60 秒；仍不行看 `docker compose logs mysql` |
| `Access denied for user` | `.env.local` 的账号密码与 compose 里的 `MYSQL_USER` / `MYSQL_PASSWORD` 不一致 |
| 建表脚本没执行 | 数据卷非空时不会执行。`cargo xtask db reset --yes` 后重来（数据会丢） |
| 迁移后仍缺列 | 只跑了 `talbe.sql` 没跑 `migrations/`；存量库要手动执行增量脚本 |
| 改了前端没反应 | 确认 `assets=disk`、`live_reload=on`（看启动摘要），以及 `/__dev/revision` 是否 200 |
| 改了 Rust 没反应 | 看 `cargo xtask dev` 输出，多半是编译失败了，旧进程还在服务 |
| 后台一直跳登录页 | 没有 `sl_session` cookie：没登录、被停用，或在别的设备上改过密码 |
| 登录后只看到「先改一下密码」 | 用的是临时口令，改完才能用其他接口 |
| `data/admin-initial-password.txt` 不见了 | 管理员已经改过密码，文件按设计删除；忘了口令就走上面的 SQL 救法 |
| 登录慢到一秒以上 | 看启动摘要的 `password_iterations`；确认 `Cargo.toml` 里 `[profile.dev]` 与 `[profile.dev.package."*"]` 还在 |
| 登录后刷新就掉线 | cookie 被清，或 `PUBLIC_BASE_URL` 配成 `https://` 却用 http 访问（`Secure` 属性导致不下发） |
| `/auth/me` 一直是 `null` | 请求没带 cookie：跨站请求被 `SameSite` 拦了，或会话被后台任务清掉 |
| curl 连不上 127.0.0.1 | 系统代理拦截，加 `--noproxy '*'` |
| 页面样式全乱 | 前端文件被存成了 GBK 或 CRLF 混合，保存为 UTF-8 |

---

## 9. 提交与协作

```bash
cargo xtask check          # 必须绿
git add -A
git commit -m 'fix: 修正过期时间的边界判定'
git push origin <branch>
```

- **提交信息**用 `<类型>: <中文说明>`，类型沿用 `feat` / `fix` / `refactor` / `docs` / `perf` / `test` / `chore`；
- **一次提交一个主题**。跨主题的大改会让 review 和回滚都变难；
- 仓库**没有 CI**，`cargo xtask check` 是唯一闸门，本地跑过再推；
- 改了发布产物相关的（`build.rs`、`assets`、`config/`）先 `cargo xtask release` 验一遍：
  产物在 `dist/`，里面有可执行文件、`config/` 与 `.env.example`，整个目录拷到服务器就能跑。

---

## 10. 清理

```bash
# 停应用：当前终端 Ctrl+C（xtask 会一起收尾）

cargo xtask db down      # 停容器，数据保留在卷里
cargo xtask db reset --yes   # 停容器 + 删卷 + 重建，数据清空
cargo clean              # 清构建产物（重新编译要几分钟）
```

只想停 Redis/MySQL 其中一个：`docker compose stop redis`。

---

## 附：常见任务命令速查

| 想做的事 | 命令 |
| --- | --- |
| 一条命令启动全部 | `cargo xtask dev` |
| 只跑应用，不管中间件 | `cargo xtask dev --no-infra` |
| 换端口 | `cargo xtask dev --port 9982` |
| 只编译 | `cargo xtask build` |
| 提交前检查 | `cargo xtask check` |
| 格式化 | `cargo xtask fmt` |
| 打发布包到 `dist/` | `cargo xtask release` |
| 起/停/看中间件 | `cargo xtask db up\|down\|ps\|logs` |
| 重置数据库 | `cargo xtask db reset --yes` |
| 跑测试 | `cargo test`（后端）/ `cargo xtask web-test`（前端） |
| 看生效的配置 | 启动日志第一行，或 `RUST_LOG=debug cargo xtask dev` |

---

第一次配环境卡住了？先看启动日志第一行的摘要，再看 `/readyz` 的 `failed` 字段 ——
这两处能定位绝大多数问题。
