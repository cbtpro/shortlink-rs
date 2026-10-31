// 开发期自动刷新：轮询资源版本号，变了就重载页面。
//
// 由后端在 ASSETS_MODE=disk 且 LIVE_RELOAD=true 时注入，生产环境不会被加载。
// 轮询而不是 WebSocket：后端重启期间连接必然断，轮询能自己恢复。

const endpoint = new URL("revision", import.meta.url);
const intervalMs = 800;

let current = null;

async function poll() {
  try {
    const response = await fetch(endpoint, { cache: "no-store" });
    if (!response.ok) throw new Error(String(response.status));

    const { revision } = await response.json();
    if (current === null) {
      current = revision;
    } else if (revision !== current) {
      location.reload();
      return;
    }
  } catch {
    // 后端正在重启，下一轮自然恢复
  }

  setTimeout(poll, intervalMs);
}

if (!globalThis.__shortlinkLiveReloadStarted) {
  globalThis.__shortlinkLiveReloadStarted = true;
  poll();
}
