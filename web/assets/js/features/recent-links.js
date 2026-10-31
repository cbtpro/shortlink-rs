import { API_BASE } from "../core/api-base.js";
import { request } from "../core/http.js";

/**
 * 「最近创建」的读写。
 *
 * 记录存在服务端，按浏览器匿名 cookie 归属：换设备看不到，清了 cookie 也看不到。
 * 同源请求默认会带上那个 cookie，这里不用额外配置。
 */

export async function list() {
  const body = await request(`${API_BASE}/recent`);
  return Array.isArray(body?.items) ? body.items : [];
}

export async function forget(code) {
  await request(`${API_BASE}/recent/${encodeURIComponent(code)}`, { method: "DELETE" });
}

export async function clear() {
  await request(`${API_BASE}/recent`, { method: "DELETE" });
}
