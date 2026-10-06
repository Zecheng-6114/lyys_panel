<template>
  <div class="tasks">
    <div class="toolbar">
      <el-button :loading="loading" @click="load">刷新</el-button>
      <el-tag v-if="hasActive" type="primary" effect="plain" size="small">
        有作业正在运行，列表每 3 秒自动刷新
      </el-tag>
      <span class="hint">
        长操作在后台执行，可以离开本页；每个作业保留最近 200 行输出
      </span>
    </div>

    <el-table
      v-loading="loading"
      :data="jobs"
      size="small"
      height="var(--panel-table-height)"
    >
      <el-table-column label="类型" v-bind="col(130)">
        <template #default="{ row }">{{ kindLabel(row.kind) }}</template>
      </el-table-column>
      <el-table-column label="参数" v-bind="col(220, true)" show-overflow-tooltip>
        <template #default="{ row }">
          <span class="mono">{{ summary(row) }}</span>
        </template>
      </el-table-column>
      <el-table-column label="状态" v-bind="col(100)">
        <template #default="{ row }">
          <el-tag :type="statusMeta(row.status).type" size="small" effect="plain">
            {{ statusMeta(row.status).label }}
          </el-tag>
        </template>
      </el-table-column>
      <el-table-column label="耗时" v-bind="col(110)" v-if="!hideColP2">
        <template #default="{ row }">{{ elapsed(row) }}</template>
      </el-table-column>
      <el-table-column label="提交时间" v-bind="col(180)" v-if="!hideColP2">
        <template #default="{ row }">{{ fmtTime(row.created_at) }}</template>
      </el-table-column>
      <el-table-column label="操作" v-bind="col(150)" align="right">
        <template #default="{ row }">
          <el-button link size="small" @click="openDetail(row.id)">查看输出</el-button>
          <el-button
            v-if="isActive(row.status)"
            link
            size="small"
            @click="doCancel(row.id)"
          >
            取消
          </el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-drawer v-model="drawer" :title="drawerTitle" size="60%" @closed="closeDetail">
      <div v-if="detail" class="detail">
        <div class="detail-meta">
          <el-tag :type="statusMeta(detail.status).type" size="small" effect="plain">
            {{ statusMeta(detail.status).label }}
          </el-tag>
          <span v-if="detail.exit_code !== null">退出码 {{ detail.exit_code }}</span>
          <span class="mono">{{ summary(detail) }}</span>
        </div>
        <el-alert
          v-if="detail.error"
          :title="detail.error"
          type="error"
          :closable="false"
          show-icon
        />
        <pre ref="outBox" class="output">{{ output || "（暂无输出）" }}</pre>
      </div>
    </el-drawer>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2 } from "../composables/useResponsive";
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import {
  cancelJob,
  getJob,
  isActive,
  KIND_LABEL,
  listJobs,
  payloadSummary,
  STATUS_META,
  streamJob,
  type JobRow,
  type JobStatus,
} from "../api/jobs";

const jobs = ref<JobRow[]>([]);
const loading = ref(false);
const drawer = ref(false);
const detail = ref<JobRow | null>(null);
const output = ref("");
const outBox = ref<HTMLElement | null>(null);

/** 有未结束的作业时才轮询列表，空闲时完全不发请求 */
const hasActive = computed(() => jobs.value.some((j) => isActive(j.status)));
const drawerTitle = computed(() =>
  detail.value ? (KIND_LABEL[detail.value.kind] ?? detail.value.kind) : "作业输出",
);

let pollTimer: number | null = null;
let streamCtl: AbortController | null = null;

function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}
function statusMeta(status: JobStatus) {
  return STATUS_META[status] ?? { label: status, type: "info" as const };
}
function summary(row: JobRow): string {
  return payloadSummary(row);
}
function fmtTime(ts: number | null): string {
  return ts ? new Date(ts * 1000).toLocaleString() : "-";
}
function elapsed(row: JobRow): string {
  if (!row.started_at) return "-";
  const end = row.finished_at ?? Math.floor(Date.now() / 1000);
  const secs = Math.max(0, end - row.started_at);
  if (secs < 60) return `${secs} 秒`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} 分 ${secs % 60} 秒`;
  return `${Math.floor(mins / 60)} 时 ${mins % 60} 分`;
}
function errText(e: unknown): string {
  const x = e as { response?: { data?: { error?: string } }; message?: string };
  return x?.response?.data?.error ?? x?.message ?? "请求失败";
}

async function load() {
  loading.value = true;
  try {
    jobs.value = await listJobs();
  } catch (e) {
    ElMessage.error(errText(e));
  } finally {
    loading.value = false;
  }
}

/** 打开详情：先取一次完整尾部，未结束时再挂上增量流 */
async function openDetail(id: string) {
  try {
    const job = await getJob(id);
    detail.value = job;
    output.value = job.stdout_tail || "";
    drawer.value = true;
    await scrollBottom();
    if (isActive(job.status)) {
      startStream(id);
    }
  } catch (e) {
    ElMessage.error(errText(e));
  }
}

function startStream(id: string) {
  stopStream();
  streamCtl = new AbortController();
  streamJob(
    id,
    (ev) => {
      if (ev.type === "lines") {
        const sep = output.value ? "\n" : "";
        output.value = trimLines(output.value + sep + ev.text);
        void scrollBottom();
        return;
      }
      // 终态：更新抽屉里的状态并刷新列表
      if (detail.value && detail.value.id === id) {
        detail.value = {
          ...detail.value,
          status: ev.status,
          exit_code: ev.exit_code,
          error: ev.error,
          finished_at: Math.floor(Date.now() / 1000),
        };
      }
      stopStream();
      void load();
    },
    streamCtl.signal,
  ).catch(() => {
    // 流被主动中止或页面已关闭：不是错误
  });
}

function stopStream() {
  streamCtl?.abort();
  streamCtl = null;
}

/**
 * 限制前端显示行数。与后端 200 行尾部不是同一口径：这里只管住渲染，
 * 防止极端输出把浏览器卡死。
 */
function trimLines(text: string): string {
  const lines = text.split("\n");
  return lines.length > 2000 ? lines.slice(-2000).join("\n") : text;
}

async function scrollBottom() {
  await nextTick();
  const el = outBox.value;
  if (el) el.scrollTop = el.scrollHeight;
}

async function doCancel(id: string) {
  try {
    await cancelJob(id);
    ElMessage.success("已取消");
    stopStream();
    await load();
  } catch (e) {
    ElMessage.error(errText(e));
  }
}

function closeDetail() {
  stopStream();
  detail.value = null;
  output.value = "";
}

onMounted(() => {
  void load();
  pollTimer = window.setInterval(() => {
    if (hasActive.value) void load();
  }, 3000);
});

onBeforeUnmount(() => {
  if (pollTimer !== null) window.clearInterval(pollTimer);
  stopStream();
});
</script>

<style scoped>
.toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
}
.hint {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
.detail {
  display: flex;
  flex-direction: column;
  gap: 10px;
  height: 100%;
}
.detail-meta {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.output {
  flex: 1;
  margin: 0;
  overflow: auto;
  padding: 10px;
  border-radius: 6px;
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  word-break: break-all;
}
.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
</style>
