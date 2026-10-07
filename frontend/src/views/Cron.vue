<template>
  <div class="cron">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button @click="openEdit(null)">新增任务</el-button>
      <span class="hint">计划任务以 root 身份写入 root crontab</span>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      class="ctable"
      height="var(--panel-table-height)"
    >
      <el-table-column label="说明" v-bind="col(140, true)">
        <template #default="{ row }">{{ row.comment || "-" }}</template>
      </el-table-column>
      <!-- 手机档把「分」也收起：时/日/月/周 早已在窄屏摘掉，单独留一个「分」
           看着像完整调度，反而误导。手机上留「说明 + 命令 + 操作」这一组，
           要看完整表达式进「编辑」。 -->
      <el-table-column label="分" prop="minute" v-bind="col(70)" v-if="!hideColP2" />
      <el-table-column label="时" prop="hour" v-bind="col(70)" v-if="!hideColP2" />
      <el-table-column label="日" prop="day" v-bind="col(70)" v-if="!hideColP2" />
      <el-table-column label="月" prop="month" v-bind="col(70)" v-if="!hideColP3" />
      <el-table-column label="周" prop="weekday" v-bind="col(70)" v-if="!hideColP3" />
      <el-table-column label="命令" prop="command" v-bind="col(280, true)" show-overflow-tooltip />
      <el-table-column label="操作" v-bind="col(120)" align="right">
        <template #default="{ row, $index }">
          <el-button link size="small" @click="openEdit({ index: $index, entry: row })">
            编辑
          </el-button>
          <el-button link size="small" @click="removeEntry($index)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <div class="section-title">systemd 定时器</div>
    <div class="toolbar">
      <el-button @click="loadTimers">刷新</el-button>
      <el-button @click="openTimer(null)">新增定时器</el-button>
      <span class="hint">由 systemd 调度（OnCalendar 表达式），输出进 journald，可查看执行日志</span>
    </div>

    <div class="block-timers">
      <el-table v-loading="timersLoading" :data="timers" size="small" class="ctable">
        <el-table-column label="说明" v-bind="col(150, true)">
          <template #default="{ row }">{{ row.description || row.id }}</template>
        </el-table-column>
        <el-table-column label="调度" prop="schedule" v-bind="col(170)" />
        <el-table-column label="命令" prop="command" v-bind="col(260, true)" show-overflow-tooltip />
        <el-table-column label="状态" v-bind="col(90)">
          <template #default="{ row }">
            <span class="dot-wrap">
              <i class="dot" :class="row.enabled ? 'dot-on' : 'dot-off'" />
              {{ row.enabled ? "启用" : "停用" }}
            </span>
          </template>
        </el-table-column>
        <el-table-column label="操作" v-bind="col(200)" align="right">
          <template #default="{ row }">
            <el-button link size="small" @click="runTimer(row.id)">执行</el-button>
            <el-button link size="small" @click="showLogs(row.id)">日志</el-button>
            <el-button link size="small" @click="openTimer(row)">编辑</el-button>
            <el-button link size="small" @click="removeTimer(row.id)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <el-dialog
      v-model="showTimer"
      :title="timerForm.id ? '编辑定时器' : '新增定时器'"
      width="560px"
    >
      <el-form label-width="92px" size="small">
        <el-form-item label="说明">
          <el-input v-model="timerForm.description" placeholder="可选，用于识别任务" />
        </el-form-item>
        <el-form-item label="OnCalendar">
          <el-input v-model="timerForm.schedule" placeholder="如：*-*-* 03:00:00" />
        </el-form-item>
        <el-form-item label="命令">
          <el-input
            v-model="timerForm.command"
            type="textarea"
            :rows="3"
            placeholder="以 /bin/sh 执行，可写多行"
          />
        </el-form-item>
        <el-form-item label="启用">
          <el-switch v-model="timerForm.enabled" />
        </el-form-item>
      </el-form>
      <div class="presets">
        <span>常用：</span>
        <button
          v-for="p in timerPresets"
          :key="p.label"
          type="button"
          class="mini-btn mini-btn--sm"
          @click="timerForm.schedule = p.value"
        >
          {{ p.label }}
        </button>
      </div>
      <template #footer>
        <el-button @click="showTimer = false">取消</el-button>
        <el-button @click="saveTimer">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showLogsDlg" :title="`执行日志 · ${logsId}`" width="720px">
      <pre class="logbox">{{ logsText || "（暂无日志）" }}</pre>
      <template #footer>
        <el-button @click="showLogsDlg = false">关闭</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showEdit" :title="editIndex === null ? '新增任务' : '编辑任务'" width="520px">
      <el-form label-width="64px" size="small">
        <el-form-item label="说明">
          <el-input v-model="form.comment" placeholder="可选，用于识别任务" />
        </el-form-item>
        <el-form-item label="调度">
          <div class="sched">
            <el-input v-model="form.minute" placeholder="分" />
            <el-input v-model="form.hour" placeholder="时" />
            <el-input v-model="form.day" placeholder="日" />
            <el-input v-model="form.month" placeholder="月" />
            <el-input v-model="form.weekday" placeholder="周" />
          </div>
        </el-form-item>
        <el-form-item label="命令">
          <el-input v-model="form.command" placeholder="如：/usr/local/bin/backup.sh" />
        </el-form-item>
      </el-form>
      <div class="presets">
        <span>常用：</span>
        <button
          v-for="p in presets"
          :key="p.label"
          type="button"
          class="mini-btn mini-btn--sm"
          @click="applyPreset(p)"
        >
          {{ p.label }}
        </button>
      </div>
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button @click="save">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";

