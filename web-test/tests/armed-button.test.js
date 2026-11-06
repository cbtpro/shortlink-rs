import assert from "node:assert/strict";
import test, { after } from "node:test";

import { armedButton } from "../../web/assets/js/features/armed-button.js";
import { flush, installDom, sleep } from "./helpers.js";

const dom = installDom();
after(() => dom.window.close());

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

/** 建一个按钮；`calls` 里每成功执行一次 onClick 就多一个元素 */
function makeButton(overrides = {}) {
  const calls = [];
  const button = armedButton({
    label: "删除",
    armedLabel: "确认删除",
    className: "row-action row-action--danger",
    onClick: async () => {
      calls.push("clicked");
    },
    ...overrides,
  });
  document.body.append(button);
  return { button, calls };
}

test("第一次点击只进入待确认，不发请求", () => {
  const { button, calls } = makeButton();

  button.click();

  assert.equal(calls.length, 0, "第一次点击不该调用 onClick");
  assert.equal(button.textContent, "确认删除");
  assert.ok(button.classList.contains("row-action--armed"));
  assert.ok(button.classList.contains("row-action--danger"), "基础 class 不能被覆盖掉");
});

test("第二次点击才真的执行，并在请求期间禁用", async () => {
  const gate = deferred();
  const { button, calls } = makeButton({
    onClick: () => gate.promise.then(() => calls.push("clicked")),
  });

  button.click();
  button.click();

  assert.equal(button.disabled, true, "请求还没回来，按钮应当已经禁用");
  assert.equal(button.textContent, "删除", "发请求时立刻复原文案，别停在「确认…」");

  gate.resolve();
  await flush();
  await flush();

  assert.equal(calls.length, 1);
  assert.equal(button.disabled, false, "请求结束后必须恢复可用");
});

test("请求进行中连点不会再发一次", async () => {
  const gate = deferred();
  const { button, calls } = makeButton({
    onClick: () => gate.promise.then(() => calls.push("clicked")),
  });

  button.click();
  button.click();
  button.click();
  button.click();

  gate.resolve();
  await flush();
  await flush();

  assert.equal(calls.length, 1, "重复提交在这里发生的话，删除会一路 404，停用会反复打同一个接口");
});

test("超时没人确认就自己复原", async () => {
  const { button, calls } = makeButton({ timeout: 20 });

  button.click();
  assert.equal(button.textContent, "确认删除");

  await sleep(120);

  assert.equal(button.textContent, "删除");
  assert.equal(button.classList.contains("row-action--armed"), false);
  assert.equal(calls.length, 0);

  // 复原之后再点，仍然只是「待确认」——不能因为超时过就跳过确认
  button.click();
  assert.equal(button.textContent, "确认删除");
  assert.equal(calls.length, 0);
});

test("第二次点击后计时器不再作废按钮状态", async () => {
  const { button, calls } = makeButton({ timeout: 20 });

  button.click();
  button.click();
  await sleep(120);

  assert.equal(calls.length, 1);
  assert.equal(button.textContent, "删除");
  assert.equal(button.disabled, false, "清除计时器时不能连按钮的可用状态一起弄丢");
});

test("title 与自定义 class 会带到按钮上", () => {
  const { button } = makeButton({ className: "row-action", title: "生成一次性口令" });

  assert.equal(button.className, "row-action");
  assert.equal(button.getAttribute("type"), "button", "别在 form 里变成提交按钮");
  assert.equal(button.title, "生成一次性口令");
});
