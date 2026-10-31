# 测试说明

## 运行

```bash
cargo test                                  # 全部用例，约 2 秒
cargo test domain::                         # 只跑某一层
cargo test expired                          # 按名字模糊匹配
cargo test -- --nocapture                   # 打印日志（warn 降级路径会用到）
cargo watch -x test                         # 保存即跑
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
| 单元 | 无 | 规则与边界值（纯函数） | `domain/`、`config`、`error` |
| 用例 | 内存实现 | 编排顺序、降级策略、重试逻辑 | `service/`、`tasks/` |
| 接口 | `actix_web::test` | 路由、状态码、响应体 | `api/mod.rs` |

用例层和接口层都用 `src/test_support.rs` 里的假实现替换 MySQL / Redis，
因此 `cargo test` 不需要任何容器在跑。

## 用例分布

| 模块 | 数量 | 覆盖的行为 |
| --- | --- | --- |
| `api` | 19 | 201/400/403/404/409/410 全链路口径一致；后台鉴权（401/禁用 404）、列表、删除、缓存清理；静态资源不被 `/{code}` 兜底路由截获 |
| `domain::short_link` | 16 | 过期边界（`<=`）、次数上限边界、未回刷增量、溢出饱和、缓存投影不含密码；`state()` 与 `AccessPolicy::check` 结论一致 |
| `domain::new_link` | 13 | URL 协议/空白、短码长度与字符集、校验顺序、可选字段 |
| `domain::code` | 10 | 生成长度、字符集白名单、空串与非 ASCII、自定义上限 |
| `repository::memory` | 11 | 自增 id、冲突、增量累加、饱和；分页排序、状态筛选、大小写搜索、概览、删除 |
| `service::shorten` | 10 | 自定义码保留、碰撞重试、5 次耗尽、非法输入、故障不重试 |
| `service::redirect` | 10 | 缓存命中不回源、缓存故障降级、受限链接不缓存、先判规则再计数 |
| `service::admin` | 9 | 令牌校验（含常量时间比较、禁用时不放行）、分页收敛、删除清缓存、缓存故障不阻断删除、时钟透传 |
| `api::dto` | 7 | 字段映射、URL trim、本地时间 ↔ UTC、响应格式 |
| `config` | 7 | 默认值、连接串脱敏、后台令牌解析（空白禁用、trim、过短拒绝） |
| `tasks::flush_visits` | 6 | 落库后抵扣、失败保留、多轮累加、扫描失败上报、链接已删除时丢弃增量 |
| `error` | 5 | 状态码映射表（含 401 / 后台禁用 404）、敏感信息隐藏、响应体 `code` 与状态码一致 |
| `cache::redis_store` | 4 | key 前缀与版本、内容/计数 key 不重叠、载荷往返、错误脱敏 |
| `api::admin` | 4 | 查询参数默认值与状态映射、未知状态回退 `all`、总页数进位、本地零点窗口 |
| `repository::mysql` | 3 | WHERE 占位符顺序与绑定顺序一致、LIKE 通配符转义 |
| `api::assets` | 2 | 资源确实编进二进制、页面引用的文件名与路由一致 |

## 测试替身

全部在 `src/test_support.rs`，命名即语义：

| 替身 | 行为 | 用来验证 |
| --- | --- | --- |
| `NoOpCache` / `NoOpCounter` | 永不命中、写入即丢、从不失败 | 只关心业务规则的用例 |
| `BrokenCache` / `BrokenCounter` | 所有操作返回错误 | 降级路径（Redis 挂了仍要能跳转） |
| `RecordingCache` | 记录写入，可预置条目 | 命中、回填、受限链接不缓存 |
| `RecordingCounter` | 维护待回刷增量，可断言自增次数 | 计数时机、上限判定含未回刷增量 |
| `ScriptedCodeGenerator` | 按脚本吐短码，可数调用次数 | 碰撞重试与耗尽 |
| `InMemoryLinkRepository` | 内存仓储（`repository/memory.rs`） | 所有不需要真实数据库的用例 |

## 未覆盖的部分

| 缺口 | 原因 | 怎么补 |
| --- | --- | --- |
| `repository/mysql.rs` 的真实 SQL | 需要活的 MySQL | 手动跑下面的冒烟脚本；或引入 `sqlx::test` 后接 CI 服务 |
| `cache/redis_store.rs` 的实际读写 | 需要活的 Redis | 同上 |
| `VisitFlusher::spawn` 的定时调度 | 依赖 tokio 时钟，容易写出偶发失败 | 目前只测 `flush_once`，调度本身靠打日志观察 |
| 并发下的计数与建链竞态 | 单元测试无法真复现 | 压测工具（`oha` / `wrk`）打 `GET /{code}` 观察 Redis 与库的差值 |

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

## 写测试的约定

- 名字描述**行为**而不是方法名：`rejects_when_pending_visits_already_reach_limit`，不是 `test_check_2`。
- 一个用例只断言一个规则；边界值（`<` / `=` / `>`）要各来一个。
- 需要失败路径就用 `Broken*` 替身，不要在测试里 `unwrap()` 掉错误。
- 断言错误时用 `matches!` 匹配变体，避免测试和文案耦合。
- 新增访问规则时，同步在 `domain::short_link` 补边界用例，并在 `api` 补一条状态码用例。
