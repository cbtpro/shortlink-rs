# 架构说明

## 分层与依赖方向

```
main.rs          进程入口：只转发给 bootstrap
  │
bootstrap.rs     组合根：装配实现、启动服务与后台任务、端口冲突处理
  │
  ├── settings/    配置装配：Profile、多层 env 合并、校验、脱敏摘要
  ├── assets/      静态资源：disk（逐请求读盘）/ embedded（编译期内嵌）、ETag、版本号
  ├── api/         HTTP 适配层：DTO + handler，只做协议转换
  │   ├── assets      页面与 /assets/** 分发（GET /、GET /login、GET /admin）
  │   ├── auth        /auth/*：注册、登录、注销、当前用户
  │   ├── session     sl_session cookie 的读写 + MaybeAuthUser 提取器
  │   ├── app_config  /app-config.js：下发给前端的运行时配置
  │   ├── dev         /__dev/*：资源版本号与自动刷新脚本，未开启热刷新时 404
  │   ├── health      /healthz（不碰依赖）、/readyz（探测 MySQL 与 Redis）
  │   ├── history     首页「最近创建」（GET/DELETE /recent*），按身份分流两种语义
  │   ├── visitor     匿名访客 cookie 的签发与校验
  │   ├── interstitial 中转页渲染：模板占位符单遍填充 + HTML 转义
  │   └── admin       后台 JSON 接口（概览 / 列表 / 删除）
  ├── service/     用例层：shorten / redirect / admin / history / auth（账号会话）/ my_links（账号下的链接）
  ├── domain/      领域层：实体、校验规则、访问策略、短码规则、链接状态、账号规则
  ├── security/    凭据原语：PBKDF2 口令散列、会话令牌生成与指纹
  ├── repository/  持久化端口（LinkRepository + CreationHistory + UserRepository + SessionRepository）
  │                + MySQL 实现 + 内存实现（测试用）
  ├── cache/       缓存端口 + Redis 实现（内容缓存 / 访问计数 / 失效清理）
  ├── health.rs    就绪探针实现（Readiness）
  └── tasks/       后台任务：访问计数回刷、过期会话清理

web/             前端源码（原生 ES Modules，无框架、无构建）
                 开发时由 assets::AssetServer 逐请求读磁盘，发布时由 build.rs 扫描后内嵌
                 assets/css/base.css 与 assets/js/core/ 为两页共用，页面入口在 assets/js/pages/
```

依赖只向内：`api → service → domain`。`service` 依赖的是 `repository` / `cache` 里的 **trait**，
具体实现（MySQL、Redis）只在 `bootstrap.rs` 里出现一次。

| 层 | 该做 | 不该做 |
| --- | --- | --- |
| `api` | 解析参数、调用例、序列化响应 | 写业务判断、直连数据库、自己拼资源路径 |
| `service` | 编排用例、决定降级策略 | 拼 SQL、拼 Redis 命令 |
| `domain` | 规则与实体（纯函数为主） | 依赖 web / sqlx / redis |
| `repository` / `cache` | 实现端口，处理存储错误 | 决定业务语义（如「冲突是否重试」） |
| `assets` | 路径校验、Content-Type、ETag、来源切换 | 决定页面长什么样 |
| `security` | 口令散列、令牌生成与指纹、常量时间比较 | 知道「谁是谁」、碰数据库 |
| `settings` | 读值、校验、给默认值、脱敏展示 | 参与业务分支（除 `Profile` 影响的默认值外） |
| `tasks` | 定时调度与批量写 | 承载跳转路径逻辑 |

`domain` 里唯一的框架痕迹是 `sqlx::FromRow` 派生（省去手写 10 个字段的映射），
它不引入运行期依赖。

## 身份如何进入请求

```
任意请求
  └─ api::session::MaybeAuthUser（FromRequest 提取器）
      ├─ 无 sl_session  ─▶ MaybeAuthUser(None)          游客，正常继续
      └─ 有 sl_session
          ├─ token::is_well_formed  ─失败─▶ MaybeAuthUser(None)   不查库
          └─ AuthService::authenticate(token::fingerprint(t))
              └─ SessionRepository::find_user(指纹, now)
                  ├─ 命中 ─▶ MaybeAuthUser(Some(User))
                  └─ 未命中 / 已过期 ─▶ MaybeAuthUser(None)
```

鉴权只写一次，handler 拿到的就是 `Option<User>`：`Some` 说明登录了，`None` 说明没登录，
**不存在「认证失败」这个分支** —— 无效 cookie 与没带 cookie 等价。存储故障是唯一例外，
它会如实返回 `500`，而不是静默降级成游客（否则用户会以为自己莫名掉线）。

归属的写入只有一处，在 `api::shorten`：

