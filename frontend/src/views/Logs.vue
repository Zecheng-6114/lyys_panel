<template>
  <div class="logs">
    <div class="toolbar">
      <!-- 从实例卡片跳进来时只有一种日志来源，不再给切换项 -->
      <el-radio-group v-if="!scoped" ref="modeGroup" v-model="mode">
        <el-radio-button value="journal">系统日志</el-radio-button>
        <el-radio-button value="file">文件日志</el-radio-button>
        <el-radio-button v-if="auth.isAdmin()" value="audit">操作审计</el-radio-button>
      </el-radio-group>
      <el-tag v-else closable type="info" size="small" @close="clearInstance">
        {{ isContainer ? `容器日志 · ${containerShort}` : `服务日志 · ${serviceUnit}` }}
      </el-tag>

      <template v-if="mode === 'journal'">
        <el-input
          v-model="unit"
          placeholder="单元名（如 sshd，留空为全部）"
          :disabled="isService"
          clearable
          style="width: 240px"
        />
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
      <template v-else-if="mode === 'container'">
        <el-input-number v-model="lines" :min="50" :max="2000" :step="100" />
        <el-button @click="load">刷新</el-button>
      </template>
      <template v-else>
        <el-button @click="loadAudit">刷新</el-button>
        <el-button :disabled="auditOffset === 0" @click="prevPage">上一页</el-button>
        <el-button @click="nextPage">下一页</el-button>
      </template>
    </div>
    <!-- 操作审计（2.3）：非 GET 请求的流水记录，仅 admin 可见 -->
    <el-table v-if="mode === 'audit'" v-loading="auditLoading" :data="auditRows" size="small" height="var(--panel-table-height)">
      <el-table-column label="时间" v-bind="col(170)">
        <template #default="{ row }">{{ fmt(row.ts) }}</template>
      </el-table-column>
      <el-table-column label="用户" prop="username" v-bind="col(120)" />
      <el-table-column label="方法" prop="method" v-bind="col(80)" class-name="mono" v-if="!hideColP3" />
      <el-table-column label="路径" prop="path" v-bind="col(220, true)" show-overflow-tooltip class-name="mono" />
      <el-table-column label="状态" prop="status" v-bind="col(80)" class-name="mono" />
      <el-table-column label="来源 IP" prop="ip" v-bind="col(140)" class-name="mono" v-if="!hideColP2" />
    </el-table>
    <pre v-else class="logbox">{{ text || "（暂无内容）" }}</pre>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { useSegmentIndicator } from "../composables/useSegmentIndicator";
import { computed, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
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

const route = useRoute();
const router = useRouter();
const auth = useAuthStore();

/// 分段单选的滑动指示块：把「第几段选中的 / 段多宽」写成 CSS 变量（详见 composable）
const modeGroup = ref<HTMLElement | null>(null);
useSegmentIndicator(modeGroup);

/** 从实例卡片跳进来时带的实例 id（形如 container:<短ID> / service:<单元名>） */
const instance = ref(String(route.query.instance ?? ""));
const isContainer = computed(() => instance.value.startsWith("container:"));
const isService = computed(() => instance.value.startsWith("service:"));
/** 实例限定下只有一种日志来源，不给切换项 */
const scoped = computed(() => isContainer.value || isService.value);
const containerShort = computed(() =>
  isContainer.value ? instance.value.slice("container:".length) : "",
);
const serviceUnit = computed(() =>
  isService.value ? instance.value.slice("service:".length) : "",
);

const mode = ref<"journal" | "file" | "audit" | "container">(
  isContainer.value ? "container" : "journal",
);
const unit = ref(serviceUnit.value);
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

/** 清掉实例限定，回到常规日志视图 */
function clearInstance() {
  instance.value = "";
  mode.value = "journal";
  unit.value = "";
  router.replace({ path: "/logs" });
  text.value = "";
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
    // 容器日志由 docker 提供，不是某个文件也不是 journal 单元
    if (mode.value === "container") {
      const { data } = await http.get("/docker/logs", {
        params: { id: containerShort.value, tail: lines.value },
      });
      text.value = data.logs || "（无日志）";
      return;
    }
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

// 从另一个实例跳过来时重新限定（组件复用时 query 变了但不会重新挂载）
watch(
  () => route.query.instance,
  (v) => {
    instance.value = String(v ?? "");
    mode.value = isContainer.value ? "container" : "journal";
    unit.value = serviceUnit.value;
    text.value = "";
    if (scoped.value) load();
  },
);

onMounted(async () => {
  await loadFiles();
  // 从实例进来就该直接看到日志，而不是一个空框
  if (scoped.value) await load();
});
</script>

<style scoped>
.logbox {
  background: var(--panel-card-bg, var(--el-bg-color));
  border-radius: var(--radius);
  /* 与卡片 / 表格同档：整块白底从页面底色上托起，否则日志区一片白贴着底色 */
  box-shadow: var(--panel-shadow-1);
  padding: var(--sp-3) var(--sp-4);
  height: var(--panel-table-height);
  overflow: auto;
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  font-size: var(--fs-sm);
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 0;
}
</style>
