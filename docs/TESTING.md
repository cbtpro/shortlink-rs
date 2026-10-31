# 测试说明

## 运行

```bash
cargo xtask check                           # fmt --check + clippy -D warnings + test，提交前跑这个
cargo test                                  # 全部用例，约 2 秒
cargo test domain::                         # 只跑某一层
cargo test expired                          # 按名字模糊匹配
cargo test -- --nocapture                   # 打印日志（warn 降级路径会用到）
cargo xtask dev                             # 改代码自动重启，另开一个终端跑 test 亦可
```

覆盖率（需要先 `cargo install cargo-tarpaulin`，仅 Linux/macOS 可用）：

```bash
cargo tarpaulin --out Html --skip-clean
```

## 为什么测试都写在 `src/` 里

`Cargo.toml` 只有 `[[bin]]` 隐含目标，没有 `lib` target，因此 `tests/` 目录下的集成测试
无法 `use shortlink_rs::...`。所以统一用 `#[cfg(test)] mod tests` 写在模块内，
测试代码与被测代码同域，能直接访问私有函数（如 `redis_store::link_key`）。

如果以后需要对外提供库接口，加一个 `src/lib.rs` 把模块暴露出去即可，
那时再把 HTTP 层测试挪到 `tests/` 也不迟。

## 三层策略

| 层 | 依赖 | 关注点 | 位置 |
| --- | --- | --- | --- |
| 单元 | 无 | 规则与边界值（纯函数） | `domain/`、`security/`、`settings`、`error` |
| 用例 | 内存实现 | 编排顺序、降级策略、重试逻辑 | `service/`、`tasks/` |
| 接口 | `actix_web::test` | 路由、状态码、响应体、cookie | `api/mod.rs` |

用例层和接口层都用 `src/test_support.rs` 里的假实现替换 MySQL / Redis，
因此 `cargo test` 不需要任何容器在跑。

## 用例分布

| 模块 | 数量 | 覆盖的行为 |
| --- | --- | --- |
| `api` | 44 | 201/400/403/404/409/410 全链路口径一致；后台鉴权（401/禁用 404）、列表、删除、缓存清理；注册登录注销与会话 cookie；链接归属与跨用户删除；匿名 / 登录两套「最近创建」语义；各类非法输入 400（负数次数、过去日期、密码含空格、请求体类型错误）；静态资源不被 `/{code}` 兜底路由截获 |
| `domain::new_link` | 32 | URL 协议/长度/空白、短码长度与字符集、密码长度/空白/注入符号/SQL 关键字（含「子串不算」的误伤用例）、次数上下界、过期时间与 `now` 的边界、校验顺序 |
| `settings`（含 `env_file`） | 24 | 默认值、连接串脱敏、后台令牌解析（空白禁用、trim、过短拒绝）、会话有效期与迭代数校验、多层覆盖优先级 |
| `api::dto` | 15 | 字段映射、URL/短码/密码的 trim、空密码不降级为 `None`、负数与溢出、本地时间 ↔ UTC、响应格式 |
| `domain::short_link` | 14 | 过期边界（`<=`）、次数上限边界、未回刷增量、溢出饱和、缓存投影不含密码；`state()` 与 `AccessPolicy::check` 结论一致 |
| `service::redirect` | 13 | 缓存命中不回源、缓存故障降级、受限链接不缓存、先判规则再计数 |
| `service::auth` | 12 | 注册即登录、邮箱归一化与重复、弱口令拒绝、登录成功/失败、失败文案一致、多会话独立、注销只影响一个令牌、过期失效、畸形令牌不查库、过期清理、TTL 透传 |
| `service::shorten` | 12 | 自定义码保留、碰撞重试、5 次耗尽、非法输入、故障不重试、过去日期与 0 次被拒 |
| `repository::memory` | 11 | 自增 id、冲突、增量累加、饱和；分页排序、状态筛选、大小写搜索、概览、删除；用户与会话的存取、过期判定 |
| `domain::code` | 10 | 生成长度、字符集白名单、空串与非 ASCII、自定义上限 |
| `domain::account` | 10 | 邮箱格式、口令长度与空白、昵称长度与控制字符、归一化、缺省昵称取邮箱前缀 |
| `service::admin` | 9 | 令牌校验（含常量时间比较、禁用时不放行）、分页收敛、删除清缓存、缓存故障不阻断删除、时钟透传 |
| `security::password` | 9 | 往返校验、盐随机、编码串自带参数（跨迭代数可验证）、拒绝畸形串、迭代数收敛、RFC 6070 已知答案、hex 往返 |
| `assets` | 9 | 资源确实编进二进制、页面引用的文件名与路由一致、disk 模式逐请求读盘、ETag 协商 |
| `service::my_links` | 8 | 按归属列出、删除自己的、删别人的不动数据、清空逐条清缓存、缓存故障不影响删除 |
| `tasks::flush_visits` | 6 | 落库后抵扣、失败保留、多轮累加、扫描失败上报、链接已删除时丢弃增量 |
| `service::history` | 6 | 记录上限、去重、删除只移出清单、清空 |
| `api::interstitial` | 6 | 占位符单遍替换、HTML 转义、无脚本 |
| `repository::mysql` | 5 | WHERE 占位符顺序与绑定顺序一致、LIKE 通配符转义、唯一键冲突映射、归属条件进 SQL |
| `error` | 5 | 状态码映射表（含 400 解析失败 / 401 / 409 / 后台禁用 404 / 内部故障 500）、敏感信息隐藏、响应体 `code` 与状态码一致 |
| `security::token` | 4 | 长度与字符集、两次生成不重复、指纹稳定且不可逆、畸形令牌判定 |
| `cache::redis_store` | 4 | key 前缀与版本、内容/计数 key 不重叠、载荷往返、错误脱敏 |
| `api::visitor` | 4 | 首次签发、复用已有 cookie、拒绝畸形值、cookie 属性 |
| `api::session` | 4 | 从 header 里取值、忽略格式不对的、cookie 属性（HttpOnly / SameSite / Secure 条件）、清除 cookie |
| `api::admin` | 4 | 查询参数默认值与状态映射、未知状态回退 `all`、总页数进位、本地零点窗口 |
| `api::assets` | 2 | 资源确实编进二进制、页面引用的文件名与路由一致 |
| `tasks::purge_sessions` | 1 | 只删已过期的会话 |

