import http from "./http";

/** 作业类型标签，与后端 `jobs::JobKind::as_str` 一一对应 */
export type JobKind =
  | "pkg_install"
  | "pkg_remove"
  | "pkg_upgrade"
  | "pkg_sysupgrade"
  | "pkg_update"
  | "backup_create"
  | "docker_pull"
  | "docker_install";

/**
 * 作业状态。
 *
 * `interrupted` 与 `failed` 是两回事：它表示「面板在作业运行中被重启」，
 * 而不是命令真的执行失败了 —— 需要分开展示，否则用户会去排查一个并不存在的故障。
 */
export type JobStatus =
  | "pending"
  | "running"
  | "success"
  | "failed"
  | "cancelled"
  | "interrupted";

export interface JobRow {
  id: string;
  kind: JobKind;
  payload: string;
  status: JobStatus;
  exit_code: number | null;
  stdout_tail: string;
  error: string | null;
  created_at: number;
  started_at: number | null;
  finished_at: number | null;
}

/** SSE 事件：一段输出，或终态通知 */
export type JobEvent =
  | { type: "lines"; text: string }
  | {
      type: "done";
      status: JobStatus;
      exit_code: number | null;
      error: string | null;
    };

/** 提交作业并拿到 id。请求立即返回，不等命令跑完 */
export async function submitJob(
  kind: JobKind,
  payload: Record<string, unknown> = {},
): Promise<string> {
  const r = await http.post<{ ok: boolean; id: string }>("/jobs", { kind, payload });
  return r.data.id;
}

export async function listJobs(limit = 50, offset = 0): Promise<JobRow[]> {
  const r = await http.get<JobRow[]>("/jobs", { params: { limit, offset } });
  return r.data;
}

export async function getJob(id: string): Promise<JobRow> {
  const r = await http.get<JobRow>(`/jobs/${id}`);
  return r.data;
}

export async function cancelJob(id: string): Promise<void> {
  await http.post(`/jobs/${id}/cancel`);
}

/**
 * 订阅作业输出流。
 *
 * 刻意不用 `EventSource`：它无法携带 `Authorization` 头，只能把 token 塞进
 * 查询串，而 URL 会进服务端访问日志与浏览器历史 —— 为一个日志流把长效凭证
 * 写到那种地方不值得。`fetch` + `ReadableStream` 能带正常请求头。
 *
 * 服务端会先回放已有输出尾部，所以断线重连不丢进度。
 */
export async function streamJob(
  id: string,
  onEvent: (ev: JobEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const token = localStorage.getItem("panel_token") ?? "";
  const resp = await fetch(`/api/jobs/${id}/stream`, {
    headers: { Authorization: `Bearer ${token}` },
    signal,
  });
  if (!resp.ok || !resp.body) {
    throw new Error(`无法建立作业输出流（HTTP ${resp.status}）`);
  }
  const reader = resp.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    // SSE 帧以空行分隔
    let idx = buf.indexOf("\n\n");
    while (idx >= 0) {
      const frame = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      const dataLine = frame.split("\n").find((l) => l.startsWith("data: "));
      if (dataLine) {
        try {
          onEvent(JSON.parse(dataLine.slice(6)) as JobEvent);
        } catch {
          // 半帧或坏帧：丢弃，不影响后续帧
        }
      }
      idx = buf.indexOf("\n\n");
    }
  }
}

/** 作业类型 → 中文名，与后端 `JobKind::label` 保持一致 */
export const KIND_LABEL: Record<string, string> = {
  pkg_install: "安装软件包",
  pkg_remove: "卸载软件包",
  pkg_upgrade: "升级软件包",
  pkg_sysupgrade: "滚动更新",
  pkg_update: "刷新软件索引",
  backup_create: "创建备份",
  docker_pull: "拉取镜像",
  docker_install: "安装 Docker",
};

type TagType = "info" | "primary" | "success" | "danger" | "warning";

/** 状态 → 标签样式与文案 */
export const STATUS_META: Record<JobStatus, { label: string; type: TagType }> = {
  pending: { label: "等待中", type: "info" },
  running: { label: "运行中", type: "primary" },
  success: { label: "成功", type: "success" },
  failed: { label: "失败", type: "danger" },
  cancelled: { label: "已取消", type: "warning" },
  interrupted: { label: "已中断", type: "warning" },
};

/** 未结束的状态：列表轮询与 SSE 只对它们有意义 */
export const ACTIVE_STATUS: JobStatus[] = ["pending", "running"];

export function isActive(status: JobStatus): boolean {
  return ACTIVE_STATUS.includes(status);
}

/** 作业参数的可读摘要（详情区展示用） */
export function payloadSummary(job: JobRow): string {
  try {
    const p = JSON.parse(job.payload || "{}") as Record<string, unknown>;
    if (Array.isArray(p.names) && p.names.length) {
      return (p.names as string[]).join(", ");
    }
    if (typeof p.target === "string") return p.target;
    if (typeof p.action === "string") return String(p.action);
  } catch {
    /* 参数不是 JSON 时退回原始文本 */
  }
  return job.payload || "-";
}
