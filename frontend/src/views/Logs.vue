<template>
  <div class="logs">
    <div class="toolbar">
      <el-radio-group v-model="mode">
        <el-radio-button value="journal">系统日志</el-radio-button>
        <el-radio-button value="file">文件日志</el-radio-button>
        <el-radio-button v-if="auth.isAdmin()" value="audit">操作审计</el-radio-button>
      </el-radio-group>
      <template v-if="mode === 'journal'">
        <el-input v-model="unit" placeholder="单元名（如 sshd，留空为全部）" clearable style="width: 240px" />
        <el-input-number v-model="lines" :min="50" :max="2000" :step="100" />
        <el-button @click="load">加载</el-button>
      </template>
      <template v-else-if="mode === 'file'">
        <el-select v-model="file" placeholder="选择 /var/log 下的文件" style="width: 240px" filterable>
          <el-option v-for="f in files" :key="f" :label="f" :value="'/var/log/' + f" />
        </el-select>
        <el-input-number v-model="lines" :min="50" :max="2000" :step="100" />
        <el-button @click="load">加载</el-button>
      </template>
      <template v-else>
        <el-button @click="loadAudit">刷新</el-button>
        <el-button :disabled="auditOffset === 0" @click="prevPage">上一页</el-button>
        <el-button @click="nextPage">下一页</el-button>
      </template>
    </div>
    <!-- 操作审计（2.3）：非 GET 请求的流水记录，仅 admin 可见 -->
    <el-table v-if="mode === 'audit'" v-loading="auditLoading" :data="auditRows" size="small" height="var(--panel-table-height)">
      <el-table-column label="时间" width="170">
        <template #default="{ row }">{{ fmt(row.ts) }}</template>
      </el-table-column>
      <el-table-column label="用户" prop="username" width="120" />
      <el-table-column label="方法" prop="method" width="80" class-name="mono col-p3" label-class-name="col-p3" />
      <el-table-column label="路径" prop="path" min-width="220" show-overflow-tooltip class-name="mono" />
      <el-table-column label="状态" prop="status" width="80" class-name="mono" />
      <el-table-column label="来源 IP" prop="ip" width="140" class-name="mono col-p2" label-class-name="col-p2" />
    </el-table>
    <pre v-else class="logbox">{{ text || "（暂无内容）" }}</pre>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import http from "../api/http";
import { useAuthStore } from "../stores/auth";

interface AuditRow {
  ts: number;
  user_id: number | null;
  username: string;
  method: string;
  path: string;
  status: number;
  ip: string;
}

const auth = useAuthStore();
const mode = ref<"journal" | "file" | "audit">("journal");
const unit = ref("");
const file = ref("");
const files = ref<string[]>([]);
const lines = ref(200);
const text = ref("");

const auditRows = ref<AuditRow[]>([]);
const auditLoading = ref(false);
const auditOffset = ref(0);
const auditLimit = 100;

function fmt(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

async function loadAudit() {
  auditLoading.value = true;
  try {
    const { data } = await http.get("/audit", {
      params: { limit: auditLimit, offset: auditOffset.value },
    });
    auditRows.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取审计日志失败");
  } finally {
    auditLoading.value = false;
  }
}

function prevPage() {
  auditOffset.value = Math.max(0, auditOffset.value - auditLimit);
  loadAudit();
}

function nextPage() {
  if (auditRows.value.length >= auditLimit) {
    auditOffset.value += auditLimit;
    loadAudit();
  }
}

// 切换到审计标签时自动加载一次
watch(mode, (m) => {
  if (m === "audit" && auditRows.value.length === 0) loadAudit();
});

async function load() {
  try {
    if (mode.value === "journal") {
      const { data } = await http.get("/logs/journal", {
        params: { unit: unit.value || undefined, lines: lines.value },
      });
      text.value = data.text;
    } else {
      if (!file.value) {
        ElMessage.warning("请先选择日志文件");
        return;
      }
      const { data } = await http.get("/logs/tail", {
        params: { path: file.value, lines: lines.value },
      });
      text.value = data.text;
    }
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取日志失败");
  }
}

async function loadFiles() {
  const { data } = await http.get("/logs/files");
  files.value = data;
}

onMounted(loadFiles);
</script>

<style scoped>
.logbox {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 12px 16px;
  height: var(--panel-table-height);
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