## 测试替身

全部在 `src/test_support.rs`，命名即语义：

| 替身 | 行为 | 用来验证 |
| --- | --- | --- |
| `NoOpCache` / `NoOpCounter` | 永不命中、写入即丢、从不失败 | 只关心业务规则的用例 |
| `BrokenCache` / `BrokenCounter` | 所有操作返回错误 | 降级路径（Redis 挂了仍要能跳转） |
| `RecordingCache` | 记录写入，可预置条目 | 命中、回填、受限链接不缓存 |
| `RecordingCounter` | 维护待回刷增量，可断言自增次数 | 计数时机、上限判定含未回刷增量 |
| `ScriptedCodeGenerator` | 按脚本吐短码，可数调用次数 | 碰撞重试与耗尽 |
| `InMemoryLinkRepository` | 内存仓储（`repository/memory.rs`）：链接 + 用户 + 会话 | 所有不需要真实数据库的用例 |
| `PlainHasher` | 不散列，把明文当「散列」 | 用例层只测流程；PBKDF2 本身另有 9 个用例 |

`PlainHasher` 是刻意的：如果把真 PBKDF2 放进接口测试，250 个用例里几十次登录各算一遍，
整套测试会从 2 秒变成几分钟，而它要验证的是「流程对不对」，不是「散列强不强」。

## 未覆盖的部分

| 缺口 | 原因 | 怎么补 |
| --- | --- | --- |
| `repository/mysql.rs` 的真实 SQL | 需要活的 MySQL | 手动跑下面的冒烟脚本；或引入 `sqlx::test` 后接 CI 服务 |
| `cache/redis_store.rs` 的实际读写 | 需要活的 Redis | 同上 |
| `VisitFlusher::spawn` / `SessionPurger::spawn` 的定时调度 | 依赖 tokio 时钟，容易写出偶发失败 | 目前只测 `*_once`，调度本身靠打日志观察 |
| PBKDF2 的真实耗时与迭代数选择 | 按机器性能而定，不能写成断言 | 手工计时：`curl -w "%{time_total}\n"` 打 `/auth/login` |
| 并发下的计数与建链竞态 | 单元测试无法真复现 | 压测工具（`oha` / `wrk`）打 `GET /{code}` 观察 Redis 与库的差值 |
| 浏览器端行为（登录跳转、按钮状态、HttpOnly） | 需要真实 DOM | 用 CDP 跑一次手工脚本，见下 |
| 前端字段校验的提示与聚焦 | 同上 | 同上：逐条填非法值，断言 `#form-error` 文案与 `activeElement` |

