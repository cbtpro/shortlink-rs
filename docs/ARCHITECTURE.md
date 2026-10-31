# 架构说明

## 分层与依赖方向

```
main.rs          组合根：读配置、装配实现、启动服务与后台任务
  │
  ├── api/         HTTP 适配层：DTO + handler，只做协议转换
  │   ├── assets      内嵌前端资源（include_str!，GET / 与 GET /admin）
  │   ├── history     首页「最近创建」（GET/DELETE /recent*）
  │   ├── visitor     匿名访客 cookie 的签发与校验
  │   ├── interstitial 中转页渲染：模板占位符单遍填充 + HTML 转义
  │   └── admin       后台 JSON 接口（概览 / 列表 / 删除）
  ├── service/     用例层：shorten（创建）/ redirect（跳转）/ admin（后台管理）/ history（最近创建）
  ├── domain/      领域层：实体、校验规则、访问策略、短码规则、链接状态
  ├── repository/  持久化端口（LinkRepository + CreationHistory）+ MySQL 实现 + 内存实现（测试用）
  ├── cache/       缓存端口 + Redis 实现（内容缓存 / 访问计数 / 失效清理）
  └── tasks/       后台任务：访问计数回刷

web/             前端源码（原生 ES Modules，无框架、无构建），经 include_str! 进入二进制
                 assets/css/base.css 与 assets/js/core/ 为两页共用，页面入口在 assets/js/pages/
```

依赖只向内：`api → service → domain`。`service` 依赖的是 `repository` / `cache` 里的 **trait**，
具体实现（MySQL、Redis）只在 `main.rs` 里出现一次。

| 层 | 该做 | 不该做 |
| --- | --- | --- |
| `api` | 解析参数、调用例、序列化响应 | 写业务判断、直连数据库 |
| `service` | 编排用例、决定降级策略 | 拼 SQL、拼 Redis 命令 |
| `domain` | 规则与实体（纯函数为主） | 依赖 web / sqlx / redis |
| `repository` / `cache` | 实现端口，处理存储错误 | 决定业务语义（如「冲突是否重试」） |
| `tasks` | 定时调度与批量写 | 承载跳转路径逻辑 |

`domain` 里唯一的框架痕迹是 `sqlx::FromRow` 派生（省去手写 10 个字段的映射），
它不引入运行期依赖。

## 一次跳转的调用链

```
GET /{code}
  └─ api::redirect::redirect                      解析 path / query
      └─ RedirectService::resolve ──▶ Landing     用例，Direct / Interstitial
          ├─ load()                               缓存优先
          │   ├─ LinkCache::get  ──命中──▶ 用 CachedLink 构造 AccessPolicy
          │   └─ 未命中 / 缓存故障 ─▶ LinkRepository::find_by_code
          │        └─ 可缓存的链接回填 LinkCache::put
          ├─ AccessPolicy::check                  过期 → 密码 → 次数上限
          └─ VisitCounter::increment              只写 Redis，不碰数据库

Direct     ─▶ 302 + Location
Interstitial ─▶ api::interstitial::render ─▶ 200 中转确认页（no-store）
```

三个降级点：缓存读失败、缓存写失败、计数写失败都只打 `warn!`，跳转照常返回。
理由是数据库才是事实来源，Redis 挂了不应该让整个服务不可用。

## 一次创建的调用链

```
POST /shorten
  └─ CreateLinkRequest → NewShortLink              边界：trim url、本地时间转 UTC
      └─ ShortenService::create
          ├─ NewShortLink::validate                url → code → password，一次报第一个错
          ├─ 指定了短码 ─▶ LinkRepository::insert  冲突直接 409
          └─ 未指定   ─▶ 生成短码，最多重试 5 次   全部撞码才返回 500
```

## 关键决策