```rust
new_link.owner_id = viewer.0.as_ref().map(|user| user.id);
```

请求体里没有 `user_id` 字段，DTO 转换时也把它钉成 `None`。归属**只由登录态决定**，
不接受客户端声明 —— 否则任何人都能把链接挂到别人名下。

## 一次登录的调用链

```
POST /auth/login
  └─ AuthService::login
      ├─ 邮箱归一化（trim + 小写）
      ├─ UserRepository::find_by_email
      ├─ 选定靶子：找到用户 → 它的散列；没找到 → 构造时预置的 decoy_hash
      ├─ spawn_blocking { hasher.verify(口令, 靶子) }     不占 actix worker
      ├─ 失败 ─▶ InvalidCredentials（不区分邮箱是否存在）
      └─ 成功 ─▶ token::mint() → 只把 SHA-256 指纹写进 user_sessions
                 └─ 明文令牌进 Set-Cookie，之后再也不出现在服务端
```

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
      └─ MaybeAuthUser ─▶ owner_id = Some(user.id) / None
          └─ ShortenService::create
              ├─ NewShortLink::validate            url → code → password，一次报第一个错
              ├─ 指定了短码 ─▶ LinkRepository::insert  冲突直接 409
              └─ 未指定   ─▶ 生成短码，最多重试 5 次   全部撞码才返回 500
      └─ 未登录才记「最近创建」：visitor::ensure + CreationHistory::record
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
| 资源清单由 `build.rs` 扫描 `web/` 生成 | 手写 `include_str!` 列表必然出现「新增文件忘了注册 → 线上 404」 | 构建脚本要维护，IDE 里没有跳转 |
| 资源来源可在 disk / embedded 间切换 | 开发要「改完即见」，发布要「单文件部署」，这是同一条链路上的两种需求 | 两套读取路径都要测；disk 模式多一次 `fs::read` |
| 前端热刷新用轮询而不是 WebSocket | 后端重启时连接必断，轮询能自愈；也不必引入推送通道 | 最坏 800ms 延迟，比推送多几个请求 |
| 配置分层而不是单份 `.env` | 「本地直读磁盘 + 生产内嵌」这类差异按环境落文件，代码里不出现 `if dev` | 要记优先级顺序（文档与启动摘要都给） |
| 基础配置编进二进制 | 发布包缺 `config/` 也能启动，避免「忘拷配置文件」这类事故 | 改默认值必须重新编译 |
| 后端热更新只做「编译成功才重启」 | 编译失败时保留旧进程，开发不中断；进程内热替换在 Rust 里不可靠 | 仍需等待增量编译 |
| 会话用不透明令牌 + `user_sessions` 表，不用 JWT | 注销要能立刻生效。JWT 想做到这点就得再维护黑名单，等于把会话表换个形式存回来 | 每个请求多一次主键查询（命中索引，可接受） |
| 库里只存令牌的 SHA-256 指纹 | 数据库泄露或备份外流时，拿到的是「算不回明文」的指纹 | 令牌无法反查，排障只能看指纹前缀 |
| 令牌每次登录都重签，不踢旧会话 | 换设备登录不该让另一台掉线 | 登出要逐设备做；过期会话靠后台清理 |
| 口令用 PBKDF2-HMAC-SHA256 | 离线环境只有 `sha2` + `hmac`；Argon2/bcrypt 需要新 crate | 没有内存硬度，只能靠迭代数撑；编码串自带参数以便日后调整 |
| 迭代数写进散列串 | 调高配置后老账号仍能用旧参数验证，不必强制改密 | 散列串比固定参数长约 10 字节 |
| 散列与校验走 `spawn_blocking` | PBKDF2 是故意慢的，占住 actix worker 会拖垮并发 | 多一层调度；panic 要手动转成 500 |
| 邮箱不存在也跑一次等价校验（decoy hash） | 否则响应时间就是账号探针：查不到立刻返回，密码错慢几百毫秒 | 每次登录多算一次散列（本来也要算） |
| 登录失败只有一种文案 | 区分「没这个邮箱」与「密码不对」等于送出注册状态 | 用户不知道自己是不是打错了邮箱 |
| `GET /auth/me` 未登录返回 200 + `null` | 语义是「现在是谁」，「没人」是正常答案，不是错误 | 调用方要判 `user` 是否为 `null` |
| 归属校验写进 SQL（`WHERE code = ? AND user_id = ?`） | 没有「先查再删」的 TOCTOU 窗口 | 删不存在的和别人返回同一个 404 |
| 他人链接与不存在都返回 404 | 403 会泄露「这个短码存在，只是不属于你」 | 用户分不清「删错了」还是「被人删了」 |
| 登录后不写 `link_creations` | 两套记录交叉会让同一条短链在两处各留一份，删一次还剩一条 | 匿名时期建的链接在登录后不可见（本来也不可认领） |
| 会话绝对过期，不滑动续期 | 长期开着的标签页该到期就到期；续期逻辑是另一种复杂度 | 活跃用户也会被登出 |
| 归一化在 DTO 的 `TryFrom`，规则在 `domain` | 「多个空格」是输入格式问题，「次数不能为 0」是业务规则；混在一起会让领域层被迫知道协议细节 | 新增字段要在两处各加一行 |
| `max_visits` 用 `i64` 接收再转 `u32` | 用 `u32` 时负数在 serde 阶段就报错，客户端拿到的是解析器原话（还带 Rust 类型名） | DTO 多一个转换函数 |
| 时间校验用调用方传入的 `now`，不读时钟 | 同一次请求里各字段基于同一时刻；「过期时间刚好等于此刻」这类边界才能测 | `ShortenService::create` 多一个参数 |
| 请求体解析失败也返回统一的 `ApiErrorBody` | 默认的纯文本 400 会让前端只能显示「HTTP 400」，还会泄露内部类型名 | 要在 `configure` 里装 `JsonConfig` |
| 访问密码黑名单（引号、注释符、SQL 整词） | 挡掉「把 SQL 当密码粘进来」的异常输入，让日志与导出保持干净 | **不构成注入防线**，真正的防线是参数化查询 |
| 「打开」一律真链接 + `window.open` 兜底 | 部分内嵌 WebView 忽略 `target="_blank"`，把地址顶掉当前页，用户刚生成的结果就没了；`window.open` 不带 features 才开标签页，带了会变弹窗 | 多一层 `preventDefault`，组合键点击要让给浏览器 |

