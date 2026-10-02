import http from "./http";

/** 包管理能力描述，由后端按 /etc/os-release 判定后下发 */
export interface PkgMeta {
  family: "arch" | "debian" | string;
  pretty: string;
  /** 包管理器命令名："pacman" / "apt" */
  manager: "pacman" | "apt" | string;
  /** true = 滚动更新发行版（Arch 系），没有「可升级」概念 */
  rolling: boolean;
}

const FALLBACK: PkgMeta = { family: "", pretty: "", manager: "", rolling: false };

let cached: Promise<PkgMeta> | null = null;

/** 全局只请求一次；失败不缓存，下次进入页面可重试 */
export function pkgMeta(): Promise<PkgMeta> {
  if (!cached) {
    cached = http
      .get<PkgMeta>("/packages/meta")
      .then((r) => r.data)
      .catch((e) => {
        cached = null;
        throw e;
      });
  }
  return cached;
}

/** 供界面直接使用：拿不到后端信息时按 Debian 口径降级，不阻断页面 */
export async function pkgMetaSafe(): Promise<PkgMeta> {
  try {
    return await pkgMeta();
  } catch {
    return FALLBACK;
  }
}
