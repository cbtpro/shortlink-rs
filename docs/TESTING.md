# 测试说明

## 运行

```bash
cargo xtask check                           # fmt --check + clippy -D warnings + test + web-test，提交前跑这个
cargo test                                  # 全部 Rust 用例，约 2 秒
cargo xtask web-test                        # 只跑前端用例（需先在 web-test/ 装依赖）
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
| `api` | 56 | 201/400/403/404/409/410 全链路口径一致；后台鉴权（无会话 401、待改密 403、非管理员 403、停用即刻失效）、列表、删除、缓存清理；普通用户只看/只删自己的链接、传 `owner` 也无效；管理员重置密码与启停账号、不能动自己与另一个管理员；`role` / `status` / `must_change_password` 的响应契约；改密后删除一次性口令文件（普通用户改密不误删）；注册登录注销与会话 cookie；链接归属与跨用户删除；匿名 / 登录两套「最近创建」语义；各类非法输入 400；静态资源不被 `/{code}` 兜底路由截获 |
| `domain::new_link` | 35 | URL 协议/长度/空白、短码长度与字符集、密码长度/空白/注入符号/SQL 关键字（含「子串不算」的误伤用例）、次数上下界、过期时间与 `now` 的边界、校验顺序 |
| `settings`（含 `env_file`） | 25 | 默认值、连接串脱敏、管理员邮箱校验（空白回退默认、非法地址启动即失败、大小写与空格归一）、口令文件路径可配、会话有效期与迭代数校验、多层覆盖优先级 |
| `service::console` | 24 | 普通用户的归属被强制覆盖、关键词不越出作用域、管理员可按归属筛、越权删除与不存在同为 404、删除清缓存、缓存故障不阻断删除、列表把未回刷增量加到库里的值上；重置密码返回一次性口令并要求改密、踢掉旧会话、不能动自己与另一个管理员（重置与停用各一条）、停用即时生效、重新启用后恢复 |
| `repository::memory` | 20 | 链接：自增 id、冲突、增量累加、饱和、分页排序、状态筛选、大小写搜索、概览、删除；用户：id 递增、邮箱唯一、批量取（跳过缺失与空入参）、`find_first_admin` 只认管理员、列表倒序与名下链接数、邮箱/昵称搜索、写操作对缺失 id 返回 false；会话：按用户删除只影响本人、禁用后立即查不出、到期边界 |
| `service::redirect` | 17 | 缓存命中不回源、缓存故障降级、受限链接不缓存、先判规则再计数 |
| `service::auth` | 16 | 注册即登录、邮箱归一化与重复、弱口令拒绝、登录成功/失败、失败文案一致、多会话独立、注销只影响一个令牌、过期失效、畸形令牌不查库、过期清理、TTL 透传；改密换掉凭据并踢掉其他会话、新旧口令相同的拒绝、停用账号先验口令再报状态 |
| `domain::short_link` | 15 | 过期边界（`<=`）、次数上限边界、未回刷增量、溢出饱和、缓存投影不含密码；`state()` 与 `AccessPolicy::check` 结论一致 |
| `domain::account` | 15 | 邮箱格式、口令长度与空白、昵称长度与控制字符、归一化、缺省昵称取邮箱前缀；角色与状态的未知值降级方向（role→user、status→disabled）、线名往返、`User` 的角色/状态谓词互不牵连 |
| `api::dto` | 15 | 字段映射、URL/短码/密码的 trim、空密码不降级为 `None`、负数与溢出、本地时间 ↔ UTC、响应格式 |
| `service::shorten` | 12 | 自定义码保留、碰撞重试、5 次耗尽、非法输入、故障不重试、过去日期与 0 次被拒 |
| `security::password` | 11 | 往返校验、盐随机、编码串自带参数（跨迭代数可验证）、拒绝畸形串、迭代数收敛、RFC 6070 已知答案、hex 往返；一次性口令的长度、字符集（去掉易混字符）与长度收敛 |
| `assets` | 11 | 页面确实编进二进制、页面引用的文件名与路由一致、**顺着模块 import 图逐个验证都能取到**、相对路径解析规则、disk 模式逐请求读盘、ETag 稳定 |
| `domain::code` | 10 | 生成长度、字符集白名单、空串与非 ASCII、自定义上限 |
| `api::template` | 10 | 模板占位符替换、HTML 转义、缺失占位符不残留 |
| `repository::mysql` | 9 | WHERE 占位符顺序与绑定顺序一致、LIKE 通配符转义、唯一键冲突映射、归属条件进 SQL、用户表列清单与 `FromRow` 一一对应、建号语句覆盖 role/status/must_change_password、改密语句带审计时间、改状态只动一列 |
| `service::my_links` | 8 | 按归属列出、删除自己的、删别人的不动数据、清空逐条清缓存、缓存故障不影响删除 |
| `bootstrap` | 8 | 凭据文件的内容与权限（unix 0600）、缺失文件不算错；`ensure_admin`：首次建号并写文件、已有管理员不重复建、已改密后清掉陈旧文件、待改密时保留文件、邮箱被普通用户占用时报错且不留凭据、写文件失败时不建号（防止锁死）、真实散列能与文件口令互相验证 |
| `api::notice` | 7 | 过期 / 上限 / 密码 / 不存在四种状态页、短码转义、页面只引用已打包资源 |
| `tasks::flush_visits` | 6 | 落库后抵扣、失败保留、多轮累加、扫描失败上报、链接已删除时丢弃增量 |
| `service::history` | 6 | 记录上限、去重、删除只移出清单、清空 |
| `error` | 6 | 状态码映射表（400 解析失败 / 401 / 403 三种权限拒绝 / 404 / 409 / 500）、敏感信息隐藏、403 文案原样透出（前端按它分流）、响应体 `code` 与状态码一致 |
| `security::token` | 4 | 长度与字符集、两次生成不重复、指纹稳定且不可逆、畸形令牌判定 |
| `cache::redis_store` | 4 | key 前缀与版本、内容/计数 key 不重叠、载荷往返、错误脱敏 |
| `api::visitor` | 4 | 首次签发、复用已有 cookie、拒绝畸形值、cookie 属性 |
| `api::session` | 4 | 从 header 里取值、忽略格式不对的、cookie 属性（HttpOnly / SameSite / Secure 条件）、清除 cookie |
| `api::console` | 4 | 查询参数默认值与状态映射、未知状态回退 `all`、总页数进位、本地零点窗口 |
| `api::redirect` | 3 | 跳转、密码表单、越界条件对应的状态页 |
| `service`（`run_blocking`） | 2 | 正常返回、任务 panic 转 500 |
| `cache`（批量读增量） | 2 | `pending_of_many` 只返回有积压的短码、空批次不查询 |
| `api::interstitial` | 2 | 占位符替换、目标 host 提取 |
| `api::assets` | 2 | `If-None-Match` 命中返回未修改、缺失 ETag 视为不新鲜 |
| `tasks::purge_sessions` | 1 | 只删已过期的会话 |

合计 **374** 个用例（`cargo test` 约 2 秒）。前端另有 22 条，见下一节。

## 前端用例（`web-test/`）

前端零构建这一点没有变：`web-test/` 里的用例直接 `import` `web/assets/js/**` 下那几个文件本身，
不打包、不转译，断言的代码和浏览器加载的是同一份。运行时只用 Node 自带的能力 ——
测试框架是 `node:test`，DOM 是 jsdom（唯一的开发依赖，`web-test/package.json` 里锁着版本）。

```bash
cd web-test && npm install     # 只需一次
cargo xtask web-test           # 等价于 cd web-test && npm test
```

`cargo xtask check` 也会跑这一段，但**依赖没装时只跳过、不失败**：Rust 侧的闸门不该被 node 绑住，
仓库离线也能编译。跳过时会打出 `cd web-test && npm install`。想让「跳过」变成错误就用
`cargo xtask web-test` —— 缺依赖直接退 1，避免以为跑过了。

| 文件 | 数量 | 覆盖的行为 |
| --- | --- | --- |
| `console-api.test.js` | 10 | 401 → `AuthError`；两种 403（停用 / 待改密）分开归类，其余 403 不冒充停用；断网不算掉线；路径拼上 `apiBase` 且不叠 `//`；短码做 URL 编码、204 映成 `null`；启停发 JSON；重置原样透出一次性口令；查询串原样透传 |
| `armed-button.test.js` | 6 | 首次点击只进入待确认（不动 base class）；二次点击才执行，请求期间禁用、结束恢复；请求中连点不会再发一次；超时自动复原且复原后仍需二次确认；`type="button"` 不变成提交钮 |
| `messages.test.js` | 4 | 文案表命中即翻译、未命中按状态码兜底、再不行透出服务端原话；非 `ApiError` 也有兜底话 |
| `api-base.test.js` | 2 | 反代到子路径时从 `location.pathname` 倒推前缀，页尾斜杠不多留一个 |

挑这几处的标准是「坏掉之后界面仍然看着正常」：401 归错类会把所有人莫名踢去登录页，
两段确认漏掉会让一次手滑删掉别人的链接，`apiBase` 算错会让整页数据 404 而页面本身打得开。

新增用例时保持同样的形态：每个测试文件自己装一份 jsdom、自己替 `fetch`，
不共享全局状态 —— `node --test` 每个文件一个进程，天然隔离。

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
| `PlainHasher` | 不散列，把明文当「散列」 | 用例层只测流程；PBKDF2 本身另有 11 个用例 |

`PlainHasher` 是刻意的：如果把真 PBKDF2 放进接口测试，374 个用例里几十次登录各算一遍，
整套测试会从 2 秒变成几分钟，而它要验证的是「流程对不对」，不是「散列强不强」。

## 未覆盖的部分

| 缺口 | 原因 | 怎么补 |
| --- | --- | --- |
| `repository/mysql.rs` 的真实 SQL | 需要活的 MySQL | 手动跑下面的冒烟脚本；或引入 `sqlx::test` 后接 CI 服务 |
| `cache/redis_store.rs` 的实际读写 | 需要活的 Redis | 同上 |
| `VisitFlusher::spawn` / `SessionPurger::spawn` 的定时调度 | 依赖 tokio 时钟，容易写出偶发失败 | 目前只测 `*_once`，调度本身靠打日志观察 |
| PBKDF2 的真实耗时与迭代数选择 | 按机器性能而定，不能写成断言 | 手工计时：`curl -w "%{time_total}\n"` 打 `/auth/login` |
| 并发下的计数与建链竞态 | 单元测试无法真复现 | 压测工具（`oha` / `wrk`）打 `GET /{code}` 观察 Redis 与库的差值 |
| 真浏览器行为（cookie 的 HttpOnly / SameSite、`location` 跳转、布局与 CSS） | jsdom 是模拟环境：不发真 cookie，属性由浏览器执行；样式完全不解析 | 手工冒烟，或用 CDP 跑一次性脚本。逻辑层已由 `web-test/` 兜住，剩下的是「浏览器真的照做吗」 |
| 首页 / 登录页的字段校验提示与聚焦 | jsdom 有 `focus()` 与 `activeElement`，测得到；但这两处的规则直接写在页面入口里，要先搭出整页 DOM | 把校验规则抽成可导入的函数即可进 `web-test/`（`password.js` 已具备条件）；在抽之前靠手工逐条填非法值 |
| 后台页面的整体编排（未登录跳转、改密面板 vs 控制台的切换、一次性口令浮层开合） | 在 `pages/admin/index.js` 里，依赖整页 DOM 与全部接口；改它的频率低于被它调用的模块 | 手工点一遍；底层模块（错误分拣、两段确认）已有用例 |

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

# 手动触发一次回刷后核对数据库（默认 60 秒一轮，等不及就重启服务，启动时会先补刷一次）
docker exec shortlink-mysql mysql -uroot -proot short_link -e \
  "SELECT code, visit_count FROM short_links"
```

中转页的计数口径是「点『继续访问』才算」，冒烟时按两步走：

```bash
curl -s --noproxy '*' -X POST http://127.0.0.1:9981/shorten -H "Content-Type: application/json" \
  -d '{"url":"https://example.com/gate","custom_code":"gate01","interstitial":true}'

# 只打开确认页：计数 key 应该不存在
curl -s -o /dev/null --noproxy '*' http://127.0.0.1:9981/gate01
docker exec shortlink-redis redis-cli get "short:v3:visits:gate01"   # 期望 (nil)

# 点「继续访问」：这一跳才计数，且票据只能用一次
NEXT=$(curl -s --noproxy '*' http://127.0.0.1:9981/gate01 | grep -o '/gate01?confirm=[a-f0-9]*')
curl -s -o /dev/null -w "%{http_code}\n" --noproxy '*' "http://127.0.0.1:9981$NEXT"   # 期望 302
docker exec shortlink-redis redis-cli get "short:v3:visits:gate01"   # 期望 1
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

后台链路（一次性口令 → 强制改密 → 管理用户）：

```bash
B=http://127.0.0.1:9981
PW=$(sed -n 's/^password=//p' data/admin-initial-password.txt)   # 首次启动后才存在

# 用一次性口令登录：响应里 must_change_password 应为 true
curl -s -c /tmp/a.txt -X POST $B/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@shortlink.local\",\"password\":\"$PW\"}"

# 没改密之前，除了改密接口一律 403 password must be changed before continuing
curl -s -b /tmp/a.txt -o /dev/null -w "%{http_code}\n" $B/console/api/links          # 403

# 改密：响应里 must_change_password 变 false，旧会话被踢、当前设备换新 cookie
curl -s -b /tmp/a.txt -c /tmp/a.txt -X POST $B/auth/password -H 'Content-Type: application/json' \
  -d "{\"current_password\":\"$PW\",\"new_password\":\"admin-2026\"}"
ls data/                                        # 期望：文件已删除

# 旧口令 401、新口令 200
curl -s -o /dev/null -w "%{http_code}\n" -X POST $B/auth/login -H 'Content-Type: application/json' \
  -d "{\"email\":\"admin@shortlink.local\",\"password\":\"$PW\"}"
curl -s -c /tmp/a.txt -o /dev/null -w "%{http_code}\n" -X POST $B/auth/login -H 'Content-Type: application/json' \
  -d '{"email":"admin@shortlink.local","password":"admin-2026"}'

# 管理员：看全站、按归属筛、看用户表
curl -s -b /tmp/a.txt "$B/console/api/links?size=3"
curl -s -b /tmp/a.txt "$B/console/api/links?owner=12"
curl -s -b /tmp/a.txt "$B/admin/api/users?size=5"

# 普通用户：只能看自己的；传 owner 也无效；用户表 403；删别人的链接 404
curl -s -b /tmp/sl.txt "$B/console/api/links?owner=9"
curl -s -b /tmp/sl.txt -o /dev/null -w "%{http_code}\n" $B/admin/api/users
curl -s -b /tmp/sl.txt -o /dev/null -w "%{http_code}\n" -X DELETE $B/console/api/links/gatesec

# 重置密码：响应里带一次性口令（只出现这一次，且 no-store）
curl -s -b /tmp/a.txt -D - -X POST $B/admin/api/users/12/password

# 管理边界：停用 / 重置自己都是 403
curl -s -b /tmp/a.txt -X POST $B/admin/api/users/11/status -H 'Content-Type: application/json' -d '{"status":"disabled"}'
curl -s -b /tmp/a.txt -X POST $B/admin/api/users/11/password

# 停用别人：对方下一次请求立刻 401，登录变 403 account is disabled
curl -s -b /tmp/a.txt -X POST $B/admin/api/users/12/status -H 'Content-Type: application/json' -d '{"status":"disabled"}'
curl -s -b /tmp/sl.txt -o /dev/null -w "%{http_code}\n" $B/console/api/links          # 401
```

## 写测试的约定

- 名字描述**行为**而不是方法名：`rejects_when_pending_visits_already_reach_limit`，不是 `test_check_2`。
- 一个用例只断言一个规则；边界值（`<` / `=` / `>`）要各来一个。
- 需要失败路径就用 `Broken*` 替身，不要在测试里 `unwrap()` 掉错误。
- 断言错误时用 `matches!` 匹配变体，避免测试和文案耦合。
- 新增访问规则时，同步在 `domain::short_link` 补边界用例，并在 `api` 补一条状态码用例。
- 碰到口令的用例一律用 `PlainHasher`，真 PBKDF2 只在 `security::password` 里出现。
