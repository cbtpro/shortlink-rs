import { $, delegate, h, setBusy } from "../core/dom.js";
import { API_BASE } from "../core/api-base.js";
import { ApiError, jsonRequest, request } from "../core/http.js";
import { timeAgo } from "../core/format.js";
import { copyText } from "../core/clipboard.js";
import { toast } from "../core/toast.js";
import * as recent from "../features/recent-links.js";

/* 服务端校验文案 → 中文，未命中的按状态码兜底 */
const MESSAGES = {
  "url must not be empty": "请填写目标地址",
  "url must start with http:// or https://": "只支持 http:// 或 https:// 开头的地址",
  "url must not contain whitespace": "地址中不能包含空格",
  "custom code must be 1-10 chars of [A-Za-z0-9-_]": "短码只能是 1–10 位的字母、数字、- 或 _",
  "password must not be empty": "密码不能为空白",
  "short code already exists": "这个短码已被占用，换一个试试",
  "failed to allocate a unique short code, please retry": "短码分配失败，请重试",
  "short link not found": "这条记录已经不在了",
};

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
};

function humanize(error) {
  if (!(error instanceof ApiError)) return error.message;
  if (MESSAGES[error.message]) return MESSAGES[error.message];
  if (error.status === 409) return "这个短码已被占用，换一个试试";
  if (error.status >= 500) return "服务端出错了，请稍后重试";
  return error.message;
}

/* datetime-local 给的是「2026-12-31T23:59」，补齐秒再交给服务端 */
function toApiDateTime(value) {
  if (!value) return null;
  return value.length === 16 ? `${value}:00` : value;
}

function buildPayload() {
  const payload = { url: el.url.value.trim() };

  const code = el.code.value.trim();
  if (code) payload.custom_code = code;

  const expire = toApiDateTime(el.expire.value);
  if (expire) payload.expire_at = expire;

  const maxVisits = Number.parseInt(el.maxVisits.value, 10);
  if (Number.isFinite(maxVisits) && maxVisits > 0) payload.max_visits = maxVisits;

  // 密码原样提交，trim 会让用户设的和存的不是同一个
  if (el.password.value.trim()) payload.password = el.password.value;

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
        h("a", { href: shortUrl, target: "_blank", rel: "noopener noreferrer", text: `/${item.code}` }),
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
        : "记录加载失败，刷新页面重试。";
    el.empty.hidden = false;
    el.list.replaceChildren();
    el.clear.hidden = true;
    return;
  }

  el.list.replaceChildren(...entries.map(recentRow));
  el.empty.hidden = entries.length > 0;
  el.clear.hidden = entries.length === 0;
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

function showError(message) {
  el.error.textContent = message;
  el.error.hidden = false;
}

function clearError() {
  el.error.hidden = true;
  el.error.textContent = "";
  el.url.removeAttribute("aria-invalid");
  el.code.removeAttribute("aria-invalid");
}

/* ---------- 提交 ---------- */

async function onSubmit(event) {
  event.preventDefault();
  clearError();

  if (!el.url.value.trim()) {
    el.url.setAttribute("aria-invalid", "true");
    el.url.focus();
    showError("请填写目标地址");
    return;
  }

  setBusy(el.submit, true);
  try {
    const payload = buildPayload();
    const body = await request(`${API_BASE}/shorten`, jsonRequest(payload));

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

el.copyResult.addEventListener("click", async () => {
  const copied = await copyText(el.resultUrl.textContent);
  toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
});

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
    button.disabled = true;
    try {
      await recent.forget(code);
      await renderRecent();
      toast("已删除记录");
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

renderRecent();
