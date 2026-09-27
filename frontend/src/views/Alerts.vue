<template>
  <div class="alerts">
    <div class="toolbar">
      <el-button @click="loadAll">刷新</el-button>
      <el-button @click="openEdit(null)">新增规则</el-button>
      <span class="hint">指标越过阈值触发告警，回落时自动恢复；规则变更一小时内生效</span>
    </div>

    <el-table v-loading="loading" :data="rules" size="small" height="240px">
      <el-table-column label="指标" width="120">
        <template #default="{ row }">{{ metricLabel(row.metric) }}</template>
      </el-table-column>
      <el-table-column label="阈值" width="100">
        <template #default="{ row }">{{ row.threshold }}%</template>
      </el-table-column>
      <el-table-column label="恢复通知" width="100">
        <template #default="{ row }">{{ row.notify_resolve ? "是" : "否" }}</template>
      </el-table-column>
      <el-table-column label="操作" width="120" align="right">
        <template #default="{ $index }">
          <el-button link size="small" @click="openEdit($index)">编辑</el-button>
          <el-button link size="small" @click="removeRule($index)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <div class="section-title">Webhook 通知</div>
    <div class="webhook-row">
      <el-input
        v-model="webhook"
        size="small"
        placeholder="https://open.feishu.cn/open-apis/bot/v2/hook/…（留空则不通知）"
        style="max-width: 520px"
      />
      <el-button size="small" @click="saveWebhook">保存</el-button>
    </div>

    <div class="section-title">最近告警事件</div>
    <el-table v-loading="eventsLoading" :data="events" size="small" height="var(--panel-table-height)">
      <el-table-column label="时间" width="180">
        <template #default="{ row }">{{ fmtTime(row.ts) }}</template>
      </el-table-column>
      <el-table-column label="指标" width="100" prop="metric" />
      <el-table-column label="当前值" width="100">
        <template #default="{ row }">{{ row.value.toFixed(1) }}%</template>
      </el-table-column>
      <el-table-column label="阈值" width="100">
        <template #default="{ row }">{{ row.threshold.toFixed(0) }}%</template>
      </el-table-column>
      <el-table-column label="状态" min-width="120">
        <template #default="{ row }">
          <span class="dot-wrap">
            <i class="dot" :class="row.state === 'firing' ? 'dot-on' : 'dot-off'" />
            {{ row.state === "firing" ? "触发" : "恢复" }}
          </span>
        </template>
      </el-table-column>
    </el-table>

    <el-dialog v-model="showEdit" :title="editIndex === null ? '新增规则' : '编辑规则'" width="400px">
      <el-form label-width="80px" size="small">
        <el-form-item label="指标">
          <el-select v-model="form.metric" style="width: 100%">
            <el-option label="CPU 使用率" value="cpu" />
            <el-option label="内存使用率" value="mem" />
            <el-option label="磁盘使用率" value="disk" />
          </el-select>
        </el-form-item>
        <el-form-item label="阈值 %">
          <el-input-number v-model="form.threshold" :min="1" :max="99" style="width: 100%" />
        </el-form-item>
        <el-form-item label="恢复通知">
          <el-switch v-model="form.notify_resolve" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button @click="saveRule">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";

interface AlertRule {
  metric: "cpu" | "mem" | "disk";
  threshold: number;
  notify_resolve: boolean;
}

interface AlertEvent {
  ts: number;
  metric: string;
  value: number;
  threshold: number;
  state: string;
}

const rules = ref<AlertRule[]>([]);
const events = ref<AlertEvent[]>([]);
const webhook = ref("");
const loading = ref(false);
const eventsLoading = ref(false);
const showEdit = ref(false);
const editIndex = ref<number | null>(null);
const form = reactive<AlertRule>({ metric: "cpu", threshold: 90, notify_resolve: false });

function metricLabel(m: string) {
  return m === "cpu" ? "CPU" : m === "mem" ? "内存" : "磁盘";
}

function fmtTime(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

async function loadRules() {
  loading.value = true;
  try {
    const { data } = await http.get("/alerts/rules");
    rules.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取规则失败");
  } finally {
    loading.value = false;
  }
}

async function loadWebhook() {
  try {
    const { data } = await http.get("/alerts/webhook");
    webhook.value = data.url ?? "";
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取 webhook 失败");
  }
}

async function loadEvents() {
  eventsLoading.value = true;
  try {
    const { data } = await http.get("/alerts/events", { params: { limit: 100 } });
    events.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取告警事件失败");
  } finally {
    eventsLoading.value = false;
  }
}

function loadAll() {
  loadRules();
  loadWebhook();
  loadEvents();
}

function openEdit(index: number | null) {
  editIndex.value = index;
  if (index === null) {
    Object.assign(form, { metric: "cpu", threshold: 90, notify_resolve: false });
  } else {
    Object.assign(form, rules.value[index]);
  }
  showEdit.value = true;
}

async function saveRule() {
  const next = [...rules.value];
  if (editIndex.value === null) {
    next.push({ ...form });
  } else {
    next[editIndex.value] = { ...form };
  }
  try {
    await http.post("/alerts/rules", next);
    showEdit.value = false;
    ElMessage.success("已保存");
    loadRules();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function removeRule(index: number) {
  const next = rules.value.filter((_, i) => i !== index);
  try {
    await http.post("/alerts/rules", next);
    ElMessage.success("已删除");
    loadRules();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

async function saveWebhook() {
  try {
    await http.post("/alerts/webhook", { url: webhook.value || null });
    ElMessage.success(webhook.value ? "webhook 已保存" : "已清除 webhook");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

onMounted(loadAll);
</script>

<style scoped>
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-left: 4px;
}
.section-title {
  font-size: 14px;
  font-weight: 500;
  margin: 20px 0 10px;
}
.webhook-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
</style>
