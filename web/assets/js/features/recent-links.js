import { apiBase } from "../core/api-base.js";
import { request } from "../core/http.js";

/**
 * 首页列表的读写。
 *
 * 同一组接口服务两种身份，行为由服务端按会话决定：
 * - 登录：返回用户名下的链接，删除等于让短链失效；
 * - 未登录：返回本浏览器匿名 cookie 记录的创建，删除只是移出清单。
 *
 * 前端不需要传身份参数 —— 会话在 HttpOnly cookie 里，同源请求会自动带上。
 */

export async function list() {
  const body = await request(`${apiBase()}/recent`);
  return Array.isArray(body?.items) ? body.items : [];
}

export async function forget(code) {
  await request(`${apiBase()}/recent/${encodeURIComponent(code)}`, { method: "DELETE" });
}

export async function clear() {
  await request(`${apiBase()}/recent`, { method: "DELETE" });
}
