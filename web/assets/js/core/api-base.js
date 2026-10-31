/** 可能部署在反向代理的子路径下，接口地址按当前页面所在目录推算。 */
export const API_BASE = (() => {
  const path = location.pathname;
  return path.endsWith("/") ? path.slice(0, -1) : path.slice(0, path.lastIndexOf("/"));
})();
