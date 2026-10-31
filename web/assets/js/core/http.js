export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** 发请求并解析 JSON。2xx 之外的响应统一抛 ApiError，网络故障 status 记 0。 */
export async function request(path, init = {}) {
  let response;
  try {
    response = await fetch(path, init);
  } catch {
    throw new ApiError(0, "无法连接服务，请确认服务已启动");
  }

  if (response.status === 204) return null;

  let body = null;
  try {
    body = await response.json();
  } catch {
    body = null;
  }

  if (!response.ok) {
    const message = body?.message || `请求失败（HTTP ${response.status}）`;
    throw new ApiError(response.status, message);
  }
  return body;
}

export function jsonRequest(payload, init = {}) {
  return {
    method: "POST",
    ...init,
    headers: { "Content-Type": "application/json", ...(init.headers || {}) },
    body: JSON.stringify(payload),
  };
}
