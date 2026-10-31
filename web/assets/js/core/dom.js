export const $ = (id) => document.getElementById(id);

/**
 * 建 DOM。`class` / `text` 直接映射，`onClick` 之类自动绑定事件，
 * 其余键优先走属性赋值，不行再 setAttribute。
 */
export function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);

  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;

    if (key === "class") {
      node.className = value;
    } else if (key === "text") {
      node.textContent = value;
    } else if (key.startsWith("on") && typeof value === "function") {
      node.addEventListener(key.slice(2).toLowerCase(), value);
    } else if (key in node) {
      node[key] = value;
    } else {
      node.setAttribute(key, value === true ? "" : value);
    }
  }

  node.append(...children.flat().filter((child) => child != null && child !== false));
  return node;
}

export function setBusy(button, busy) {
  button.disabled = busy;
  button.setAttribute("aria-busy", busy ? "true" : "false");
}

/**
 * 让点击一定落在新标签页里，不把当前页顶掉。
 *
 * 单靠 `target="_blank"` 不够：部分内嵌 WebView（桌面客户端、微信、某些壳浏览器）
 * 会忽略这个属性，直接把地址加载到当前页 —— 用户刚生成的结果或者正在看的列表就没了。
 * 这里显式开一个页签兜底；`window.open` 返回 null 说明被拦截，此时不 preventDefault，
 * 交回浏览器的默认行为，至少还有一次机会。
 *
 * `window.open` 刻意不带 features 参数：带参数的调用会被当成弹出窗口，
 * 而我们要的是普通标签页。断开 opener 的效果与 `rel="noopener"` 等价；
 * `rel="noreferrer"` 这条享受不到了（脚本打开的窗口带 referrer），
 * 短链场景里这个代价可以接受。
 */
export function openInNewTab(event) {
  if (event.defaultPrevented) return;
  // 组合键点击本来就由浏览器处理（新标签页 / 新窗口 / 下载），别抢
  if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;

  const opened = window.open(event.currentTarget.href, "_blank");
  if (!opened) return;

  try {
    opened.opener = null;
  } catch {
    // 跨域时个别内核会拒绝赋值，链接已经打开了，不值得为此打断
  }
  event.preventDefault();
}

export function delegate(root, selector, type, handler) {
  root.addEventListener(type, (event) => {
    const target = event.target.closest(selector);
    if (target && root.contains(target)) handler(event, target);
  });
}