interface CronEntry {
  minute: string;
  hour: string;
  day: string;
  month: string;
  weekday: string;
  command: string;
  comment: string;
}

const rows = ref<CronEntry[]>([]);
const loading = ref(false);
const showEdit = ref(false);
const editIndex = ref<number | null>(null);
const form = reactive<CronEntry>({
  minute: "*",
  hour: "*",
  day: "*",
  month: "*",
  weekday: "*",
  command: "",
  comment: "",
});

const presets = [
  { label: "每分钟", minute: "*", hour: "*", day: "*", month: "*", weekday: "*" },
  { label: "每小时", minute: "0", hour: "*", day: "*", month: "*", weekday: "*" },
  { label: "每天零点", minute: "0", hour: "0", day: "*", month: "*", weekday: "*" },
  { label: "每周一", minute: "0", hour: "9", day: "*", month: "*", weekday: "1" },
];

function applyPreset(p: (typeof presets)[number]) {
  form.minute = p.minute;
  form.hour = p.hour;
  form.day = p.day;
  form.month = p.month;
  form.weekday = p.weekday;
}

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/cron");
    rows.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取计划任务失败");
  } finally {
    loading.value = false;
  }
}

function openEdit(target: { index: number; entry: CronEntry } | null) {
  if (target === null) {
    editIndex.value = null;
    Object.assign(form, {
      minute: "*",
      hour: "*",
      day: "*",
      month: "*",
      weekday: "*",
      command: "",
      comment: "",
    });
  } else {
    editIndex.value = target.index;
    Object.assign(form, target.entry);
  }
  showEdit.value = true;
}

async function save() {
  if (!form.command.trim()) {
    ElMessage.warning("命令不能为空");
    return;
  }
  try {
    if (editIndex.value === null) {
      await http.post("/cron", { entry: form });
    } else {
      await http.put("/cron", { index: editIndex.value, entry: form });
    }
    showEdit.value = false;
    ElMessage.success("已保存");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function removeEntry(index: number) {
  try {
    await ElMessageBox.confirm("确定删除该计划任务？", "删除确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete("/cron", { data: { index } });
    ElMessage.success("已删除");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

// ---------- systemd 定时器 ----------

interface TimerJob {
  id: string;
  schedule: string;
  command: string;
  description: string;
  enabled: boolean;
}

const timers = ref<TimerJob[]>([]);
const timersLoading = ref(false);
const showTimer = ref(false);
const timerForm = reactive({
  id: "",
  schedule: "",
  command: "",
  description: "",
  enabled: true,
});

const timerPresets = [
  { label: "每小时", value: "hourly" },
  { label: "每天零点", value: "*-*-* 00:00:00" },
  { label: "每周一 09:00", value: "Mon *-*-* 09:00:00" },
  { label: "每月 1 号 03:00", value: "*-*-01 03:00:00" },
];

const showLogsDlg = ref(false);
const logsText = ref("");
const logsId = ref("");

async function loadTimers() {
  timersLoading.value = true;
  try {
    const { data } = await http.get("/timers");
    timers.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取定时任务失败");
  } finally {
    timersLoading.value = false;
  }
}

function openTimer(row: TimerJob | null) {
  if (row === null) {
    Object.assign(timerForm, {
      id: "",
      schedule: "*-*-* 03:00:00",
      command: "",
      description: "",
      enabled: true,
    });
  } else {
    Object.assign(timerForm, {
      id: row.id,
      schedule: row.schedule,
      command: row.command,
      description: row.description,
      enabled: row.enabled,
    });
  }
  showTimer.value = true;
}

async function saveTimer() {
  if (!timerForm.schedule.trim()) {
    ElMessage.warning("OnCalendar 表达式不能为空");
    return;
  }
  if (!timerForm.command.trim()) {
    ElMessage.warning("命令不能为空");
    return;
  }
  try {
    await http.post("/timers", {
      // 新建时传 null，由服务端分配标识
      id: timerForm.id || null,
      schedule: timerForm.schedule,
      command: timerForm.command,
      description: timerForm.description,
      enabled: timerForm.enabled,
    });
    showTimer.value = false;
    ElMessage.success("已保存");
    loadTimers();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function removeTimer(id: string) {
  try {
    await ElMessageBox.confirm("确定删除该定时器？", "删除确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete("/timers", { data: { id } });
    ElMessage.success("已删除");
    loadTimers();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

async function runTimer(id: string) {
  try {
    const { data } = await http.post("/timers/run", { id });
    ElMessage.success(data.message ?? "已触发执行");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "执行失败");
  }
}

async function showLogs(id: string) {
  logsId.value = id;
  logsText.value = "";
  showLogsDlg.value = true;
  try {
    const { data } = await http.get("/timers/logs", { params: { id, lines: 200 } });
    logsText.value = data.logs ?? "";
  } catch (e: any) {
    logsText.value = e.response?.data?.error ?? "读取日志失败";
  }
}

onMounted(() => {
  load();
  loadTimers();
});
</script>

<style scoped>
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-left: var(--sp-1);
}
.sched {
  display: flex;
  gap: var(--sp-2);
  width: 100%;
}
.presets {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  font-size: 12px;
  color: var(--el-text-color-secondary);
  padding-left: 64px;
}
.section-title {
  font-size: 14px;
  font-weight: 600;
  /* 父容器不是带 gap 的 flex，标题上方留白得自己带（R4） */
  margin: var(--sp-4) 0 var(--sp-2);
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-2);
}
.logbox {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  box-shadow: var(--panel-shadow-1);
  padding: var(--sp-3) var(--sp-4);
  max-height: 420px;
  overflow: auto;
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 0;
}
</style>
