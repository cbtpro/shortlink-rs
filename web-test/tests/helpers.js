import { JSDOM } from "jsdom";

/**
 * 这些模块是写给浏览器的：`document` / `location` / `__APP_CONFIG__` 都是全局量。
 * 测试不加打包步骤、也不改被测代码 —— 把 jsdom 的窗口装到 globalThis 上，
 * 模块就能原样跑，import 路径与线上完全一致。
 */
export function installDom({ url = "http://127.0.0.1:9981/admin", config = {} } = {}) {
  const dom = new JSDOM("<!doctype html><html><head></head><body></body></html>", { url });

  globalThis.window = dom.window;
  globalThis.document = dom.window.document;
  globalThis.location = dom.window.location;
  // 服务端 /app-config.js 的替身；config.js 首次读取后自己缓存，所以每个测试文件一份
  globalThis.__APP_CONFIG__ = config;

  return dom;
}

/**
 * fetch 替身。`handler` 返回响应对象；返回 Error 实例表示网络层故障。
 * 拿到的数组就是「实际发出了哪些请求」，断言路径、方法、请求体都看它。
 */
export function stubFetch(handler) {
  const calls = [];
  const original = globalThis.fetch;

  globalThis.fetch = async (input, init = {}) => {
    const url = String(input);
    calls.push({ url, init });

    const reply = await handler(url, init);
    if (reply instanceof Error) throw reply;
    return reply;
  };

  calls.restore = () => {
    globalThis.fetch = original;
  };
  return calls;
}

/** 构造一个 JSON 响应；2xx 之外按 http.js 的约定读 `message` 字段。 */
export function json(status, body) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  };
}

/** 204 的响应体是空的，`json()` 会抛 —— 真实 Response 也是这样。 */
export const noContent = {
  ok: true,
  status: 204,
  json: async () => {
    throw new SyntaxError("Unexpected end of JSON input");
  },
};

/** 等 `ms` 毫秒。测试里用它跨越 setTimeout，比假计时器少一层心智负担。 */
export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** 把一连串微任务跑完（两个 await 足够让 async 监听器走到下一次挂起）。 */
export const flush = () => new Promise((resolve) => setImmediate(resolve));
