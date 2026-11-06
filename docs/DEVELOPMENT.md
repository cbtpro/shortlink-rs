# 开发环境

这份文档解释「怎么改代码最快看到结果」，以及这套开发环境为什么这么搭。
架构层面的分层与依赖方向见 [ARCHITECTURE.md](ARCHITECTURE.md)。

> 第一次配环境、只想按步骤把服务跑起来，看 [开发手册](HANDBOOK.md)；
> 这份文档讲的是原理与取舍。

## 一条命令

```bash
cargo xtask dev
```

等价于：

1. `docker compose up -d --wait`：把 MySQL / Redis 拉到 healthy，失败只警告不中断
   （数据库可能在远端）；
2. `cargo build`；
3. 启动 `target/debug/shortlink-rs`，把日志直接打到当前终端；
4. 进入监听循环。

打开 `http://127.0.0.1:9981/`。不想起中间件加 `--no-infra`，想换端口加 `--port 9982`。

## 两条热更新链路

| 改了什么 | 生效方式 | 耗时 |
| --- | --- | --- |
| `src/`、`config/`、`Cargo.toml`、`build.rs`、`xtask/` | 重新编译 → 杀进程 → 重启 | 几秒（增量编译） |
| `web/` 下的 HTML / CSS / JS | 不重启、不重编译，浏览器自己刷新 | 一个轮询周期（≤ 800ms） |

### 后端为什么是「重启」而不是热替换

Rust 没有可靠的函数级热替换。做进程内热更新的方案（dylib 重载之类）会把构建链路复杂化，
收益却很有限。这里选择的是最朴素也最可靠的做法：文件变更 → 增量编译 → 重启。
关键细节是**构建成功才重启**：编译失败时旧进程继续服务，你修完保存，它自己接着来。

监听用轮询（400ms 比较 mtime + 文件大小），不引入 `notify` 之类的依赖：
构建工具本身不该成为新的维护负担。目录排除 `target`、`dist`、`.git`、`.workbuddy`。

### 前端为什么能真热

开发档（development.env）把静态资源切成 `ASSETS_MODE=disk`：每个请求都从 `web/` 读文件，
不经过编译期内嵌。因此改 CSS 保存后，刷新浏览器就能看到 —— 连 `cargo build` 都不用。

页面自动刷新靠一条轮询：

```
页面 ──GET /__dev/live-reload.js──▶ 注入的模块（仅开发档）
     ──GET /__dev/revision──────▶ {"revision":"<目录 mtime 聚合>"}
     revision 变了 → location.reload()
```

- 版本号是「路径 + mtime + 大小」的哈希，不读文件内容，轮询成本可以忽略；
- 后端重启期间轮询失败会被静默吞掉，下一轮自然恢复；
- `LIVE_RELOAD=false` 或资源内嵌时，`/__dev/*` 一律 404，生产环境不存在这个入口。

CSS 目前走整页重载而不是局部替换 —— 页面状态（表单输入、滚动位置）会丢。要保留状态可以
在 `live-reload.js` 里改成「只换 `<link>` 的 query」；代价是对模块热更新的复杂度上升，
当前规模不值得。

## 配置怎么叠

```
config/base.env          编进二进制的默认值（零配置可启动）
      ↓
config/{APP_ENV}.env     环境档：端口、日志、资源模式
      ↓
.env                     兼容旧习惯（存在才读）
      ↓
.env.local               个人覆盖与本地密钥，不入库
      ↓
进程环境变量              部署时只认这一层
```

- `APP_ENV` 默认 `development`，可选 `test` / `production`；
- 配置目录查找顺序：`SHORTLINK_CONFIG_DIR` → `./config` → 可执行文件同级的 `config/`；
- 文件格式是 `KEY=VALUE`，支持整行 `#` 注释与成对引号，行内 `#` 不当注释（密码里会有）；
- 空值 `KEY=` 等同未设置，避免用占位行把默认值顶掉；
- 校验在装配时一次做完：数值越界、口令过短、必填缺失都直接启动失败。

启动日志第一行是全部关键配置的摘要（连接串已隐去凭据）：

```
profile=development addr=0.0.0.0:9981 public=http://127.0.0.1:9981 db=mysql://***@127.0.0.1:3306/short_link assets=disk live_reload=on admin=admin@shortlink.local password_file=data/admin-initial-password.txt session_ttl=30d password_iterations=100000
```

### 为什么开发档的迭代数是 10 万

`PASSWORD_HASH_ITERATIONS` 生产默认 600k（OWASP 建议），开发档压到 100k：本地一次登录
从三百多毫秒降到几十毫秒，脚本化冒烟与手工点页面都更顺手。

散列串自带迭代数，所以本地账号不会被这个差异锁住 —— 在开发库里建的账号拿到生产参数下
照样能验证通过。