## 扩展指引

| 想做的事 | 改动位置 |
| --- | --- |
| 换存储（Postgres / 内存 / 远程 API） | 实现 `repository::LinkRepository`，在 `bootstrap.rs` 换一行 |
| 换缓存（Memcached / 本地 LRU） | 实现 `cache::LinkCache` + `cache::VisitCounter` |
| 新增访问规则（IP 白名单、UA 黑名单） | 加在 `domain::AccessPolicy::check`，其余各层不动 |
| 新增接口字段 | 只改 `api/dto.rs` |
| 换短码算法（号段 / 雪花） | 实现 `domain::code::ShortCodeGenerator` |
| 新增路由 | 在 `api/mod.rs::configure` 挂载；注意具体路径要注册在 `/{code}` 兜底路由之前 |
| 改前端页面 | 直接编辑 `web/`，`cargo xtask dev` 下刷新即可；生产要重新构建（资源是编进二进制的） |
| 给前端加开关（主题、功能位） | `settings` 加配置项 → `api::app_config` 下发 → `web/assets/js/core/config.js` 加默认值 |
| 新增后台能力 | 端口加方法（`repository`）→ `service::admin` 编排 → `api::admin` 暴露，与既有用例同构 |
| 换「最近创建」的归属方式 | 已登录走 `MyLinksService`，未登录走 `CreationHistory`；换匿名那半套只动 `api::visitor` |
| 接入第三方登录（OAuth / 手机号） | 在 `service::auth` 加一条「拿到已确认身份」的入口，复用 `issue()`；`UserRepository` 增加按 provider 查 |
| 新增需要登录的接口 | handler 参数加 `MaybeAuthUser`，`match viewer.0 { Some(user) => …, None => Err(AppError::InvalidCredentials) }` |
| 改口令强度规则 | `domain::account` 的常量与 `NewUser::validate`，前端提示同步 `web/assets/js/pages/login.js` |

## 已知缺口

- `ip_limit` / `ua_limit` 两列已入库，但 `AccessPolicy::check` 尚未校验（只有 `is_cacheable` 会因此拒绝缓存）。
- 链接的访问密码仍是明文比对，与账号口令的 PBKDF2 不是一套；应引入同一套 `PasswordHasher` 后迁移存量数据。
- 后台令牌经 `Authorization` 头明文传输，只在 HTTPS 或内网环境安全；令牌无法按会话吊销，更换 `ADMIN_TOKEN` 会让所有已登录标签页同时失效。
- 没有 migrations 机制，`talbe.sql` 需要手工执行（Docker 首次建库会自动跑一次）；已有库的增量 SQL 放在 `migrations/`，目前只有两份。
- 没有全局限流与幂等键，创建接口在异常重试下可能重复建链。
- 账号体系缺邮箱验证、找回密码与会话管理页（看不到「在哪几台设备登录过」）。
- `AppError::Internal` 只用来承接后台线程的失败，没有统一的结构化错误上报。
