import { $, setBusy } from "../core/dom.js";
import { applySiteName } from "../core/config.js";
import { ApiError } from "../core/http.js";
import * as session from "../core/session.js";

/* 服务端文案 → 中文。没命中的按状态码兜底。 */
const MESSAGES = {
  "email is not a valid address": "邮箱格式不对",
  "password must be at least 8 characters": "密码至少 8 位",
  "password must be at most 128 characters": "密码最多 128 位",
  "password must not be blank": "密码不能为空白",
  "display name must be at most 32 characters": "昵称最多 32 个字",
  "display name must not contain control characters": "昵称不能包含换行或制表符",
  "email is already registered": "这个邮箱已经注册过了，直接登录吧",
  "email or password is incorrect": "邮箱或密码不正确",
  "request body is not valid": "提交的数据格式不对，刷新页面重试",
};

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

function humanize(error) {
  if (!(error instanceof ApiError)) return error.message;
  if (MESSAGES[error.message]) return MESSAGES[error.message];
  if (error.status === 0) return "连不上服务，请确认服务已启动";
  if (error.status >= 500) return "服务端出错了，请稍后重试";
  return error.message;
}

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
