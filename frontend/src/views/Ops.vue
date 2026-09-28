<template>
  <div class="ops">
    <!-- ---------- SMART 磁盘健康 ---------- -->
    <div class="panel">
      <div class="panel-head">
        <div class="section-title">磁盘健康（SMART）</div>
        <button
          class="mini-btn"
          type="button"
          :disabled="smartLoading"
          @click="loadSmart"
        >
          刷新
        </button>
      </div>

      <el-alert
        v-if="smart && !smart.available"
        type="warning"
        :closable="false"
        show-icon
        :title="'SMART 监控不可用'"
        :description="smart.message"
      />
      <el-table
        v-else
        v-loading="smartLoading"
        :data="smart?.disks ?? []"
        size="small"
      >
        <el-table-column label="设备" prop="device" width="130" class-name="mono" />
        <el-table-column label="型号" prop="model" min-width="180" show-overflow-tooltip />
        <el-table-column label="序列号" prop="serial" min-width="140" show-overflow-tooltip class-name="col-p3" label-class-name="col-p3" />
        <el-table-column label="健康状态" width="120">
          <template #default="{ row }">
            <span class="dot-wrap">
              <i class="dot" :class="row.healthy ? 'dot-on' : 'dot-off'" />
              {{ row.healthy ? "健康" : "异常" }}
            </span>
          </template>
        </el-table-column>
        <el-table-column label="温度" prop="temperature" width="80" class-name="col-p2" label-class-name="col-p2">
          <template #default="{ row }">{{ row.temperature != null ? row.temperature + "°C" : "—" }}</template>
        </el-table-column>
        <el-table-column label="通电时长" prop="power_on_hours" width="110" class-name="col-p2" label-class-name="col-p2">
          <template #default="{ row }">{{ row.power_on_hours != null ? row.power_on_hours + "h" : "—" }}</template>
        </el-table-column>
      </el-table>
    </div>

    <!-- ---------- 服务 unit 文件查看 ---------- -->
    <div class="panel">
      <div class="section-title">服务 unit 文件查看</div>
      <div class="unit-bar">
        <el-select
          v-model="unitName"
          filterable
          clearable
          placeholder="选择或输入服务名（如 sshd）"
          class="unit-select"
        >
          <el-option
            v-for="s in services"
            :key="s.name"
            :label="`${s.name} — ${s.description}`"
            :value="s.name"
          />
        </el-select>
        <button
          class="mini-btn"
          type="button"
          :disabled="!unitName || unitLoading"
          @click="loadUnit"
        >
          查看 unit 文件
        </button>
      </div>
      <el-alert
        v-if="unitError"
        type="error"
        :closable="true"
        show-icon
        :title="unitError"
      />
      <pre v-else-if="unitContent" class="unit-pre mono">{{ unitContent }}</pre>
      <div v-else class="hint">选择服务后展示其 systemd unit 文件内容（只读）</div>
    </div>

    <!-- ---------- 容器日志流 ---------- -->
    <div class="panel">
      <div class="section-title">容器日志流（实时）</div>
      <div class="unit-bar">
        <el-select
          v-model="logContainer"
          filterable
          placeholder="选择容器"
          class="unit-select"
        >
          <el-option
            v-for="c in containers"
            :key="c.id"
            :label="`${c.name}（${c.state}）`"
            :value="c.id"
          />
        </el-select>
        <el-select v-model="logTail" class="tail-select">
          <el-option v-for="n in [50, 100, 200, 500]" :key="n" :label="`尾部 ${n} 行`" :value="n" />
        </el-select>
        <button
          v-if="!logConnected"
          class="mini-btn"
          type="button"
          :disabled="!logContainer"
          @click="startLog"
        >
          开始
        </button>
        <button v-else class="mini-btn" type="button" @click="stopLog">停止</button>
        <button
          class="mini-btn"
          type="button"
          :disabled="!logLines.length"
          @click="logLines = []"
        >
          清空
        </button>
      </div>
      <div ref="logPane" class="log-pane mono">
        <div v-if="!logLines.length && !logConnected" class="hint">
          选择容器后点击「开始」，实时日志将在此滚动展示
        </div>
        <div v-for="(l, i) in logLines" :key="i" class="log-line">{{ l }}</div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import http from "../api/http";

// ---------- SMART ----------
interface SmartDisk {
  device: string;
  model: string | null;
  serial: string | null;
  healthy: boolean;
  temperature: number | null;
  power_on_hours: number | null;
}
interface SmartReport {
  available: boolean;
  message: string | null;
  disks: SmartDisk[];
}
const smart = ref<SmartReport | null>(null);
const smartLoading = ref(false);

