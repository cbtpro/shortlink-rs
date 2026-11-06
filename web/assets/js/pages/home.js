import { $, delegate, h, openInNewTab, setBusy } from "../core/dom.js";
import { apiBase } from "../core/api-base.js";
import { applySiteName } from "../core/config.js";
import { ApiError, jsonRequest, request } from "../core/http.js";
import { humanize } from "../core/messages.js";
import { timeAgo } from "../core/format.js";
import { copyText } from "../core/clipboard.js";
import { toast } from "../core/toast.js";
import * as session from "../core/session.js";
import * as recent from "../features/recent-links.js";

const el = {
  form: $("create-form"),
  url: $("url"),
  code: $("code"),
  expire: $("expire"),
  maxVisits: $("max-visits"),
  password: $("password"),
  interstitial: $("interstitial"),
  submit: $("submit"),
  error: $("form-error"),
  result: $("result"),
  resultCode: $("result-code"),
  resultUrl: $("result-url"),
  resultMeta: $("result-meta"),
  resultWarn: $("result-warn"),
  copyResult: $("copy-result"),
  openResult: $("open-result"),
  list: $("recent-list"),
  empty: $("recent-empty"),
  clear: $("clear-recent"),
  account: $("account"),
  recentTitle: $("recent-title"),
  footerNote: $("footer-note"),
};

/** 列表与删除的语义取决于这个身份，见 features/recent-links.js 的说明 */
let signedIn = false;

/* ---------- 校验 ----------
 *
 * 前端校验只负责「少一次往返」和「指出是哪一栏错了」。
 * 规则与后端一一对应，但**后端才是权威**：绕过页面直接调接口同样拦得住，
 * 所以两边都要有，不能只留一边。
 */

const LIMITS = { url: 2048, code: 10, password: 64, maxVisits: 100_000_000 };