### 校验的冒烟清单

字段校验最容易「加了后端忘了前端」或反过来，改完照这张表过一遍：

```bash
B=http://127.0.0.1:9981/shorten
post() { curl -s --noproxy '*' -X POST $B -H 'Content-Type: application/json' -d "$1"; echo; }

post '{"url":"https://example.com","max_visits":-1}'          # 400 负数
post '{"url":"https://example.com","max_visits":0}'           # 400 零次
post '{"url":"https://example.com","max_visits":100000001}'   # 400 超上界
post '{"url":"https://example.com","expire_at":"2000-01-01T00:00:00"}'  # 400 过去
post '{"url":"https://example.com","password":"ab cd"}'       # 400 含空格
post '{"url":"https://example.com","password":"select"}'      # 400 关键字
post '{"url":"https://example.com","max_visits":"many"}'      # 400 类型错误（统一 JSON）
post '{"url":"  https://example.com  "}'                      # 201 首尾空格被 trim
post '{"url":"https://example.com","password":"candy"}'       # 201 不该误伤
```

页面上再走一遍同样的输入，确认：提示是中文、落在对应的输入框上、焦点已经跳过去，
以及「更多选项」折叠时会被自动展开。

### 手工冒烟脚本

启动容器与服务后执行（Windows 上注意 `--noproxy '*'`）：

```bash
# 建链
curl -s --noproxy '*' -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/smoke","custom_code":"smoke01"}'

# 跳转（期望 302）
curl -s -o /dev/null -w "%{http_code} %{redirect_url}\n" --noproxy '*' http://127.0.0.1:9981/smoke01

# 计数是否进 Redis（期望值为访问次数）
docker exec shortlink-redis redis-cli get "short:v3:visits:smoke01"

# 手动触发一次回刷后核对数据库（等不及 300 秒就重启服务，或用该 SQL 直接看）
docker exec shortlink-mysql mysql -uroot -proot short_link -e \
  "SELECT code, visit_count FROM short_links"
```

账号链路（`-c` 存 cookie、`-b` 带上）：

```bash
EMAIL="smoke$(date +%s)@example.com"

# 注册：期望 201 + Set-Cookie: sl_session=...（注意响应体里不该有 password_hash）
curl -s -i -c /tmp/sl.txt -X POST http://127.0.0.1:9981/auth/register \
  -H "Content-Type: application/json" \
  -d "{\"email\":\"$EMAIL\",\"password\":\"secret123\"}" | head -20

# 当前用户：期望 user 非 null
curl -s -b /tmp/sl.txt --noproxy '*' http://127.0.0.1:9981/auth/me

# 带着会话建链，然后确认它出现在「我的链接」里
curl -s -b /tmp/sl.txt -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" -d '{"url":"https://example.com/mine","custom_code":"mine01"}'
curl -s -b /tmp/sl.txt --noproxy '*' http://127.0.0.1:9981/recent

# 换个「浏览器」（不带 cookie）看同一个短码：列表里不该有它
curl -s --noproxy '*' http://127.0.0.1:9981/recent

# 删除自己的链接：204，且跳转变 404
curl -s -o /dev/null -w "%{http_code}\n" -b /tmp/sl.txt -X DELETE http://127.0.0.1:9981/recent/mine01
curl -s -o /dev/null -w "%{http_code}\n" --noproxy '*' http://127.0.0.1:9981/mine01

# 注销：204，之后 /auth/me 的 user 应为 null
curl -s -o /dev/null -w "%{http_code}\n" -b /tmp/sl.txt -X POST http://127.0.0.1:9981/auth/logout
curl -s -b /tmp/sl.txt --noproxy '*' http://127.0.0.1:9981/auth/me
```

## 写测试的约定

- 名字描述**行为**而不是方法名：`rejects_when_pending_visits_already_reach_limit`，不是 `test_check_2`。
- 一个用例只断言一个规则；边界值（`<` / `=` / `>`）要各来一个。
- 需要失败路径就用 `Broken*` 替身，不要在测试里 `unwrap()` 掉错误。
- 断言错误时用 `matches!` 匹配变体，避免测试和文案耦合。
- 新增访问规则时，同步在 `domain::short_link` 补边界用例，并在 `api` 补一条状态码用例。
- 碰到口令的用例一律用 `PlainHasher`，真 PBKDF2 只在 `security::password` 里出现。
