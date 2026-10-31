import { API_BASE } from "../core/api-base.js";
import { ApiError, request } from "../core/http.js";

/** 口令无效，调用方应退回登录状态 */
export class AuthError extends Error {}

/** 服务端没配 ADMIN_TOKEN，所有管理接口都是 404 */
export class DisabledError extends Error {}

const MESSAGES = {
  "short link not found": "该短链已不存在，列表可能已过期",
  "internal server error": "服务端出错了，请稍后重试",
};

export function createAdminClient(readToken) {
  async function call(path, init = {}) {
    try {
      return await request(`${API_BASE}/admin/api${path}`, {
        ...init,
        headers: { Authorization: `Bearer ${readToken()}`, ...(init.headers || {}) },
      });
    } catch (error) {
      if (!(error instanceof ApiError)) throw error;
      if (error.status === 401) throw new AuthError("口令无效或已失效");
      // 删不存在的短码也是 404，只能靠文案区分「后台未启用」
      if (error.status === 404 && error.message === "admin api is disabled") {
        throw new DisabledError(error.message);
      }
      throw new Error(MESSAGES[error.message] || error.message);
    }
  }

  return {
    overview: () => call("/overview"),
    list: (query) => call(`/links?${query}`),
    remove: (code) => call(`/links/${encodeURIComponent(code)}`, { method: "DELETE" }),
  };
}
