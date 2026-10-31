import { $, delegate, h, setBusy } from "../core/dom.js";
import { formatNumber } from "../core/format.js";
import { copyText } from "../core/clipboard.js";
import { toast } from "../core/toast.js";
import { AuthError, DisabledError, createAdminClient } from "../features/admin-api.js";

const TOKEN_KEY = "shortlink.admin.token";
const PAGE_SIZE = 20;
const SEARCH_DEBOUNCE_MS = 300;

const STATE_LABEL = {
  active: "有效",
  expired: "已过期",
  exhausted: "已达上限",
};

const state = {
  token: "",
  page: 1,
  status: "all",
  keyword: "",
};

const api = createAdminClient(() => state.token);

const el = {
  gate: $("gate"),
  gateForm: $("gate-form"),
  gateToken: $("token"),
  gateError: $("gate-error"),
  gateSubmit: $("gate-submit"),
  console: $("console"),
  topbarActions: $("topbar-actions"),
  signOut: $("sign-out"),
  statTotal: $("stat-total"),
  statVisits: $("stat-visits"),
  statToday: $("stat-today"),
  statExpiring: $("stat-expiring"),
  search: $("search"),
  filters: $("filters"),
  refresh: $("refresh"),
  error: $("board-error"),
  rows: $("rows"),
  empty: $("empty"),
  pagerInfo: $("pager-info"),
  prev: $("prev"),
  next: $("next"),
};

/* ---------- 视图切换 ---------- */

function showGate(message) {
  el.console.hidden = true;
  el.topbarActions.hidden = true;
  el.gate.hidden = false;
  el.gateError.textContent = message || "";
  el.gateError.hidden = !message;
  el.gateToken.focus();
}

function showConsole() {
  el.gate.hidden = true;
  el.console.hidden = false;
  el.topbarActions.hidden = false;
}

/* ---------- 表格 ---------- */

function shortUrlOf(code) {
  return new URL(`/${code}`, location.origin).toString();
}

async function copyLink(url) {
  const copied = await copyText(url);
  toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
}

/** 点两次才生效：第一次变成「确认删除」，3 秒不动则复原。 */
function deleteButton(code) {
  const button = h("button", { type: "button", class: "row-action row-action--danger", text: "删除" });
  let timer = 0;

  const disarm = () => {
    clearTimeout(timer);
    button.textContent = "删除";
    button.classList.remove("row-action--armed");
  };

  button.addEventListener("click", async () => {
    if (!button.classList.contains("row-action--armed")) {
      button.classList.add("row-action--armed");
      button.textContent = "确认删除";
      timer = setTimeout(disarm, 3000);
      return;
    }

    disarm();
    try {
      await api.remove(code);
      toast(`已删除 /${code}`);
      await refresh(state.page);
    } catch (error) {
      showBoardError(error);
    }
  });

  return button;
}

function buildRow(item) {
  const shortUrl = shortUrlOf(item.code);

  return h(
    "tr",
    {},
    h(
      "td",
      { class: "cell-code" },
      h("a", { href: shortUrl, target: "_blank", rel: "noopener noreferrer", text: `/${item.code}` }),
      item.has_password ? h("span", { class: "badge badge--locked", text: "密码" }) : null
    ),
    h("td", { class: "cell-url", title: item.long_url, text: item.long_url }),
    h("td", {
      class: "table__num",
      text: item.max_visits
        ? `${formatNumber(item.visit_count)} / ${formatNumber(item.max_visits)}`
        : formatNumber(item.visit_count),
    }),
    h("td", {}, h("span", { class: `badge badge--${item.state}`, text: STATE_LABEL[item.state] || item.state })),
    h("td", { class: "cell-time", text: item.created_at }),
    h("td", { class: "cell-time", text: item.expire_at || "长期" }),
    h(
      "td",
      { class: "cell-actions" },
      h("button", { type: "button", class: "row-action", text: "复制", onClick: () => copyLink(shortUrl) }),
      h("button", { type: "button", class: "row-action", text: "打开", onClick: () => window.open(shortUrl, "_blank", "noopener") }),
      deleteButton(item.code)
    )
  );
}

function renderRows(page) {
  el.rows.replaceChildren(...page.items.map(buildRow));
  el.empty.hidden = page.items.length > 0;
}

