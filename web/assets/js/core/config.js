/**
 * 运行时配置。
 *
 * 服务端在 /app-config.js 里写进 `window.__APP_CONFIG__`，页面因此不必为不同
 * 环境重新构建。注入脚本没加载成功时退回默认值：宁可少一个开关，
 * 也不要整页白屏。
 */

const DEFAULTS = Object.freeze({
  env: "production",
  siteName: "shortlink",
  apiBase: "",
  adminEnabled: true,
  liveReload: false,
  assetsStamp: "",
});

let cached = null;

export function appConfig() {
  if (cached) return cached;

  const injected = globalThis.__APP_CONFIG__;
  cached = Object.freeze({
    ...DEFAULTS,
    ...(injected && typeof injected === "object" ? injected : {}),
  });
  return cached;
}

/** 标题与品牌名跟着配置走，换部署不用改 HTML。 */
export function applySiteName() {
  const { siteName } = appConfig();
  if (!siteName) return;

  document.querySelectorAll(".brand__name, .topbar__brand span").forEach((node) => {
    node.textContent = siteName;
  });

  const title = document.title;
  if (title.includes("·")) {
    const [, suffix] = title.split("·");
    document.title = `${siteName}${suffix ? ` ·${suffix}` : ""}`;
  }
}
