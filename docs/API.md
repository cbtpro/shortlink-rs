# 接口文档

服务只有两个资源：短链接与账号。

- 基地址：`PUBLIC_BASE_URL`（默认 `http://127.0.0.1:8080`，开发档为 `:9981`）
- 请求与响应均为 `application/json`（跳转接口除外）
- `GET /` 是操作界面（单页），`GET /login` 是登录 / 注册页，`GET /admin` 是管理后台；资源集中在
  `GET /assets/{path}`（样式与 ES 模块，清单由 `build.rs` 扫描 `web/` 自动生成），另有
  `/favicon.ico`（204），全部注册在 `/{code}` 兜底路由之前；前端源码在 `web/`，无构建步骤
- 前端启动时会加载 `GET /app-config.js`（运行时配置），运维用 `GET /healthz`、`GET /readyz`，
  见 [运行时与运维接口](#运行时与运维接口)

## 通用约定

### 时间

接口上的时间都是**本地时间**，不带时区偏移：

```
2026-12-31T23:59:59
```

空格分隔（`2026-12-31 23:59:59`）同样接受。落库时统一转成 UTC，
读回来再转回本地，因此调用方只需要关心墙上时钟。

### 错误响应

所有错误共用同一结构，`code` 与 HTTP 状态码一致：

```json
{ "code": 404, "message": "short link not found" }
```

`5xx` 的 `message` 固定为 `internal server error`，具体堆栈只进服务端日志。

`400` 有两类来源，响应结构完全一样：

- **解析失败**：缺字段、类型不对、不是合法 JSON → `request body is not valid`；
- **校验失败**：字段读进来了但不合法 → 具体的英文原因（见各接口的错误表）。

### 身份与会话

注册或登录成功后，服务通过 `Set-Cookie` 下发 `sl_session`：

```
Set-Cookie: sl_session=<64 位十六进制>; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000
```

| 属性 | 说明 |
| --- | --- |
| `HttpOnly` | 前端脚本读不到，`document.cookie` 里不会出现 —— 这是设计而非缺陷 |
| `SameSite=Lax` | 挡住跨站表单提交携带会话；不影响从外站点击链接触发的 `GET /{code}` 跳转 |
| `Max-Age` | 等于 `SESSION_TTL_DAYS`，**绝对过期**，不随请求续期 |
| `Secure` | 仅当 `PUBLIC_BASE_URL` 为 `https://` 时追加 |

服务端只存令牌的 SHA-256 指纹，明文令牌只在这一次响应里出现。库里的会话记录被删掉
（注销、过期清理、管理员删除用户）后，旧 cookie 立刻失效，没有等待窗口。

后续请求带上这个 cookie 即为登录态；未登录也能调用除 `/auth/*` 以外的全部业务接口，
只是创建出来的短链不归属任何账号。

## POST /auth/register

注册并直接登录，不要求邮箱验证。

### 请求体

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `email` | string | 是 | 邮箱，首尾空格与大小写都会被归一化后再判重 |
| `password` | string | 是 | 口令，8–128 字符，空白视为非法；不做字符类型要求 |
| `display_name` | string | 否 | 昵称，≤ 32 字符；留空取邮箱 `@` 前那段 |

```bash
curl -i -c cookies.txt -X POST http://127.0.0.1:9981/auth/register \
  -H "Content-Type: application/json" \
  -d '{"email": "me@example.com", "password": "correct horse", "display_name": "小明"}'
```

### 响应 `201 Created`

```json
{
  "user": {
    "id": 7,
    "email": "me@example.com",
    "display_name": "小明",
    "created_at": "2026-11-01 01:20:11"
  }
}
```

响应头同时带 `Cache-Control: no-store` 与 `Set-Cookie`。`password_hash` 永远不会出现在任何响应里。

### 错误

| 状态码 | `message` | 触发条件 |
| --- | --- | --- |
| 400 | `email is not a valid address` | 邮箱格式不合法 |
| 400 | `password must be at least 8 characters` | 口令短于 8 位 |
| 400 | `password must be at most 128 characters` | 口令超过 128 位 |
| 400 | `password must not be blank` | 口令全是空白 |
| 400 | `display name must be at most 32 characters` | 昵称超长 |
| 400 | `display name must not contain control characters` | 昵称含换行、制表符等控制字符 |
| 409 | `email is already registered` | 邮箱已存在（大小写不敏感） |

## POST /auth/login

```bash
curl -i -c cookies.txt -X POST http://127.0.0.1:9981/auth/login \
  -H "Content-Type: application/json" \
  -d '{"email": "me@example.com", "password": "correct horse"}'
```

### 响应 `200 OK`

结构与注册完全一致。每次登录都签发**新**令牌，旧令牌仍然有效 —— 换设备登录不该踢掉另一台。

### 错误

| 状态码 | `message` | 触发条件 |
| --- | --- | --- |
| 401 | `email or password is incorrect` | 邮箱不存在，或口令不匹配 |

两种情况给同一个状态码与同一句话，且服务端对不存在的邮箱也会执行一次等价代价的
口令校验（decoy hash）。因此**响应时间不构成账号枚举的旁路**，正常耗时在百毫秒量级，
由 `PASSWORD_HASH_ITERATIONS` 决定。

## POST /auth/logout

注销当前会话，成功 `204`。响应带一个 `Max-Age=0` 的 `sl_session` 把浏览器的 cookie 一并清掉。

```bash
curl -i -b cookies.txt -X POST http://127.0.0.1:9981/auth/logout
```

没有 cookie、cookie 格式不对、令牌已过期，一律返回 `204` —— 客户端要的结果只是「这个会话没了」。
`logout` 只影响当前这一个令牌，其他设备上的会话照旧。

## GET /auth/me

探测当前登录态，前端启动时调一次。

```json
{ "user": { "id": 7, "email": "me@example.com", "display_name": "小明", "created_at": "2026-11-01 01:20:11" } }
```

未登录（或会话已失效）时同样返回 `200`，只是 `user` 为 `null`：

```json
{ "user": null }
```

不用 `401` 是有意的：这个接口的语义是「现在是谁」，「没人」是一个正常答案。
存储故障会如实返回 `500`，而不是假装成未登录 —— 后者会让人以为莫名掉线了。
响应头带 `Cache-Control: no-store`：内容随 cookie 变化，任何缓存层存下来都会串号。

## POST /shorten

创建短链接。

**不要求登录**，但登录与否结果不同：

| 身份 | 归属 | 「最近创建」记在哪 |
| --- | --- | --- |
| 已登录（带 `sl_session`） | `short_links.user_id` 写入当前用户 | `short_links`，换设备可见 |
| 未登录 | `user_id` 为 `NULL` | `link_creations`，只属于这个浏览器 |

归属**只由登录态决定**，请求体里没有也不接受 `user_id` 字段 —— 否则谁都能把链接挂到别人名下。

### 请求体

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `url` | string | 是 | 目标地址，必须以 `http://` 或 `https://` 开头，1–2048 字符，不能含空白。首尾空格会被去掉 |
| `custom_code` | string | 否 | 自定义短码，1–10 位，仅 `[A-Za-z0-9-_]`。首尾空格会去掉；留空（或全是空格）则由服务端生成 |
| `expire_at` | string | 否 | 过期时间，本地时间。**必须晚于当前时间**，到点即失效（含等于） |
| `max_visits` | integer | 否 | 最大成功跳转次数，含尚未回刷的计数。范围 `1–100000000`，`0` 与负数都会被拒 |
| `password` | string | 否 | 访问密码，跳转时用 `?password=` 提交。1–64 字符，**不能含空白、引号、反引号、反斜杠、分号**，不能是 SQL 关键字 |
| `ip_limit` | object | 否 | 预留字段，当前仅存储、不参与判定（见「已知限制」）。须为 JSON 对象或数组，≤ 4096 字节 |
| `ua_limit` | object | 否 | 同上 |
| `interstitial` | boolean | 否 | `true` 时跳转前先返回中转确认页，默认 `false` |

归一化规则（服务端统一处理，调用方不必自己 trim）：

- `url`、`custom_code`、`password` 都会去掉**首尾**空格；
- `custom_code` 去空白后为空等同没填；
- `password` 去空白后为空会报错，**不会**被当成「不设密码」——
  悄悄降级会让人以为链接有保护；
- 密码中间的空格不会被去掉，而是直接拒绝。

```bash
curl -X POST http://127.0.0.1:9981/shorten \
  -H "Content-Type: application/json" \
  -d '{
        "url": "https://example.com/demo",
        "custom_code": "promo",
        "expire_at": "2026-12-31T23:59:59",
        "max_visits": 100
      }'
```

### 响应 `201 Created`

| 字段 | 说明 |
| --- | --- |
| `short_url` | 完整短链，由 `PUBLIC_BASE_URL` + `/` + `code` 拼成 |
| `code` | 短码 |
| `long_url` | 目标地址（已 trim） |
| `created_at` | 创建时间，本地时间，格式 `YYYY-MM-DD HH:MM:SS` |
| `interstitial` | 是否开启了中转页 |

```json
{
  "short_url": "http://127.0.0.1:9981/promo",
  "code": "promo",
  "long_url": "https://example.com/demo",
  "created_at": "2026-10-31 23:09:38",
  "interstitial": false
}
```

### 错误

| 状态码 | `message` | 触发条件 |
| --- | --- | --- |
| 400 | `request body is not valid` | 缺字段、类型不对、不是合法 JSON（如 `max_visits` 传字符串） |
| 400 | `url must not be empty` | `url` 为空白 |
| 400 | `url must be at most 2048 characters` | `url` 超长 |
| 400 | `url must start with http:// or https://` | 协议不支持（`javascript:`、`ftp:` 等） |
| 400 | `url must not contain whitespace` | URL 中间含空格 |
| 400 | `custom code must be 1-10 chars of [A-Za-z0-9-_]` | 短码超长或含非法字符 |
| 400 | `expire_at must be in the future` | 过期时间早于或等于此刻 |
| 400 | `max_visits must be between 1 and 100000000` | 次数为 0、负数或超出上界 |
| 400 | `password must not be empty` | 密码全是空白 |
| 400 | `password must be at most 64 characters` | 密码超长 |
| 400 | `password must not contain whitespace` | 密码含空格、制表符、换行 |
| 400 | `password must not contain quotes, backslashes or semicolons` | 密码含 `'` `"` `` ` `` `\` `;` 或 `--` `/*` `*/` |
| 400 | `password must not contain SQL keywords` | 密码里出现 SQL 关键字（整词） |
| 400 | `ip_limit and ua_limit must be JSON objects or arrays under 4096 bytes` | 规则字段传了标量，或体积过大 |
| 409 | `short code already exists` | 自定义短码已被占用 |
| 500 | `failed to allocate a unique short code, please retry` | 随机短码连续 5 次撞码（极小概率） |

一次只报**第一个**问题，顺序固定为 `url → custom_code → expire_at → max_visits → password`，
与页面上的字段顺序一致。校验在前端也做了一遍，但那只是为了让用户少等一次往返，
绕过页面直接调接口同样拦得住。

> 关于「密码不能含 SQL 关键字」——它**不是**注入防线。存取全部走参数化查询，
> 密码永远不会拼进 SQL 文本。这条规则挡的是「把一段 SQL 当密码粘进来」这类异常输入：
> 这种密码没人记得住，只会污染日志、导出和排障现场。
> 匹配是**整词**的，`candy`、`order`、`dropbox` 都不会被误伤。

## GET /{code}

跳转到目标地址。

### 路径与查询参数

| 参数 | 位置 | 必填 | 说明 |
| --- | --- | --- | --- |
| `code` | path | 是 | 短码 |
| `password` | query | 条件 | 链接设置了密码时必填 |

```bash
curl -i http://127.0.0.1:9981/promo
curl -i "http://127.0.0.1:9981/promo?password=123456"
```

> Windows 上如果配了全局代理，`curl` 可能把 `127.0.0.1` 也交给代理，表现为 502。
> 加 `--noproxy '*'` 即可。

### 响应 `302 Found`

```
Location: https://example.com/demo
```

### 响应 `200 OK`（开启中转页时）

创建时带了 `"interstitial": true` 的链接，跳转会先返回一张**中转确认页**
（`text/html`，`Cache-Control: no-store`）：页面显示目标主机与完整地址，
由访问者点击「继续访问」进入目标。页面没有脚本，目标地址经 HTML 转义后渲染，
并以 `Referrer-Policy: no-referrer` 输出，目标站点拿不到来源信息。

注意两点：

- **打开中转页就算一次访问**，点「继续访问」不重复计数；
- 中转页只是展示方式，密码、过期、次数上限等规则照常生效（先鉴权，后出页面）。

计数在返回 302 之后异步写入 Redis，因此「先跳转成功、计数稍后才可见」是正常现象；
后台任务每 `VISIT_FLUSH_INTERVAL_SECONDS`（默认 300 秒）批量回刷到 MySQL。

### 错误

| 状态码 | `message` | 触发条件 |
| --- | --- | --- |
| 404 | `short link not found` | 短码不存在 |
| 403 | `password is required` | 链接有密码，但请求未带 `password` |
| 403 | `invalid password` | 密码不匹配 |
| 410 | `short link has expired` | 已过 `expire_at` |
| 410 | `visit limit exceeded` | 已达到 `max_visits` |

## 最近创建

同名的一组接口，按身份走两套语义：

| | 已登录 | 未登录 |
| --- | --- | --- |
| 数据来源 | `short_links.user_id` | `link_creations`（按 `sl_visitor` cookie） |
| 范围 | 账号下全部记录 | 只属于这个浏览器 |
| 换设备 | 可见 | 不可见 |
| 删除 `DELETE /recent/{code}` | **短链本身失效** | 只从清单里移出，短链仍可跳转 |
| 清空 `DELETE /recent` | **删掉账号下全部短链** | 清空自己的清单 |

清空在两种身份下的代价差得很远，所以界面只在未登录时显示「清空」按钮：
登录后那一次点击等于删光自己所有短链，误触的代价太大。接口本身保持可用
（`DELETE /recent` 的语义就是「清空我看到的这份清单」），但脚本调用时要想清楚。

未登录时的 `sl_visitor` cookie 为 HttpOnly + SameSite=Lax，有效期一年；
首次访问任意 `/recent*` 或匿名 `POST /shorten` 会签发。清除 cookie 等同于换了一个新访客，
旧记录还在库里但不可再访问。这只是便利功能，不用于审计，也不与任何账号关联。

登录用户创建短链时不会写 `link_creations`，也不会下发 `sl_visitor` —— 两套记录不交叉，
免得同一条短链在两处各留一份、删一次还剩一条。

### GET /recent

最近 `HISTORY_RECENT_LIMIT`（默认 20）条，按创建时间倒序。

```json
{
  "items": [
    {
      "code": "promo",
      "short_url": "http://127.0.0.1:9981/promo",
      "long_url": "https://example.com/demo",
      "created_at": "2026-11-01 00:34:46",
      "expire_at": null,
      "max_visits": 100,
      "has_password": false
    }
  ]
}
```

密码只暴露 `has_password` 布尔值，明文不出服务端。

### DELETE /recent/{code}

删除一条记录，成功 `204`。

- **已登录**：按 `code + user_id` 一起删（归属条件写在 SQL 里，不做「先查再删」）。
  删掉即短链失效，之后的 `GET /{code}` 返回 `404`；Redis 缓存同步清理。
- **未登录**：只从本人的 `link_creations` 里移出，短链本身不动，跳转仍然正常。
- 别人的短链、不存在的短码，一律 `404` —— 不区分这两种情况，否则能凭响应探测短码是否存在。

### DELETE /recent

清空全部记录，成功 `204`。

- **未登录**：清空本浏览器的清单；本来就没有记录同样返回 `204`。
- **已登录**：删掉账号下的全部短链，逐条清缓存。语义见本节开头的对照表。

## 后台管理接口

页面在 `GET /admin`（源码 `web/admin.html`），数据接口都在 `/admin/api/*` 下。

**启用方式**：`.env` 配置 `ADMIN_TOKEN`（至少 8 个字符，留空则后台关闭，
所有 `/admin*` 路由返回 404 —— 连「有后台」这件事都不暴露）。

**鉴权**：除页面本身外，所有请求带 `Authorization: Bearer <ADMIN_TOKEN>`，
否则 401。口令比较为常量时间实现。

### GET /admin/api/overview

概览统计。

```json
{
  "total_links": 42,
  "total_visits": 1287,
  "created_today": 6,
  "expiring_soon": 3
}
```

- `total_visits` 只含**已回刷到库**的访问，最近几分钟的增量不计入；
- `created_today` 按服务器本地日历的「今天」划分；
- `expiring_soon` 为未来 7 天内到期的链接数。

### GET /admin/api/links

分页列表，按创建时间倒序。

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `page` | `1` | 从 1 开始 |
| `size` | `20` | 上限 100，超出按 100 处理 |
| `q` | — | 模糊匹配短码与目标地址（大小写不敏感，`%`/`_` 会被转义） |
| `status` | `all` | `all` / `active` / `expired` / `exhausted`，其他值按 `all` |

```json
{
  "items": [
    {
      "code": "promo",
      "long_url": "https://example.com/demo",
      "created_at": "2026-10-31 23:09:38",
      "expire_at": "2026-12-31 23:59:59",
      "max_visits": 100,
      "visit_count": 3,
      "has_password": false,
      "state": "active"
    }
  ],
  "page": 1,
  "page_size": 20,
  "total": 42,
  "total_pages": 3
}
```

时间字段为本地时间；`state` 的判定与跳转时的规则完全一致
（`domain::short_link::state`，有测试锁住两者不漂移）。

### DELETE /admin/api/links/{code}

删除短链，成功返回 `204`。

- 删除后立即 404，缓存同步清理；
- 该短码尚未回刷的访问计数会被**丢弃**（记录已不存在，保留只会无限重试）；
- 删除不存在的短码返回 404。

### 后台接口错误

| 状态码 | `message` | 触发条件 |
| --- | --- | --- |
| 401 | `unauthorized` | 缺少 `Authorization`、格式不对或口令不匹配 |
| 404 | `admin api is disabled` | 未配置 `ADMIN_TOKEN` |
| 404 | `short link not found` | 删除时短码不存在 |

## 运行时与运维接口

这些不是业务接口，但会出现在抓包与部署配置里，统一记在这里。

### GET /app-config.js

前端启动时加载的运行时配置。返回可执行脚本，供 `window.__APP_CONFIG__` 赋值。

```
200 OK
Content-Type: text/javascript; charset=utf-8
Cache-Control: no-store
```

```js
// generated by the server, do not edit
window.__APP_CONFIG__ = {"env":"development","siteName":"shortlink","apiBase":"","adminEnabled":true,"liveReload":true,"assetsStamp":"d8c553eddf829bab"};
```

| 字段 | 说明 |
| --- | --- |
| `env` | 当前 `APP_ENV` |
| `siteName` | 站点名，来自 `SITE_NAME` |
| `apiBase` | 接口前缀；反代到子路径时形如 `/sub`，同源部署为空串 |
| `adminEnabled` | 是否配置了 `ADMIN_TOKEN` |
| `liveReload` | 是否注入自动刷新脚本（仅开发档为 true） |
| `assetsStamp` | 资源指纹，排查「浏览器用了旧资源」时对一下 |

`no-store`：配置随环境变量变化，缓存会导致页面行为与新配置不一致。

### GET /healthz

存活检查，不访问 MySQL / Redis —— 进程活着就返回 `200`。

```json
{ "status": "ok", "profile": "development", "assets": "disk", "stamp": "d8c553eddf829bab" }
```

### GET /readyz

就绪检查，带 2 秒超时探测依赖。

| 状态码 | 响应 |
| --- | --- |
| 200 | `{"status":"ready"}` |
| 503 | `{"status":"degraded","failed":["mysql"]}`，`failed` 取值为 `mysql` / `redis` |

### GET /__dev/revision

仅当 `LIVE_RELOAD=true` 且资源来自磁盘时可用，否则 `404`。浏览器据此判断是否需要重载。

```json
{ "revision": "764024ffbd48a713", "stamp": "d8c553eddf829bab" }
```

`revision` 是 `web/` 目录下所有文件「路径 + mtime + 大小」的哈希；`stamp` 是编译期内容指纹。

### GET /__dev/live-reload.js

自动刷新脚本本体，同样只在开发档可用。页面在 `</body>` 前被注入一行
`<script type="module" src="/__dev/live-reload.js"></script>`。

## 状态码总表

| 状态码 | 含义 | 典型场景 |
| --- | --- | --- |
| 200 | 请求成功 | `POST /auth/login`、`GET /recent`、`GET /auth/me` |
| 200 | 中转确认页 | `GET /{code}`，链接开启 `interstitial` 时 |
| 201 | 创建成功 | `POST /shorten`、`POST /auth/register` |
| 204 | 成功但无内容 | `POST /auth/logout`、`DELETE /admin/api/links/{code}`、`DELETE /recent*` |
| 302 | 跳转 | `GET /{code}` |
| 304 | 资源未变化 | `GET /assets/**` 带 `If-None-Match` 命中 |
| 400 | 参数不合法 | URL 协议不对、短码非法、次数越界、日期在过去、邮箱或口令不合规、请求体解析失败 |
| 401 | 凭据无效 | 缺少或错误的 Bearer token；登录失败（邮箱与口令不区分） |
| 403 | 缺少或错误的密码 | 受保护链接 |
| 404 | 不存在 / 未启用 / 端点关闭 | 拼错短码；删别人的链接；未配置 `ADMIN_TOKEN`；生产访问 `/__dev/*` |
| 409 | 冲突 | 自定义短码重复；邮箱已注册 |
| 410 | 资源已失效 | 过期、次数用尽 |
| 500 | 服务端故障 | 数据库/Redis 不可用、短码分配失败 |
| 503 | 依赖不可用 | `/readyz` 探测到 MySQL 或 Redis 异常 |

## 已知限制

- `ip_limit` / `ua_limit` 只落库、不校验。传了这两个字段会**跳过缓存**（避免规则被缓存旁路），
  但当前不影响跳转结果 —— 实现前不要依赖它做访问控制。
- 链接的**访问密码是明文比较**，与账号口令的 PBKDF2 不是一套；仅适用于内部或低风险场景。
- 创建短链不要求登录：任何能访问服务的人都能建链，只是登录后才归属到账号。
  后台接口有 `ADMIN_TOKEN` 保护，但令牌经 `Authorization` 头明文传输，仅在 HTTPS 或内网环境下使用。
- 没有邮箱验证、找回密码、改密码与注销账号的入口。
- 没有按账号的配额与频率限制：`POST /auth/register` 可被批量调用，`POST /auth/login` 也没有
  失败次数锁定。生产部署应在反向代理层加限流。
- 匿名链接不能凭账号认领：`user_id` 为空的行没有任何途径确认「这是我建的」。