另外 `Cargo.toml` 里配了两条 profile：依赖始终 `opt-level = 3`，自己的代码 dev 档用 1。
泛型代码（`Hmac<Sha256>` 就是）在**调用方** crate 里单态化，只优化依赖没有用。
不配这两条时，10 万轮 PBKDF2 要 3 秒。

想知道某个值是谁给的，用 `RUST_LOG=debug` 启动，会打出每一层配置文件的实际路径。

## 前端配置

HTML 里没有硬编码的环境信息。页面加载 `/app-config.js`（服务端生成，`no-store`），
得到：

```js
window.__APP_CONFIG__ = {
  env: "development",
  siteName: "shortlink",
  apiBase: "",            // 反代到子路径时是 "/sub"
  liveReload: true,
  assetsStamp: "d8c553eddf829bab",
};
```

- `core/config.js` 读取它，缺失时退回默认值（宁可少个开关，也不要白屏）；
- `apiBase` 来自 `PUBLIC_BASE_URL` 的路径部分，`core/api-base.js` 优先用它，
  拿不到才按当前页面路径推算；
- 站点名通过 `applySiteName()` 写进品牌位与标题；
- 想加前端开关，就在 `api::app_config` 的结构体里加字段，前端加个默认值即可。

## xtask 命令

```
cargo xtask dev [--port N] [--no-infra] [--once] [--release]
cargo xtask build
cargo xtask release
cargo xtask check
cargo xtask web-test
cargo xtask fmt [--check]
cargo xtask db <up|down|restart|ps|logs|reset>
```

- `release`：`cargo build --release --locked` 后把可执行文件、`config/`、`.env.example`
  组装到 `dist/`，整个目录拷到服务器就能跑；
- `check`：格式 → clippy（`-D warnings`）→ Rust 测试 → 前端测试，提交前跑一遍；
  前端那段在 `web-test/node_modules` 不存在时只跳过（并打出安装命令），
  这样没装 node 的机器仍能过闸门；要确认它真的跑过，单独执行 `cargo xtask web-test`；
- `web-test`：在 `web-test/` 里跑 `node --test`，用例直接 import `web/assets/js/**`，
  不打包、不转译；依赖只有一个 jsdom（`cd web-test && npm install`）；
- `db reset`：删数据卷重建，必须加 `--yes`。

`cargo xtask` 是 `.cargo/config.toml` 里的 alias，等价于 `cargo run -p xtask --`。

## 排错

| 现象 | 原因与处理 |
| --- | --- |
| `9981 is already in use` | 上个实例没退干净。按提示 `netstat -ano \| findstr :9981` 找 PID 结束，或 `--port` 换端口 |
| 改了前端没反应 | 确认 `ASSETS_MODE=disk`、`LIVE_RELOAD=true`（`/healthz` 的 `assets` 字段会显示），以及浏览器控制台里 `/__dev/revision` 是否 200 |
| 改了 Rust 没反应 | 看 `cargo xtask dev` 的输出：多半是编译失败了，旧进程还在服务 |
| 启动即报缺 `DATABASE_URL` | 配置层没读到：检查 `APP_ENV`、`config/` 是否在预期位置，必要时 `SHORTLINK_CONFIG_DIR` 指过去 |
| `/readyz` 返回 503 | 响应体里的 `failed` 会写明是 `mysql` 还是 `redis`；`cargo xtask db ps` 看容器 |
| 后台一直跳登录页 | 没有 `sl_session` cookie：没登录、账号被停用，或在别处改过密码；看 `/auth/me` 的返回 |
| 登录后只能看到改密面板 | 用的是临时口令（`must_change_password` 为真），改完才能用其他接口 |
| 登录接口慢到一秒以上 | 先看启动摘要里的 `password_iterations`；再确认 `Cargo.toml` 的 `[profile.dev]` 与 `[profile.dev.package."*"]` 两条还在 |
| 登录后刷新就掉线 | cookie 被清或 `Secure` 属性和当前协议不匹配：http 访问却把 `PUBLIC_BASE_URL` 配成了 `https://` |
| `/auth/me` 一直返回 `null` | 浏览器没带 cookie（跨站请求、`SameSite` 拦截），或会话已被后台清理 |

## 目录约定

- 前端文件**不需要注册**：`build.rs` 扫描 `web/` 生成清单，新增即生效；
- 想给 `/assets/**` 加新类型，在 `src/assets/mod.rs` 的 `mime_for` 补一行扩展名映射；
- 新增配置项：`config/base.env` 给默认值 → `settings::Config` 加字段 → `summary()` 补一行；
- 所有路径校验、ETag、Content-Type 都在 `src/assets/`，别在 handler 里另写一套。
