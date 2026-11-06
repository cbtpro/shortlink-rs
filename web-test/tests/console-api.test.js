import assert from "node:assert/strict";
import test, { after, afterEach } from "node:test";

import {
  AccountDisabledError,
  AuthError,
  PasswordChangeRequiredError,
  createConsoleClient,
} from "../../web/assets/js/features/console-api.js";
import { installDom, json, noContent, stubFetch } from "./helpers.js";

// apiBase 用带尾斜杠的形式：反代到子路径时配置常常是人手写的，容易多一个 `/`
const dom = installDom({ config: { apiBase: "/sub/" } });
after(() => dom.window.close());

let calls = null;
afterEach(() => calls?.restore());

const client = createConsoleClient();

test("请求路径带上 apiBase，尾斜杠不会叠成 //", async () => {
  calls = stubFetch(() => json(200, { total_links: 0 }));
  await client.overview();

  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/sub/console/api/overview");
});

test("401 归为 AuthError：页面据此退回未登录视图", async () => {
  calls = stubFetch(() => json(401, { code: 401, message: "unauthorized" }));

  const error = await client.overview().then(
    () => assert.fail("401 不该成功"),
    (reason) => reason,
  );

  assert.ok(error instanceof AuthError, `实际类型是 ${error.constructor.name}`);
  assert.equal(error.message, "登录已过期，请重新登录");
});

test("被停用的是 403，归为 AccountDisabledError", async () => {
  calls = stubFetch(() => json(403, { code: 403, message: "account is disabled" }));

  const error = await client.overview().catch((reason) => reason);

  assert.ok(error instanceof AccountDisabledError);
  assert.equal(error.message, "账号已被停用，请联系管理员");
});

test("临时口令未更换也是 403，但必须与停用区分开", async () => {
  // 两者状态码相同、处置方式完全不同：停用要请管理员，待改密是去改密码。
  // 只按状态码分拣的话，会把「先去改个密码」的人引到「联系管理员」上。
  calls = stubFetch(() =>
    json(403, { code: 403, message: "password must be changed before continuing" }),
  );

  const error = await client.overview().catch((reason) => reason);

  assert.ok(error instanceof PasswordChangeRequiredError);
  assert.ok(!(error instanceof AccountDisabledError));
  assert.equal(error.message, "请先修改密码");
});

test("其余 403 不冒充停用：普通拒绝走普通错误", async () => {
  calls = stubFetch(() =>
    json(403, { code: 403, message: "administrator privileges required" }),
  );

  const error = await client.listUsers("page=1").catch((reason) => reason);

  assert.equal(error.constructor, Error, "只有这两种 403 才该被分类");
  assert.ok(!(error instanceof AccountDisabledError));
  assert.equal(error.message, "只有管理员能操作这个");
});

test("断网不能当成掉线：不清会话，只提示连不上", async () => {
  // 把「连不上」也译成 AuthError 的话，服务重启一次所有人都会被踢去登录页
  calls = stubFetch(() => new TypeError("fetch failed"));

  const error = await client.overview().catch((reason) => reason);

  assert.equal(error.constructor, Error);
  assert.equal(error.message, "连不上服务，请确认服务已启动");
});

test("删除短码时做 URL 编码，204 返回 null", async () => {
  calls = stubFetch(() => noContent);

  const result = await client.deleteLink("a/b c");

  assert.equal(result, null);
  assert.equal(calls[0].url, "/sub/console/api/links/a%2Fb%20c");
  assert.equal(calls[0].init.method, "DELETE");
});

test("启停账号发 JSON，字段名就是服务端要的 status", async () => {
  calls = stubFetch(() => json(204, null));

  await client.setUserStatus(12, "disabled");

  assert.equal(calls[0].url, "/sub/admin/api/users/12/status");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.headers["Content-Type"], "application/json");
  assert.deepEqual(JSON.parse(calls[0].init.body), { status: "disabled" });
});

test("重置密码返回的一次性口令原样透出，不在这里做人情加工", async () => {
  calls = stubFetch(() => json(200, { email: "u@example.com", password: "Temp-1234abcd" }));

  const credential = await client.resetPassword(12);

  assert.equal(calls[0].url, "/sub/admin/api/users/12/password");
  assert.equal(calls[0].init.method, "POST");
  assert.deepEqual(credential, { email: "u@example.com", password: "Temp-1234abcd" });
});

test("列表查询串原样接到路径后面", async () => {
  calls = stubFetch(() => json(200, { items: [], total: 0 }));

  await client.listLinks("page=2&size=20");
  await client.listUsers("page=1&size=20&q=ali");

  assert.equal(calls[0].url, "/sub/console/api/links?page=2&size=20");
  assert.equal(calls[1].url, "/sub/admin/api/users?page=1&size=20&q=ali");
});
