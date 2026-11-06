import { apiBase } from "./api-base.js";
import { jsonRequest, request } from "./http.js";

/**
 * 登录状态。
 *
 * 会话靠 HttpOnly cookie 维持，脚本读不到令牌，也就不可能把它存进 localStorage
 * 再被 XSS 取走。这里缓存的是「当前用户是谁」，只用于渲染，不承担鉴权。
 */

let loaded = false;
let current = null;

/** 当前用户；未登录返回 null。同一次页面生命周期只问服务端一次。 */
export async function currentUser() {
  if (loaded) return current;

  try {
    const body = await request(`${apiBase()}/auth/me`);
    current = body?.user ?? null;
  } catch {
    // 读不到就按未登录处理：页首的登录链接不该因为一次网络抖动而报错
    current = null;
  }

  loaded = true;
  return current;
}

export async function login(email, password) {
  const body = await request(
    `${apiBase()}/auth/login`,
    jsonRequest({ email, password })
  );
  return remember(body);
}

export async function register({ email, password, displayName }) {
  const payload = { email, password };
  if (displayName) payload.display_name = displayName;

  const body = await request(
    `${apiBase()}/auth/register`,
    jsonRequest(payload)
  );
  return remember(body);
}

export async function logout() {
  await request(`${apiBase()}/auth/logout`, { method: "POST" });
  current = null;
  loaded = true;
}

/**
 * 改密。
 *
 * 服务端会把该账号所有会话作废，同时给这次请求重新签一枚 cookie ——
 * 所以改完密码当前这台设备还在线，别处登录过的都下线了。
 */
export async function changePassword(currentPassword, newPassword) {
  const body = await request(
    `${apiBase()}/auth/password`,
    jsonRequest({ current_password: currentPassword, new_password: newPassword })
  );
  return remember(body);
}

function remember(body) {
  current = body?.user ?? null;
  loaded = true;
  return current;
}
