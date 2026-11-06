import { $, setBusy } from "../core/dom.js";
import { applySiteName } from "../core/config.js";
import { humanize } from "../core/messages.js";
import * as session from "../core/session.js";

const el = {
  form: $("auth-form"),
  email: $("email"),
  password: $("password"),
  displayName: $("display-name"),
  nameField: $("name-field"),
  lead: $("auth-lead"),
  hint: $("password-hint"),
  error: $("auth-error"),
  submit: $("auth-submit"),
  submitLabel: $("submit-label"),
  tabs: Array.from(document.querySelectorAll("[data-mode]")),
};

let mode = "login";

/**
 * 登录后回到原来要去的页面。
 *
 * 只接受站内绝对路径：`//evil.com` 和 `https://evil.com` 都会被浏览器当成外站，
 * 开放重定向就是这么来的。
 */
function nextLocation() {
  const target = new URLSearchParams(location.search).get("next");

  if (!target || !target.startsWith("/") || target.startsWith("//")) return "/";
  return target;
}

function applyMode(next) {
  mode = next === "register" ? "register" : "login";
  const registering = mode === "register";

  for (const tab of el.tabs) {
    tab.classList.toggle("tab--active", tab.dataset.mode === mode);
    tab.setAttribute("aria-selected", String(tab.dataset.mode === mode));
  }

  el.nameField.hidden = !registering;
  el.submitLabel.textContent = registering ? "注册并登录" : "登录";
  el.lead.textContent = registering
    ? "注册后，新建的短链会挂在你的账号下。"
    : "登录后，新建的短链会挂在你的账号下。";
  el.password.autocomplete = registering ? "new-password" : "current-password";
  el.hint.textContent = registering
    ? "至少 8 位，区分大小写。"
    : "区分大小写。";

  document.title = registering ? "注册" : "登录";
  applySiteName();
  clearError();
}

function showError(message) {
  el.error.textContent = message;
  el.error.hidden = false;
}

function clearError() {
  el.error.hidden = true;
  el.error.textContent = "";
  el.email.removeAttribute("aria-invalid");
  el.password.removeAttribute("aria-invalid");
}

async function onSubmit(event) {
  event.preventDefault();
  clearError();

  const email = el.email.value.trim();
  const password = el.password.value;

  if (!email) {
    el.email.setAttribute("aria-invalid", "true");
    el.email.focus();
    showError("请填写邮箱");
    return;
  }

  // 登录时不校验长度：老账号的口令规则可能和现在的不同，交给服务端判定
  if (mode === "register" && password.length < 8) {
    el.password.setAttribute("aria-invalid", "true");
    el.password.focus();
    showError("密码至少 8 位");
    return;
  }

  setBusy(el.submit, true);
  try {
    if (mode === "register") {
      await session.register({
        email,
        password,
        displayName: el.displayName.value.trim(),
      });
    } else {
      await session.login(email, password);
    }

    location.assign(nextLocation());
  } catch (error) {
    showError(humanize(error));
    setBusy(el.submit, false);
  }
}

for (const tab of el.tabs) {
  tab.addEventListener("click", () => {
    applyMode(tab.dataset.mode);
    // 切到注册时把焦点送到昵称，省一次点击
    if (mode === "register") el.displayName.focus({ preventScroll: true });
  });
}

el.form.addEventListener("submit", onSubmit);

const params = new URLSearchParams(location.search);
applyMode(params.get("mode"));

// 已经登录的人不该停在登录页
session.currentUser().then((user) => {
  if (user) location.replace(nextLocation());
});