async function loadSmart() {
  smartLoading.value = true;
  try {
    const { data } = await http.get("/disks/smart");
    smart.value = data;
  } finally {
    smartLoading.value = false;
  }
}

// ---------- unit 文件 ----------
interface ServiceInfo {
  name: string;
  description: string;
}
const services = ref<ServiceInfo[]>([]);
const unitName = ref("");
const unitContent = ref("");
const unitError = ref("");
const unitLoading = ref(false);

async function loadServices() {
  try {
    const { data } = await http.get("/services");
    services.value = data;
  } catch {
    ElMessage.error("服务列表加载失败");
  }
}
async function loadUnit() {
  if (!unitName.value) return;
  unitLoading.value = true;
  unitError.value = "";
  unitContent.value = "";
  try {
    const { data } = await http.get("/services/unit", { params: { name: unitName.value } });
    unitContent.value = data.content;
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    unitError.value = err.response?.data?.error ?? "unit 文件读取失败";
  } finally {
    unitLoading.value = false;
  }
}

// ---------- 容器日志流（WebSocket） ----------
interface ContainerInfo {
  id: string;
  name: string;
  state: string;
}
const containers = ref<ContainerInfo[]>([]);
const logContainer = ref("");
const logTail = ref(100);
const logConnected = ref(false);
const logLines = ref<string[]>([]);
const logPane = ref<HTMLElement>();
let ws: WebSocket | null = null;

const MAX_LINES = 2000; // 环形缓冲上限，防止长时间挂着撑爆内存

async function loadContainers() {
  try {
    const { data } = await http.get("/docker/containers");
    containers.value = data;
  } catch {
    ElMessage.error("容器列表加载失败");
  }
}

function wsBase() {
  const p = location.protocol === "https:" ? "wss" : "ws";
  return `${p}://${location.host}`;
}

function startLog() {
  if (!logContainer.value || ws) return;
  const token = localStorage.getItem("panel_token");
  if (!token) return;
  const url =
    `${wsBase()}/api/docker/logstream?id=${encodeURIComponent(logContainer.value)}` +
    `&tail=${logTail.value}&token=${encodeURIComponent(token)}`;
  ws = new WebSocket(url);
  logConnected.value = true;
  ws.onmessage = (ev) => {
    logLines.value.push(String(ev.data).replace(/\n$/, ""));
    if (logLines.value.length > MAX_LINES) {
      logLines.value.splice(0, logLines.value.length - MAX_LINES);
    }
    nextTick(() => {
      logPane.value?.scrollTo({ top: logPane.value.scrollHeight });
    });
  };
  ws.onclose = () => {
    logConnected.value = false;
    ws = null;
  };
  ws.onerror = () => {
    // onclose 一定跟在 onerror 后面，状态复位交给 onclose
  };
}

function stopLog() {
  ws?.close();
  ws = null;
  logConnected.value = false;
}

// 容器运行状态变化后重启流才生效；切换容器时直接断开旧流
watch(logContainer, () => stopLog());

onMounted(() => {
  loadSmart();
  loadServices();
  loadContainers();
});
onBeforeUnmount(() => {
  stopLog();
});
</script>

<style scoped>
.ops {
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.panel {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 16px;
}
.panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 8px;
}
.section-title {
  font-size: 14px;
  font-weight: 500;
  margin-bottom: 8px;
}
.panel-head .section-title {
  margin-bottom: 0;
}
.unit-bar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.unit-select {
  flex: 1;
  min-width: 220px;
  max-width: 480px;
}
.tail-select {
  width: 130px;
}
.mini-btn {
  border: none;
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
  border-radius: var(--radius);
  padding: 6px 12px;
  font-size: 12px;
  cursor: pointer;
  flex: none;
}
.mini-btn:hover {
  background: var(--el-fill-color);
}
.mini-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.unit-pre {
  margin: 8px 0 0;
  padding: 12px;
  background: var(--el-fill-color-lighter);
  border-radius: var(--radius);
  font-size: 12px;
  line-height: 1.6;
  overflow: auto;
  max-height: 420px;
  white-space: pre-wrap;
  word-break: break-all;
}
.log-pane {
  margin-top: 8px;
  padding: 12px;
  height: 320px;
  overflow: auto;
  background: var(--el-fill-color-lighter);
  border-radius: var(--radius);
  font-size: 12px;
  line-height: 1.6;
}
.log-line {
  white-space: pre-wrap;
  word-break: break-all;
}
.hint {
  margin-top: 8px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
}
.dot-on {
  background: var(--el-color-success);
}
.dot-off {
  background: var(--el-color-danger);
}
@media (max-width: 768px) {
  .unit-select {
    max-width: none;
  }
}
</style>
