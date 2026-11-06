import { $, setBusy } from "../../core/dom.js";
import { humanize } from "../../core/messages.js";
import { toast } from "../../core/toast.js";
import * as session from "../../core/session.js";

/**
 * 强制改密面板。
 *
 * 用一次性口令登录后，服务端只放行 `POST /auth/password`，其他接口全是 403，
 * 所以这个面板不是「建议改密」，而是唯一的出路。
 * 改完旧会话全部作废，服务端给当前这台设备重签了一枚 cookie，不用再登一次。
 */

const MIN_PASSWORD_LEN = 8;

export function createPasswordPanel({ onDone }) {
  const el = {
    form: $("password-form"),
    current: $("current-password"),
    next: $("new-password"),
    confirm: $("confirm-password"),
    error: $("password-error"),
    submit: $("password-submit"),
  };

  function showError(message, field) {
    el.error.textContent = message;
    el.error.hidden = false;

    if (field) {
      field.setAttribute("aria-invalid", "true");
      field.focus();
    }
  }

  function clearError() {
    el.error.hidden = true;
    el.error.textContent = "";
    for (const field of [el.current, el.next, el.confirm]) {
      field.removeAttribute("aria-invalid");
    }
  }

  el.form.addEventListener("submit", async (event) => {
    event.preventDefault();
    clearError();

    const current = el.current.value;
    const next = el.next.value;

    // 与服务端同一条规则，先在这里挡一次是为了把话说在字段旁边；
    // 真正的判定权在服务端，绕过页面直接调接口同样过不去
    if (!current) return showError("请输入当前口令", el.current);
    if (next.length < MIN_PASSWORD_LEN) {
      return showError(`新密码至少 ${MIN_PASSWORD_LEN} 位`, el.next);
    }
    if (next === current) return showError("新密码不能和当前口令一样", el.next);
    if (next !== el.confirm.value) return showError("两次输入的新密码不一致", el.confirm);

    setBusy(el.submit, true);
    try {
      const user = await session.changePassword(current, next);
      el.form.reset();
      toast("密码已更新");
      onDone(user);
    } catch (error) {
      showError(humanize(error));
    } finally {
      setBusy(el.submit, false);
    }
  });

  return {
    show() {
      clearError();
      el.current.focus();
    },
  };
}
