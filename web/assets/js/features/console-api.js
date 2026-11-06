import { apiBase } from "../core/api-base.js";
import { ApiError, jsonRequest, request } from "../core/http.js";
import { humanize } from "../core/messages.js";

/**
 * 后台接口。
 *
 * 鉴权沿用登录会话那枚 HttpOnly cookie，这里不碰任何令牌 ——
 * 页面脚本读不到凭据，也就不会有人把它塞进 localStorage 再被 XSS 捞走。
 * 「能看多少」由服务端按角色判定，前端只决定画哪几块。
 */

/** 会话已经不能用了：没登录（401）。调用方应退回未登录视图。 */
export class AuthError extends Error {}

/** 账号被停用（403）。 */
export class AccountDisabledError extends Error {}

/** 临时口令未更换（403）。除了改密接口，其他一律被挡。 */
export class PasswordChangeRequiredError extends Error {}

export function createConsoleClient() {
  async function call(path, init = {}) {
    try {
      return await request(`${apiBase()}${path}`, init);
    } catch (error) {
      throw translate(error);
    }
  }

  return {
    overview: () => call("/console/api/overview"),

    listLinks: (query) => call(`/console/api/links?${query}`),

    deleteLink: (code) =>
      call(`/console/api/links/${encodeURIComponent(code)}`, { method: "DELETE" }),

    listUsers: (query) => call(`/admin/api/users?${query}`),

    /** 返回一次性口令，只在这一刻可见 */
    resetPassword: (id) => call(`/admin/api/users/${id}/password`, { method: "POST" }),

    setUserStatus: (id, status) =>
      call(`/admin/api/users/${id}/status`, jsonRequest({ status })),
  };
}

/** 把服务端错误分拣成调用方需要区分的几类，其余统一成一句人话。 */
function translate(error) {
  if (!(error instanceof ApiError)) return error;

  const text = humanize(error);

  if (error.status === 401) return new AuthError(text || "登录已过期，请重新登录");
  if (error.message === "account is disabled") return new AccountDisabledError(text);
  if (error.message === "password must be changed before continuing") {
    return new PasswordChangeRequiredError(text);
  }
  return new Error(text);
}
