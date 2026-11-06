import { $, delegate, setBusy } from "../../core/dom.js";
import { applySiteName } from "../../core/config.js";
import { copyText } from "../../core/clipboard.js";
import { toast } from "../../core/toast.js";
import * as session from "../../core/session.js";
import {
  AccountDisabledError,
  AuthError,
  PasswordChangeRequiredError,
  createConsoleClient,
} from "../../features/console-api.js";
import { createLinksPanel } from "./links.js";
import { createUsersPanel } from "./users.js";
import { createPasswordPanel } from "./password.js";

/**
 * 后台页面。
 *
 * 这一层只做编排：谁能进来看什么由服务端按角色决定，这里负责把
 * 「未登录 / 待改密 / 已登录」三种状态对应的视图摆出来，并把两个面板
 * 的报错收拢到一处 —— 会话类错误必须整套退出，不能只在某个表格里显示一行红字。
 */

const LOGIN_PATH = "/login?next=/admin";

const api = createConsoleClient();
const state = { user: null, isAdmin: false, tab: "links" };

const el = {
  gate: $("gate"),
  gateMessage: $("gate-message"),
  passwordGate: $("password-gate"),
  console: $("console"),
  topbarActions: $("topbar-actions"),
  topbarUser: $("topbar-user"),
  topbarRole: $("topbar-role"),
  signOut: $("sign-out"),
  tabUsers: $("tab-users"),
  panelLinks: $("panel-links"),
  panelUsers: $("panel-users"),
  colOwner: $("col-owner"),
  statTotalLabel: $("stat-total-label"),
  statVisitsLabel: $("stat-visits-label"),
  credential: $("credential"),
  credentialEmail: $("credential-email"),
  credentialPassword: $("credential-password"),
  credentialCopy: $("credential-copy"),
  credentialClose: $("credential-close"),
};

/* ---------- 视图切换 ---------- */

function showGate(message) {
  el.gate.hidden = false;
  el.gateMessage.textContent = message || "这个页面要登录后才能看。";
  el.passwordGate.hidden = true;
  el.console.hidden = true;
  el.topbarActions.hidden = true;
}

function showPasswordGate() {
  el.gate.hidden = true;
  el.passwordGate.hidden = false;
  el.console.hidden = true;
  // 停在改密页时也要能退出：万一登错了账号，或者口令根本不对
  el.topbarActions.hidden = false;
  password.show();
}

function showConsole() {
  el.gate.hidden = true;
  el.passwordGate.hidden = true;
  el.console.hidden = false;
  el.topbarActions.hidden = false;
}

/* ---------- 报错收拢 ---------- */

/**
 * 面板统一的出错出口。
 *
 * 面板只关心「我这块显示什么」，会话过期、账号被停用这类要换整页的事
 * 交给这里判断 —— 否则每个面板都得重写一遍同样的分支，漏一个就是白屏。
 */
function report(error, showInline) {
  if (error instanceof AuthError || error instanceof AccountDisabledError) {
    showGate(error.message);
    return;
  }
  if (error instanceof PasswordChangeRequiredError) {
    showPasswordGate();
    return;
  }
  showInline(error.message);
}

/* ---------- 一次性口令 ---------- */

function revealCredential(credential) {
  el.credentialEmail.textContent = credential.email;
  el.credentialPassword.textContent = credential.password;
  el.credential.hidden = false;
  el.credentialClose.focus();
}

function hideCredential() {
  el.credential.hidden = true;
  el.credentialPassword.textContent = "";
}

/* ---------- 标签页 ---------- */

function applyTab(tab) {
  // 普通用户没有「用户」页，任何来源的请求都退回短链页
  state.tab = tab === "users" && state.isAdmin ? "users" : "links";

  for (const button of document.querySelectorAll("[data-tab]")) {
    const active = button.dataset.tab === state.tab;
    button.classList.toggle("tab--active", active);
    button.setAttribute("aria-selected", String(active));
  }

  el.panelLinks.hidden = state.tab !== "links";
  el.panelUsers.hidden = state.tab !== "users";

  if (state.tab === "users") users.show();
  else links.show();
}

/* ---------- 面板 ---------- */

const links = createLinksPanel({ api, isAdmin: () => state.isAdmin, report });

const users = createUsersPanel({
  api,
  report,
  currentUserId: () => state.user?.id,
  onViewLinks: (owner) => {
    links.setOwner(owner);
    applyTab("links");
  },
  onTemporaryPassword: revealCredential,
});

const password = createPasswordPanel({
  onDone: (user) => {
    if (!user) {
      // 响应里没带用户信息说明服务端契约变了，重新走一遍启动流程最稳
      location.reload();
      return;
    }
    applyUser(user);
  },
});

/* ---------- 身份 ---------- */

function applyUser(user) {
  state.user = user;
  state.isAdmin = user.role === "admin";

  el.topbarUser.textContent = user.display_name || user.email;
  el.topbarUser.title = user.email;
  el.topbarRole.hidden = !state.isAdmin;

  // 管理员才有用户页和归属列；普通用户看不到别人的东西，这两处都是空的
  el.tabUsers.hidden = !state.isAdmin;
  el.colOwner.hidden = !state.isAdmin;
  el.statTotalLabel.textContent = state.isAdmin ? "总链接" : "我的短链";
  el.statVisitsLabel.textContent = state.isAdmin ? "总访问" : "我的访问";

  if (user.must_change_password) {
    showPasswordGate();
    return;
  }

  showConsole();
  applyTab("links");
}

/* ---------- 事件 ---------- */

delegate(document, "[data-tab]", "click", (_event, button) => applyTab(button.dataset.tab));

el.signOut.addEventListener("click", async () => {
  setBusy(el.signOut, true);
  try {
    await session.logout();
    location.assign(LOGIN_PATH);
  } catch (error) {
    toast(error.message);
    setBusy(el.signOut, false);
  }
});

el.credentialCopy.addEventListener("click", async () => {
  const copied = await copyText(el.credentialPassword.textContent);
  toast(copied ? "已复制到剪贴板" : "复制失败，请手动选择");
});

el.credentialClose.addEventListener("click", hideCredential);

// 点背景关掉；卡片内部的点击不冒到这一层
el.credential.addEventListener("click", (event) => {
  if (event.target === el.credential) hideCredential();
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !el.credential.hidden) hideCredential();
});

/* ---------- 启动 ---------- */

applySiteName();

const currentUser = await session.currentUser();

if (currentUser) {
  applyUser(currentUser);
} else {
  // 没会话就直接去登录，别让人对着一个空后台猜自己是不是点错了
  location.replace(LOGIN_PATH);
}
