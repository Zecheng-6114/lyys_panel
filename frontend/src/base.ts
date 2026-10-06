/**
 * 运行期基路径：全部取自服务端注入的 `<base href>`。
 *
 * 启用「安全入口」后，面板只在 `/<入口>/` 下可达，服务端会把该前缀写进 index.html
 * 的 base 标签；资源、接口与 WebSocket 都按 base 推导，因此前端一行地址都不用写死。
 * 未启用入口时 base 为 `/`，与不带 base 标签等价。
 */
export function basePath(): string {
  const href = document.querySelector("base")?.getAttribute("href") ?? "/";
  return href.endsWith("/") ? href : `${href}/`;
}

/** 后端接口基路径，如 `/api` 或 `/secret1/api` */
export function apiBase(): string {
  return `${basePath()}api`;
}

/** 拼接 WebSocket 地址，path 形如 `/terminal` */
export function wsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  return `${proto}://${location.host}${apiBase()}${path}`;
}
