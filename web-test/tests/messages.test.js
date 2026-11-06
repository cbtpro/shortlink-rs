import assert from "node:assert/strict";
import test from "node:test";

import { ApiError } from "../../web/assets/js/core/http.js";
import { humanize } from "../../web/assets/js/core/messages.js";

/**
 * 这张表是全站唯一的一份「服务端文案 → 人话」映射：首页、登录页、后台都读它。
 * 它错了不会崩，只会让同一个故障在不同页面说成不同的话。
 */

test("命中表里的文案原样翻译", () => {
  assert.equal(humanize(new ApiError(409, "short code already exists")), "这个短码已被占用，换一个试试");
  assert.equal(humanize(new ApiError(403, "account is disabled")), "账号已被停用，请联系管理员");
});

test("表里没有的按状态码兜底", () => {
  assert.equal(humanize(new ApiError(0, "anything")), "连不上服务，请确认服务已启动");
  assert.equal(humanize(new ApiError(500, "internal error")), "服务端出错了，请稍后重试");
  assert.equal(humanize(new ApiError(409, "conflict")), "这个短码已被占用，换一个试试");
});

test("都不命中就把服务端原话交出来，别吞成一句「出错了」", () => {
  // 未知的 400 往往是校验规则变了而前端还没跟上，原文比「出错了」有用得多
  assert.equal(humanize(new ApiError(400, "custom code is reserved")), "custom code is reserved");
});

test("不是 ApiError 也能拿到一句话", () => {
  assert.equal(humanize(new TypeError("x is not a function")), "x is not a function");
  assert.equal(humanize(null), "出错了，请稍后重试");
  assert.equal(humanize(undefined), "出错了，请稍后重试");
});
