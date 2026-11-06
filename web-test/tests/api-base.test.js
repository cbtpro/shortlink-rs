import assert from "node:assert/strict";
import test, { after } from "node:test";

import { apiBase } from "../../web/assets/js/core/api-base.js";
import { installDom } from "./helpers.js";

/**
 * 接口前缀。
 *
 * 反代到子路径（`/sub/admin`）时，`apiBase` 必须算出 `/sub`，
 * 否则每个请求都会打到站根上 —— 页面能打开、数据全 404，看起来像后端坏了。
 */

// 没下发 apiBase 时才走推算，所以这里故意给一份空配置
const dom = installDom({ config: {}, url: "http://127.0.0.1:9981/sub/admin" });
after(() => dom.window.close());

/** 推算只看 `location.pathname`，换一个对象比换整份 DOM 省事 */
const setPath = (pathname) => {
  globalThis.location = { pathname };
};

test("从页面路径倒推目录", () => {
  setPath("/sub/admin");
  assert.equal(apiBase(), "/sub");

  setPath("/admin");
  assert.equal(apiBase(), "");
});

test("路径本身以斜杠结尾时不多留一个", () => {
  setPath("/sub/");
  assert.equal(apiBase(), "/sub");

  setPath("/");
  assert.equal(apiBase(), "");
});
