# 接口文档

服务只有一个资源：短链接。

- 基地址：`PUBLIC_BASE_URL`（默认 `http://127.0.0.1:8080`）
- 请求与响应均为 `application/json`（跳转接口除外）
- `GET /` 是内嵌的操作界面（单页），`GET /admin` 是管理后台；资源集中在
  `GET /assets/{path}`（样式与 ES 模块，清单见 `src/api/assets.rs`），另有 `/favicon.ico`（204），
  全部注册在 `/{code}` 兜底路由之前；前端源码在 `web/`，无构建步骤

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

## POST /shorten

创建短链接。

### 请求体

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `url` | string | 是 | 目标地址，必须以 `http://` 或 `https://` 开头，不能含空白字符。首尾空格会被去掉 |
| `custom_code` | string | 否 | 自定义短码，1–10 位，仅 `[A-Za-z0-9-_]`。留空则由服务端生成 |
| `expire_at` | string | 否 | 过期时间，本地时间。到点即失效（含等于） |
| `max_visits` | integer | 否 | 最大成功跳转次数，含尚未回刷的计数 |
| `password` | string | 否 | 访问密码，跳转时用 `?password=` 提交 |
| `ip_limit` | object | 否 | 预留字段，当前仅存储、不参与判定（见「已知限制」） |
| `ua_limit` | object | 否 | 同上 |
| `interstitial` | boolean | 否 | `true` 时跳转前先返回中转确认页，默认 `false` |

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
| 400 | `url must not be empty` | `url` 为空白 |
| 400 | `url must start with http:// or https://` | 协议不支持（`javascript:`、`ftp:` 等） |
| 400 | `url must not contain whitespace` | URL 中间含空格 |
| 400 | `custom code must be 1-10 chars of [A-Za-z0-9-_]` | 短码超长或含非法字符 |
| 400 | `password must not be empty` | 传了空密码 |
| 409 | `short code already exists` | 自定义短码已被占用 |
| 500 | `failed to allocate a unique short code, please retry` | 随机短码连续 5 次撞码（极小概率） |

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

首页的「最近创建」列表存在服务端，按**匿名浏览器**归属：没有账号体系，
服务用 `sl_visitor` cookie（HttpOnly + SameSite=Lax，有效期一年）标识浏览器，
创建记录落在 `link_creations` 表，每访客最多保留 `HISTORY_RECENT_LIMIT`（默认 20）条。

- 首次访问任意 `/recent*` 或 `POST /shorten` 会签发 cookie；
- 记录随短链删除而消失（外键级联），不会指向已删除的链接；
- 清除 cookie 等同于换了一个新访客，旧记录还在库里但不可再访问；
- 这只是便利功能，不用于审计，也不与任何账号关联。

### GET /recent

当前浏览器最近的创建，按时间倒序。

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

删除一条创建记录，成功 `204`；记录不存在或没有访客 cookie 返回 `404`。

### DELETE /recent

清空当前浏览器的全部记录，成功 `204`。没有记录（或没有 cookie）同样返回 `204`。

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

## 状态码总表

| 状态码 | 含义 | 典型场景 |
| --- | --- | --- |
| 201 | 创建成功 | `POST /shorten` |
| 204 | 删除成功 | `DELETE /admin/api/links/{code}`、`DELETE /recent*` |
| 302 | 跳转 | `GET /{code}` |
| 200 | 中转确认页 | `GET /{code}`，链接开启 `interstitial` 时 |
| 400 | 参数不合法 | URL 协议不对、短码非法 |
| 401 | 后台口令无效 | 缺少或错误的 Bearer token |
| 403 | 缺少或错误的密码 | 受保护链接 |
| 404 | 短码不存在 / 后台未启用 | 拼错短码；未配置 `ADMIN_TOKEN` |
| 409 | 短码冲突 | 自定义短码重复 |
| 410 | 资源已失效 | 过期、次数用尽 |
| 500 | 服务端故障 | 数据库/Redis 不可用、短码分配失败 |

## 已知限制

- `ip_limit` / `ua_limit` 只落库、不校验。传了这两个字段会**跳过缓存**（避免规则被缓存旁路），
  但当前不影响跳转结果 —— 实现前不要依赖它做访问控制。
- 密码为明文比较，仅适用于内部或低风险场景。
- 创建短链的接口没有鉴权：任何能访问服务的人都能建链。后台接口有 `ADMIN_TOKEN` 保护，
  但令牌经 `Authorization` 头明文传输，仅在 HTTPS 或内网环境下使用。