const CODE_PATTERN = /^[A-Za-z0-9_-]+$/;
/** 与后端 domain::new_link 的 FORBIDDEN_PASSWORD_CHARS / SEQUENCES 对齐 */
const FORBIDDEN_PASSWORD_CHARS = /['"`\\;]/;
const FORBIDDEN_PASSWORD_SEQUENCES = ["--", "/*", "*/"];
/** 与后端 SQL_KEYWORDS 对齐，同样按整词匹配 */
const SQL_KEYWORDS = new Set([
  "select", "insert", "update", "delete", "drop", "truncate", "alter", "create",
  "grant", "revoke", "union", "join", "where", "from", "exec", "execute",
  "declare", "sleep", "benchmark", "outfile", "load_file", "information_schema",
  "sqlite_master", "or", "and",
]);

function passwordProblem(raw) {
  if (!raw) return null;

  // 与后端一致：先 trim 再判，前后空格不算错，中间的空格算
  const password = raw.trim();
  if (!password) return "密码不能为空白";
  if (password.length > LIMITS.password) return `密码最长 ${LIMITS.password} 位`;
  if (/\s/.test(password)) return "密码不能包含空格";
  if (
    FORBIDDEN_PASSWORD_CHARS.test(password) ||
    FORBIDDEN_PASSWORD_SEQUENCES.some((seq) => password.includes(seq))
  ) {
    return "密码不能包含引号、反斜杠、分号或注释符号";
  }

  // 逐词匹配：`candy` 含 `and`，但按关键字拒掉它显然不对
  const tokens = password.split(/[^A-Za-z0-9_]+/);
  if (tokens.some((token) => token && SQL_KEYWORDS.has(token.toLowerCase()))) {
    return "密码不能包含 SQL 关键字";
  }

  return null;
}

/**
 * 逐字段校验，返回第一个问题：`[元素, 提示]`。
 *
 * 一次只报一个，与后端顺序一致（url → code → expire → maxVisits → password），
 * 免得用户改一个报一个来回跑。
 */
function firstProblem(values) {
  const url = values.url.trim();
  if (!url) return [el.url, "请填写目标地址"];
  if (url.length > LIMITS.url) return [el.url, `地址最长 ${LIMITS.url} 个字符`];
  if (/\s/.test(url)) return [el.url, "地址中不能包含空格"];
  if (!/^https?:\/\//i.test(url)) return [el.url, "只支持 http:// 或 https:// 开头的地址"];

  const code = values.code.trim();
  if (code && (!CODE_PATTERN.test(code) || code.length > LIMITS.code)) {
    return [el.code, "短码只能是 1–10 位的字母、数字、- 或 _"];
  }

  if (values.expire && Date.parse(values.expire) <= Date.now()) {
    return [el.expire, "有效期必须晚于当前时间"];
  }

  if (values.maxVisits !== "") {
    const visits = Number(values.maxVisits);
    if (!Number.isInteger(visits)) return [el.maxVisits, "访问次数上限必须是整数"];
    if (visits < 1) return [el.maxVisits, "访问次数上限至少为 1"];
    if (visits > LIMITS.maxVisits) {
      return [el.maxVisits, `访问次数上限不能超过 ${LIMITS.maxVisits}`];
    }
  }

  const problem = passwordProblem(values.password);
  if (problem) return [el.password, problem];

  return null;
}

/* datetime-local 给的是「2026-12-31T23:59」，补齐秒再交给服务端 */
function toApiDateTime(value) {
  if (!value) return null;
  return value.length === 16 ? `${value}:00` : value;
}

/** 把日期选择器的下界设成「此刻」，让原生控件不去选过去的日期 */
function refreshExpireBounds() {
  const now = new Date();
  const pad = (value) => String(value).padStart(2, "0");
  el.expire.min = [
    now.getFullYear(),
    pad(now.getMonth() + 1),
    pad(now.getDate()),
  ].join("-") + `T${pad(now.getHours())}:${pad(now.getMinutes())}`;
}

function buildPayload() {
  const payload = { url: el.url.value.trim() };

  const code = el.code.value.trim();
  if (code) payload.custom_code = code;

  const expire = toApiDateTime(el.expire.value);
  if (expire) payload.expire_at = expire;

  if (el.maxVisits.value !== "") payload.max_visits = Number(el.maxVisits.value);

  // 与后端归一化一致：去掉首尾空格再提交，否则用户设的和服务端存的不是同一个
  const password = el.password.value.trim();
  if (password) payload.password = password;

  if (el.interstitial.checked) payload.interstitial = true;

  return payload;
}

/* ---------- 渲染 ---------- */

function recentRow(item) {
  const shortUrl = item.short_url;
  // 服务端给的是本地墙上时间，空格分隔，Safari 需要换成 T 才能解析
  const createdAt = Date.parse(String(item.created_at || "").replace(" ", "T")) || Date.now();

  return h(
    "li",
    { class: "recent__item" },
    h(
      "div",
      { class: "recent__main" },
      h(
        "div",
        { class: "recent__title" },
        h("a", {
          href: shortUrl,
          target: "_blank",
          rel: "noopener noreferrer",
          onClick: openInNewTab,
          text: `/${item.code}`,
        }),
        item.interstitial ? h("span", { class: "recent__tag", text: "中转" }) : null,
        item.has_password ? h("span", { class: "recent__tag", text: "密码" }) : null
      ),
      h("p", { class: "recent__url", title: item.long_url, text: item.long_url }),
      h("p", { class: "recent__url", text: timeAgo(createdAt) })
    ),
    h(
      "div",
      { class: "recent__actions" },
      h("button", { type: "button", class: "icon-button", "data-action": "copy", "data-code": item.code, text: "复制" }),
      h("button", { type: "button", class: "icon-button", "data-action": "remove", "data-code": item.code, text: "删除" })
    )
  );
}

async function renderRecent() {
  let entries = [];
  try {
    entries = await recent.list();
  } catch (error) {
    // 列表拉不到不影响创建，但要说清楚是「读失败」而不是「没有记录」
    el.empty.textContent =
      error instanceof ApiError && error.status === 0
        ? "读不到记录，服务可能没启动。"
        : error instanceof ApiError && error.status === 403
          ? `${humanize(error)}——去后台改完密码再回来。`
          : "记录加载失败，刷新页面重试。";
    el.empty.hidden = false;
    el.list.replaceChildren();
    el.clear.hidden = true;
    return;
  }

  el.list.replaceChildren(...entries.map(recentRow));
  el.empty.hidden = entries.length > 0;
  // 「清空」在登录后等于删掉自己全部短链，太容易误触，干脆只对匿名访客开放
  el.clear.hidden = entries.length === 0 || signedIn;
}

/* ---------- 身份 ---------- */

function renderAccount(user) {
  if (!user) {
    el.account.replaceChildren(
      h("a", { href: "/login", text: "登录" }),
      h("span", { class: "account__sep", text: "·" }),
      h("a", { href: "/login?mode=register", text: "注册" })
    );
    return;
  }

  el.account.replaceChildren(
    h("span", {
      class: "account__user",
      title: user.email,
      text: user.display_name || user.email,
    }),
    // 后台对每个登录用户都开着：普通用户在那里看自己的短链，管理员还多一个用户页
    h("a", { class: "link-button", href: "/admin", text: "后台" }),
    h("button", {
      type: "button",
      class: "link-button",
      text: "退出",
      onClick: onLogout,
    })
  );
}

function applyIdentity(user) {
  signedIn = Boolean(user);

  // 临时口令没换掉时，列表接口会被服务端挡回来 —— 与其让列表显示「加载失败」，
  // 不如直接说清楚要先改密码
  const pendingPassword = Boolean(user?.must_change_password);

  el.recentTitle.textContent = signedIn ? "我的链接" : "最近创建";
  el.footerNote.textContent = signedIn
    ? `已登录 ${user.email}`
    : "未登录：记录按浏览器保存";
  el.empty.textContent = pendingPassword
    ? "你用的是临时口令，去后台改完密码后才能看到链接。"
    : signedIn
      ? "还没有创建过短链。"
      : "还没有记录。创建过的短链会记在这个浏览器下。";
}

async function onLogout() {
  try {
    await session.logout();
    location.reload();
  } catch (error) {
    toast(humanize(error));
  }
}

function showResult(link) {
  el.resultCode.textContent = `/${link.code}`;
  el.resultUrl.textContent = link.shortUrl;
  el.resultUrl.href = link.shortUrl;
  el.openResult.href = link.shortUrl;

  const details = [`创建于 ${link.createdAt}`];
  if (link.expireAt) details.push(`有效期至 ${link.expireAt.replace("T", " ")}`);
  if (link.maxVisits) details.push(`限 ${link.maxVisits} 次访问`);
  if (link.interstitial) details.push("已开启中转页");
  el.resultMeta.textContent = details.join(" · ");

  el.resultWarn.hidden = !link.hasPassword;
  if (link.hasPassword) {
    el.resultWarn.textContent = "该链接已设置密码，访问时需带上 ?password=… 参数。";
  }

  el.result.hidden = false;
  el.copyResult.focus({ preventScroll: true });
}

/** 把焦点送到出错的字段上。
 *
 * 「更多选项」是折叠的，对里面的 input 调 focus() 没有效果 ——
 * 先把分组展开，否则用户只看到一句报错，却在页面上找不到那个输入框。
 */
function revealField(field) {
  const details = field.closest("details");
  if (details && !details.open) details.open = true;
  field.focus({ preventScroll: false });
}

function showError(message, field) {
  el.error.textContent = message;
  el.error.hidden = false;

  if (field) {
    field.setAttribute("aria-invalid", "true");
    revealField(field);
  }
}

function clearError() {
  el.error.hidden = true;
  el.error.textContent = "";
  for (const field of [el.url, el.code, el.expire, el.maxVisits, el.password]) {
    field.removeAttribute("aria-invalid");
  }
}

/* ---------- 提交 ---------- */

async function onSubmit(event) {
  event.preventDefault();
  clearError();

  const problem = firstProblem({
    url: el.url.value,
    code: el.code.value,
    expire: el.expire.value,
    maxVisits: el.maxVisits.value,
    password: el.password.value,
  });
  if (problem) {
    // 元组是 [字段, 文案]，顺序与 showError(文案, 字段) 相反，这里显式解构别用展开
    const [field, message] = problem;
    showError(message, field);
    return;
  }

  setBusy(el.submit, true);
  try {
    const payload = buildPayload();
    const body = await request(`${apiBase()}/shorten`, jsonRequest(payload));

    const record = {
      code: body.code,
      shortUrl: body.short_url,
      longUrl: body.long_url,
      createdAt: body.created_at,
      maxVisits: payload.max_visits || null,
      expireAt: el.expire.value || null,
      hasPassword: Boolean(payload.password),
      interstitial: Boolean(body.interstitial),
    };

    showResult(record);
    // 服务端已经记下这一笔，重新拉一次即可，本地不再保存副本
    await renderRecent();
    el.form.reset();
  } catch (error) {
    showError(humanize(error));
  } finally {
    setBusy(el.submit, false);
  }
}

/* ---------- 事件 ---------- */

el.form.addEventListener("submit", onSubmit);

// 页面开着不动也会「现在」变旧，所以每次聚焦重算一次下界
el.expire.addEventListener("focus", refreshExpireBounds);

el.copyResult.addEventListener("click", async () => {
  const copied = await copyText(el.resultUrl.textContent);
  toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
});

// 结果区的两个入口都走同一层兜底：target="_blank" 在部分壳浏览器里会被忽略，
// 那样点击会把刚生成的结果顶掉，用户得从头再建一次。
for (const link of [el.resultUrl, el.openResult]) {
  link.addEventListener("click", openInNewTab);
}

delegate(el.list, "[data-action]", "click", async (_event, button) => {
  const { action, code } = button.dataset;

  if (action === "copy") {
    // 行里的短链地址就是复制目标，不必再为一次复制去拉整个列表
    const link = button.closest(".recent__item")?.querySelector(".recent__title a");
    const copied = await copyText(link?.href || "");
    toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
    return;
  }

  if (action === "remove") {
    // 登录后删除等于让短链失效，值得问一句；匿名访客只是移出清单，不拦
    if (signedIn && !window.confirm(`删除 /${code}？删除后短链立即失效。`)) return;

    button.disabled = true;
    try {
      await recent.forget(code);
      await renderRecent();
      toast(signedIn ? "已删除，短链已失效" : "已从列表移出");
    } catch (error) {
      button.disabled = false;
      toast(humanize(error));
    }
  }
});

el.clear.addEventListener("click", async () => {
  if (!window.confirm("清空全部记录？短链本身不受影响。")) return;

  try {
    await recent.clear();
    await renderRecent();
    toast("已清空");
  } catch (error) {
    toast(humanize(error));
  }
});

applySiteName();
refreshExpireBounds();

const user = await session.currentUser();
renderAccount(user);
applyIdentity(user);
await renderRecent();
