import { defineStore } from "pinia";
import { ElNotification } from "element-plus";
import {
  getJob,
  isActive,
  KIND_LABEL,
  listJobs,
  payloadSummary,
  STATUS_META,
  type JobRow,
  type JobStatus,
} from "../api/jobs";

/**
 * 后台作业的完成提示。
 *
 * 作业是「提交即返回」的：提交页只回一句「已加入任务队列」，之后跑完没人知道 ——
 * 用户离开提交页去干别的，结果就永远等不到。这里把提交过的作业登记下来，
 * 轮询它们的终态，结束时弹一条通知。
 *
 * 只在有登记中的作业时轮询，空闲时一个请求都不发。
 */

/** 轮询间隔：与「任务」页的列表自动刷新保持一致 */
const POLL_MS = 3000;

/** 终态 → 通知样式。不能直接用状态标签的 type：作业里的 primary/danger
 *  是 Element Plus 标签的取值，通知只认 success/warning/info/error。 */
const NOTIFY_TYPE: Record<JobStatus, "success" | "warning" | "info" | "error"> = {
  pending: "info",
  running: "info",
  success: "success",
  failed: "error",
  cancelled: "warning",
  interrupted: "warning",
};

export const useJobsStore = defineStore("jobs", () => {
  /** 已登记、还没拿到终态的作业 id */
  const watched = new Set<string>();
  let timer: number | null = null;
  /** 上一轮请求还没回来时跳过本轮，避免慢请求把轮询堆起来 */
  let polling = false;

  /** 登记一个作业，它跑完后会弹通知 */
  function watch(id: string): void {
    watched.add(id);
    if (timer === null) {
      timer = window.setInterval(() => void poll(), POLL_MS);
    }
  }

  /**
   * 进入面板时补登记服务端还在跑的作业。
   *
   * 覆盖「提交完刷新/重开页面」：作业在服务端还在跑，本地却已经不记得它了，
   * 不补这一下就永远等不到通知。库里已有的历史作业都是终态，登记它们没意义。
   */
  async function adoptRunning(): Promise<void> {
    try {
      for (const row of await listJobs()) {
        if (isActive(row.status)) watch(row.id);
      }
    } catch {
      /* 拉不到就少一次补登记，不影响提交时的登记 */
    }
  }

  async function poll(): Promise<void> {
    if (polling || watched.size === 0) return;
    polling = true;
    try {
      for (const id of [...watched]) {
        let row: JobRow;
        try {
          row = await getJob(id);
        } catch (e) {
          // 记录已不在库里（被清理），或凭证已失效（这时再问下去只会每 3 秒被弹一次登录）：
          // 都别再问了。其余错误留到下一轮重试。
          const st = (e as { response?: { status?: number } })?.response?.status;
          if (st === 404 || st === 401) watched.delete(id);
          continue;
        }
        if (isActive(row.status)) continue;
        watched.delete(id);
        notify(row);
      }
    } finally {
      polling = false;
      if (watched.size === 0 && timer !== null) {
        window.clearInterval(timer);
        timer = null;
      }
    }
  }

  function notify(row: JobRow): void {
    const name = KIND_LABEL[row.kind] ?? row.kind;
    const meta = STATUS_META[row.status] ?? { label: row.status };
    const parts: string[] = [];
    const detail = payloadSummary(row);
    // 参数为空对象（如创建备份）时 payloadSummary 会回 "{}"，没有信息量，不往通知里塞
    if (detail && detail !== "-" && detail !== "{}") parts.push(detail);
    if (row.error) parts.push(row.error);
    else if (row.status === "failed" && row.exit_code !== null) {
      parts.push(`退出码 ${row.exit_code}`);
    }
    ElNotification({
      title: `${name} · ${meta.label}`,
      message: parts.join(" · "),
      type: NOTIFY_TYPE[row.status] ?? "info",
      // 自动消失：常驻通知不会自己收走，跑几个作业就在右上角叠成一列，
      // 把后面的页面一直挡着，只能挨个点 ×。给足 8 秒让人从别处扫到一眼，
      // 之后自己退场（想提前关点 × 即可）。
      duration: 8000,
    });
  }

  return { watch, adoptRunning };
});
