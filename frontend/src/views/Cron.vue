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
      <el-table-column label="说明" min-width="140">
        <template #default="{ row }">{{ row.comment || "-" }}</template>
      </el-table-column>
      <el-table-column label="分" prop="minute" width="70" />
      <el-table-column label="时" prop="hour" width="70" />
      <el-table-column label="日" prop="day" width="70" />
      <el-table-column label="月" prop="month" width="70" />
      <el-table-column label="周" prop="weekday" width="70" />
      <el-table-column label="命令" prop="command" min-width="280" show-overflow-tooltip />
      <el-table-column label="操作" width="120" align="right">
        <template #default="{ row, $index }">
          <el-button link size="small" @click="openEdit({ index: $index, entry: row })">
            编辑
          </el-button>
          <el-button link size="small" @click="removeEntry($index)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

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

onMounted(load);
</script>

<style scoped>
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-left: 4px;
}
.sched {
  display: flex;
  gap: 6px;
  width: 100%;
}
.presets {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  padding-left: 64px;
}
</style>