function renderStats(overview) {
  el.statTotal.textContent = formatNumber(overview.total_links);
  el.statVisits.textContent = formatNumber(overview.total_visits);
  el.statToday.textContent = formatNumber(overview.created_today);
  el.statExpiring.textContent = formatNumber(overview.expiring_soon);
}

function renderPager(page) {
  const totalPages = Math.max(1, page.total_pages);

  el.pagerInfo.textContent = page.total
    ? `共 ${formatNumber(page.total)} 条 · 第 ${page.page} / ${totalPages} 页`
    : "";
  el.prev.disabled = page.page <= 1;
  el.next.disabled = page.page >= totalPages;

  return totalPages;
}

/* ---------- 错误 ---------- */

function showBoardError(error) {
  el.error.textContent = error.message;
  el.error.hidden = false;
}

function clearBoardError() {
  el.error.hidden = true;
  el.error.textContent = "";
}

function handleError(error) {
  if (error instanceof AuthError) {
    dropToken();
    showGate(error.message);
    return;
  }

  if (error instanceof DisabledError) {
    showGate("服务端未启用后台：请在 .env 中配置 ADMIN_TOKEN 后重启服务。");
    return;
  }

  showBoardError(error);
}

/* ---------- 加载 ---------- */

function listQuery(page) {
  const params = new URLSearchParams({
    page: String(page),
    size: String(PAGE_SIZE),
    status: state.status,
  });
  if (state.keyword) params.set("q", state.keyword);
  return params.toString();
}

/** 取概览与当前页。返回请求的页码，供调用方判断是否越界。 */
async function load(page) {
  const [overview, result] = await Promise.all([api.overview(), api.list(listQuery(page))]);
  const totalPages = renderPager(result);

  showConsole();
  renderStats(overview);
  renderRows(result);

  return { totalPages };
}

async function refresh(page = state.page) {
  setBusy(el.refresh, true);
  clearBoardError();

  try {
    const { totalPages } = await load(page);
    // 删掉末页最后一条后当前页会越界，退一页重取
    if (page > totalPages) {
      state.page = totalPages;
      await refresh(totalPages);
      return;
    }
    state.page = page;
  } catch (error) {
    handleError(error);
  } finally {
    setBusy(el.refresh, false);
  }
}

/** 条件变了就回第一页 */
function reloadFromFirstPage() {
  state.page = 1;
  void refresh(1);
}

/* ---------- 口令 ---------- */

function dropToken() {
  state.token = "";
  sessionStorage.removeItem(TOKEN_KEY);
}

el.gateForm.addEventListener("submit", async (event) => {
  event.preventDefault();

  const token = el.gateToken.value.trim();
  if (!token) {
    el.gateError.textContent = "请输入口令";
    el.gateError.hidden = false;
    el.gateToken.focus();
    return;
  }

  state.token = token;
  state.page = 1;
  setBusy(el.gateSubmit, true);

  try {
    await refresh(1);
    // 验证通过才落盘，避免把错口令留在标签页里
    sessionStorage.setItem(TOKEN_KEY, token);
    el.gateToken.value = "";
  } catch (error) {
    handleError(error);
  } finally {
    setBusy(el.gateSubmit, false);
  }
});

el.signOut.addEventListener("click", () => {
  dropToken();
  showGate();
  toast("已退出");
});

/* ---------- 列表交互 ---------- */

let searchTimer = 0;
el.search.addEventListener("input", () => {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    state.keyword = el.search.value.trim();
    reloadFromFirstPage();
  }, SEARCH_DEBOUNCE_MS);
});

delegate(el.filters, "[data-status]", "click", (_event, button) => {
  if (button.dataset.status === state.status) return;

  for (const item of el.filters.querySelectorAll("[data-status]")) {
    item.classList.toggle("segmented__item--on", item === button);
  }

  state.status = button.dataset.status;
  reloadFromFirstPage();
});

el.refresh.addEventListener("click", () => void refresh());

el.prev.addEventListener("click", () => {
  if (state.page > 1) void refresh(state.page - 1);
});

el.next.addEventListener("click", () => {
  if (!el.next.disabled) void refresh(state.page + 1);
});

/* ---------- 启动 ---------- */

const savedToken = sessionStorage.getItem(TOKEN_KEY);
if (savedToken) {
  state.token = savedToken;
  void refresh(1);
} else {
  showGate();
}
