import { $, delegate, h, openInNewTab, setBusy } from "../../core/dom.js";
import { formatNumber } from "../../core/format.js";
import { copyText } from "../../core/clipboard.js";
import { toast } from "../../core/toast.js";
import { armedButton } from "../../features/armed-button.js";

/**
 * 短链面板：概览 + 列表 + 删除。
 *
 * 同一套接口既服务普通用户也服务管理员，差别只有两点：
 * - 归属列只在管理员视角下存在；
 * - 管理员可以按归属筛选（从用户页点进来）。
 * 其余「能看多少」完全由服务端决定，前端不参与判断。
 */

const PAGE_SIZE = 20;
const SEARCH_DEBOUNCE_MS = 300;

const STATE_LABEL = {
  active: "有效",
  expired: "已过期",
  exhausted: "已达上限",
};

export function createLinksPanel({ api, isAdmin, report }) {
  const el = {
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
    ownerFilter: $("owner-filter"),
    ownerName: $("owner-filter-name"),
    ownerClear: $("owner-filter-clear"),
  };

  const state = { page: 1, status: "all", keyword: "", owner: null };

  /* ---------- 表格 ---------- */

  function shortUrlOf(code) {
    return new URL(`/${code}`, location.origin).toString();
  }

  async function copyLink(url) {
    const copied = await copyText(url);
    toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
  }

  function buildRow(item) {
    const shortUrl = shortUrlOf(item.code);

    return h(
      "tr",
      {},
      h(
        "td",
        { class: "cell-code" },
        h("a", {
          href: shortUrl,
          target: "_blank",
          rel: "noopener noreferrer",
          onClick: openInNewTab,
          text: `/${item.code}`,
        }),
        item.has_password ? h("span", { class: "badge badge--locked", text: "密码" }) : null
      ),
      // 归属列只给管理员开：普通用户看到的每一条都是自己的，写了也是废话
      isAdmin()
        ? h("td", {
            class: "cell-owner",
            title: item.owner ? item.owner.email : "",
            text: item.owner ? item.owner.display_name || item.owner.email : "匿名",
          })
        : null,
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
        // 用真链接而不是 window.open：新标签页是浏览器的默认行为，不会被弹窗拦截器吃掉，
        // 也支持中键 / 右键「在新标签页中打开」。
        h("a", {
          class: "row-action",
          href: shortUrl,
          target: "_blank",
          rel: "noopener noreferrer",
          onClick: openInNewTab,
          text: "打开",
        }),
        armedButton({
          label: "删除",
          armedLabel: "确认删除",
          className: "row-action row-action--danger",
          onClick: async () => {
            try {
              await api.deleteLink(item.code);
              toast(`已删除 /${item.code}`);
              await refresh(state.page);
            } catch (error) {
              report(error, showBoardError);
            }
          },
        })
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

  function showBoardError(message) {
    el.error.textContent = message;
    el.error.hidden = false;
  }

  function clearBoardError() {
    el.error.hidden = true;
    el.error.textContent = "";
  }

  /* ---------- 加载 ---------- */

  function listQuery(page) {
    const params = new URLSearchParams({
      page: String(page),
      size: String(PAGE_SIZE),
      status: state.status,
    });
    if (state.keyword) params.set("q", state.keyword);
    if (state.owner) params.set("owner", String(state.owner.id));
    return params.toString();
  }

  /** 取概览与当前页。返回总页数，供调用方判断是否越界。 */
  async function load(page) {
    const [overview, result] = await Promise.all([api.overview(), api.listLinks(listQuery(page))]);

    renderStats(overview);
    renderRows(result);
    return renderPager(result);
  }

  async function refresh(page = state.page) {
    setBusy(el.refresh, true);
    clearBoardError();

    try {
      const totalPages = await load(page);
      // 删掉末页最后一条后当前页会越界，退一页重取
      if (page > totalPages) {
        state.page = totalPages;
        await refresh(totalPages);
        return;
      }
      state.page = page;
    } catch (error) {
      report(error, showBoardError);
    } finally {
      setBusy(el.refresh, false);
    }
  }

  /** 条件变了就回第一页 */
  function reloadFromFirstPage() {
    state.page = 1;
    void refresh(1);
  }

  /* ---------- 归属筛选 ---------- */

  function renderOwnerFilter() {
    el.ownerFilter.hidden = !state.owner;
    el.ownerName.textContent = state.owner ? state.owner.display_name || state.owner.email : "";
    el.ownerFilter.title = state.owner ? state.owner.email : "";
  }

  /**
   * 只看某个人的链接（管理员从用户页点进来时用）。
   *
   * 这里只改状态不拉数据：调用方随后切到本标签页时统一走 `show()`，
   * 免得一次点击打两个一样的请求。
   */
  function setOwner(owner) {
    state.owner = owner;
    state.page = 1;
    renderOwnerFilter();
  }

  /* ---------- 交互 ---------- */

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

  el.ownerClear.addEventListener("click", () => {
    setOwner(null);
    reloadFromFirstPage();
  });

  el.refresh.addEventListener("click", () => void refresh());

  el.prev.addEventListener("click", () => {
    if (state.page > 1) void refresh(state.page - 1);
  });

  el.next.addEventListener("click", () => {
    if (!el.next.disabled) void refresh(state.page + 1);
  });

  return {
    show: () => void refresh(state.page),
    setOwner,
  };
}
