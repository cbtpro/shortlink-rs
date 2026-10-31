/**
 * 接口前缀。
 *
 * 优先用服务端下发的 apiBase：反代到子路径部署时它会带上 `/sub`，
 * 比按当前页面路径推算可靠。没拿到就退回按目录推算，兼容手工拷贝的静态副本。
 */
import { appConfig } from "./config.js";

export function apiBase() {
  const { apiBase: configured } = appConfig();

  if (typeof configured === "string" && configured.length > 0) {
    return configured.replace(/\/$/, "");
  }

  return fromPath(location.pathname);
}

function fromPath(path) {
  return path.endsWith("/") ? path.slice(0, -1) : path.slice(0, path.lastIndexOf("/"));
}