| 决策 | 原因 | 代价 |
| --- | --- | --- |
| 异步 trait 方法手写 `BoxFuture` | `config.toml` 里 `net.offline = true`，加 `async-trait` 会断构建 | 签名比 `async fn` 啰嗦 |
| 用 `query_as` + `FromRow`，不用 `query!` 宏 | 构建不依赖活的 `DATABASE_URL` 或 `.sqlx` 目录 | 失去编译期 SQL 校验 |
| 冲突交给唯一键，不做「先查后插」 | 消除 `SELECT COUNT(*)` 与 `INSERT` 之间的 TOCTOU | 需要把数据库错误码映射成 `RepoError::DuplicateCode` |
| 访问计数只写 Redis，后台增量回刷 | 跳转路径不写库，高频访问不锁行 | at-least-once，崩溃时可能多算几次 |
| 缓存只存 `CachedLink` 投影 | 凭据不进 Redis，日志也不会打印整条记录 | 多一个结构体要维护 |
| 中转页由服务端渲染，不含脚本，占位符单遍替换并转义 | 目标地址来自数据库，必须按不可信输入处理；单遍替换避免「值里含占位符被再展开」 | 模板与渲染逻辑分居 `web/` 与 `api::interstitial` |
| 「给不给中转页」由 `RedirectService` 返回 `Landing` 决定 | 中转是一条业务规则，不是 handler 里的 if；接口层只做表示 | resolve 的返回类型从 `String` 变成枚举 |
| 带密码 / 限制 / 次数上限的链接不缓存 | 缓存里的旧计数会让上限判定失真 | 这类链接每次都要回源 |
| 时间一律存 UTC，只在 API 边界换算 | 不受会话时区影响，跨部署一致 | 直接查库看到的时间比本地早 8 小时 |
| 错误到状态码的映射集中在 `AppError` | 改状态码只改一处，handler 不用 `match` | 新增错误变体要同步两处 |
| key 前缀带版本号（内容缓存 `short:v4:`） | 缓存结构变更时旧数据自然过期，无需迁移 | 改结构要记得递增版本；计数 key 保持 `v3` 不动，升了会丢未回刷的增量 |
| 后台未启用（无 `ADMIN_TOKEN`）时 `/admin*` 一律 404 | 连「存在后台入口」都不暴露，而不是暴露一个登录页 | 页面无法区分「未启用」与「口令错误」，用固定文案提示 |
| 删除先删库、再清缓存 | 反过来的话，两步之间进来的请求会把记录重新写回缓存，已删链接还能跳到 TTL 结束 | 清缓存失败只记 warn：数据库已是事实来源，跳转会 404 |
| `ShortLink::state()` 与 `AccessPolicy::check` 判定规则一致，并有测试锁住 | 后台列表展示的状态和真实跳转结果不能互相矛盾 | 改过期/上限规则要同时改两处（测试会提醒） |
| 概览的时间窗口由调用方算好传入，仓储不取时钟 | 同一次请求里的几个统计基于同一时刻，测试也能固定时间 | 调用方多写几行窗口计算 |

## 扩展指引

| 想做的事 | 改动位置 |
| --- | --- |
| 换存储（Postgres / 内存 / 远程 API） | 实现 `repository::LinkRepository`，在 `main.rs` 换一行 |
| 换缓存（Memcached / 本地 LRU） | 实现 `cache::LinkCache` + `cache::VisitCounter` |
| 新增访问规则（IP 白名单、UA 黑名单） | 加在 `domain::AccessPolicy::check`，其余各层不动 |
| 新增接口字段 | 只改 `api/dto.rs` |
| 换短码算法（号段 / 雪花） | 实现 `domain::code::ShortCodeGenerator` |
| 新增路由 | 在 `api/mod.rs::configure` 挂载；注意具体路径要注册在 `/{code}` 兜底路由之前 |
| 改前端页面 | 直接编辑 `web/`，无构建步骤；重新 `cargo build` 后生效（资源是编进二进制的） |
| 新增后台能力 | 端口加方法（`repository`）→ `service::admin` 编排 → `api::admin` 暴露，与既有用例同构 |
| 换「最近创建」的归属方式（真账号体系） | `CreationHistory` 的 `visitor` 参数换成用户 id，`api::visitor` 改为读会话即可 |

## 已知缺口

- `ip_limit` / `ua_limit` 两列已入库，但 `AccessPolicy::check` 尚未校验（只有 `is_cacheable` 会因此拒绝缓存）。
- 密码仍是明文比对，应引入 `PasswordHasher` 端口后迁移存量数据。
- 后台令牌经 `Authorization` 头明文传输，只在 HTTPS 或内网环境安全；令牌无法按会话吊销，更换 `ADMIN_TOKEN` 会让所有已登录标签页同时失效。
- 没有 migrations 机制，`talbe.sql` 需要手工执行（Docker 首次建库会自动跑一次）；已有库的增量 SQL 放在 `migrations/`，目前只有一份。
- 没有全局限流与幂等键，创建接口在异常重试下可能重复建链。
