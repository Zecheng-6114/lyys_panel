<template>
  <div class="logs">
    <div class="toolbar">
      <el-radio-group v-model="mode">
        <el-radio-button value="journal">系统日志</el-radio-button>
        <el-radio-button value="file">文件日志</el-radio-button>
      </el-radio-group>
      <el-input
        v-if="mode === 'journal'"
        v-model="unit"
        placeholder="单元名（如 sshd，留空为全部）"
        clearable
        style="width: 240px"
      />
      <el-select
        v-else
        v-model="file"
        placeholder="选择 /var/log 下的文件"
        style="width: 240px"
        filterable
      >
        <el-option v-for="f in files" :key="f" :label="f" :value="'/var/log/' + f" />
      </el-select>
      <el-input-number v-model="lines" :min="50" :max="2000" :step="100" />
      <el-button @click="load">加载</el-button>
    </div>
    <pre class="logbox">{{ text || "（暂无内容）" }}</pre>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import http from "../api/http";

const mode = ref<"journal" | "file">("journal");
const unit = ref("");
const file = ref("");
const files = ref<string[]>([]);
const lines = ref(200);
const text = ref("");

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
  border-radius: 6px;
  padding: 12px 16px;
  height: var(--panel-table-height);
  overflow: auto;
  font-family: var(--panel-mono);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 0;
}
</style>
