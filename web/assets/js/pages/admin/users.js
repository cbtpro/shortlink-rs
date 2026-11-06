import { $, h, setBusy } from "../../core/dom.js";
import { formatNumber } from "../../core/format.js";
import { toast } from "../../core/toast.js";
import { armedButton } from "../../features/armed-button.js";

/**
 * 用户面板（仅管理员）。
 *
 * 这里能点到的每个动作，服务端都会再判一次角色与目标：
 * 前端隐藏按钮只是别让人对着按不动的按钮发愣，不是权限。
 */

const PAGE_SIZE = 20;
const SEARCH_DEBOUNCE_MS = 300;

export function createUsersPanel({ api, report, currentUserId, onViewLinks, onTemporaryPassword }) {
  const el = {
    search: $("user-search"),
    refresh: $("user-refresh"),
    error: $("users-error"),
    rows: $("user-rows"),
    empty: $("users-empty"),
    pagerInfo: $("user-pager-info"),
    prev: $("user-prev"),
    next: $("user-next"),
  };

  const state = { page: 1, keyword: "" };

  /* ---------- 错误 ---------- */

  function showError(message) {
    el.error.textContent = message;
    el.error.hidden = false;
  }

  function clearError() {
    el.error.hidden = true;
    el.error.textContent = "";
  }

  /* ---------- 操作 ---------- */

  async function resetPassword(user) {
    try {
      const credential = await api.resetPassword(user.id);
      onTemporaryPassword(credential);
      await refresh(state.page);
    } catch (error) {
      report(error, showError);
    }
  }

  async function setStatus(user, status) {
    try {
      await api.setUserStatus(user.id, status);
      toast(status === "active" ? `已启用 ${user.email}` : `已停用 ${user.email}`);
      await refresh(state.page);
    } catch (error) {
      report(error, showError);
    }
  }

  /* ---------- 表格 ---------- */

  function buildRow(user) {
    // 管理员不能停用/重置自己，也不能动另一个管理员 —— 服务端会拒绝，
    // 这里干脆不画出按钮，同时说明为什么
    const isSelf = user.id === currentUserId();
    const manageable = !isSelf && user.role !== "admin";
    const active = user.status === "active";

    return h(
      "tr",
      {},
      h(
        "td",
        { class: "cell-account" },
        h("span", { class: "cell-account__email", text: user.email }),
        user.display_name
          ? h("span", { class: "cell-account__name", text: user.display_name })
          : null,
        user.must_change_password
          ? h("span", { class: "badge badge--warn", text: "待改密" })
          : null
      ),
      h(
        "td",
        {},
        user.role === "admin"
          ? h("span", { class: "badge badge--admin", text: "管理员" })
          : h("span", { class: "cell-muted", text: "普通用户" })
      ),
      h(
        "td",
        {},
        active
          ? h("span", { class: "badge badge--active", text: "正常" })
          : h("span", { class: "badge badge--danger", text: "已停用" })
      ),
      h("td", { class: "table__num", text: formatNumber(user.link_count) }),
      h("td", { class: "cell-time", text: user.created_at }),
      h(
        "td",
        { class: "cell-actions" },
        h("button", {
          type: "button",
          class: "row-action",
          text: "查看链接",
          onClick: () => onViewLinks({ id: user.id, email: user.email, display_name: user.display_name }),
        }),
        manageable
          ? armedButton({
              label: "重置密码",
              armedLabel: "确认重置",
              title: "生成一次性口令，对方登录后必须先改密码",
              onClick: () => resetPassword(user),
            })
          : null,
        manageable
          ? active
            ? armedButton({
                label: "停用",
                armedLabel: "确认停用",
                className: "row-action row-action--danger",
                title: "停用后对方的会话立即失效",
                onClick: () => setStatus(user, "disabled"),
              })
            : h("button", {
                type: "button",
                class: "row-action",
                text: "启用",
                onClick: () => setStatus(user, "active"),
              })
          : h("span", { class: "row-note", text: isSelf ? "本人" : "管理员" })
      )
    );
  }

  function renderRows(page) {
    el.rows.replaceChildren(...page.items.map(buildRow));
    el.empty.hidden = page.items.length > 0;
  }

  function renderPager(page) {
    const totalPages = Math.max(1, page.total_pages);

    el.pagerInfo.textContent = page.total
      ? `共 ${formatNumber(page.total)} 个账号 · 第 ${page.page} / ${totalPages} 页`
      : "";
    el.prev.disabled = page.page <= 1;
    el.next.disabled = page.page >= totalPages;

    return totalPages;
  }

  /* ---------- 加载 ---------- */

  function listQuery(page) {
    const params = new URLSearchParams({ page: String(page), size: String(PAGE_SIZE) });
    if (state.keyword) params.set("q", state.keyword);
    return params.toString();
  }

  async function refresh(page = state.page) {
    setBusy(el.refresh, true);
    clearError();

    try {
      const result = await api.listUsers(listQuery(page));
      const totalPages = renderPager(result);

      // 搜到一半条件变严，当前页可能已经越界
      if (page > totalPages) {
        await refresh(totalPages);
        return;
      }

      state.page = page;
      renderRows(result);
    } catch (error) {
      report(error, showError);
    } finally {
      setBusy(el.refresh, false);
    }
  }

  /* ---------- 交互 ---------- */

  let searchTimer = 0;
  el.search.addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      state.keyword = el.search.value.trim();
      state.page = 1;
      void refresh(1);
    }, SEARCH_DEBOUNCE_MS);
  });

  el.refresh.addEventListener("click", () => void refresh());

  el.prev.addEventListener("click", () => {
    if (state.page > 1) void refresh(state.page - 1);
  });

  el.next.addEventListener("click", () => {
    if (!el.next.disabled) void refresh(state.page + 1);
  });

  return { show: () => void refresh(state.page) };
}
