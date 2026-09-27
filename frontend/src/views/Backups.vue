<template>
  <div class="backups">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button :loading="creating" @click="createNow">立即备份</el-button>
      <span class="hint">每日自动备份，保留最近 {{ keep }} 份；恢复需重启服务生效</span>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      height="var(--panel-table-height)"
    >
      <el-table-column label="文件名" prop="name" min-width="240">
        <template #default="{ row }">
          <span class="mono">{{ row.name }}</span>
        </template>
      </el-table-column>
      <el-table-column label="大小" width="120">
        <template #default="{ row }">{{ fmtSize(row.size) }}</template>
      </el-table-column>
      <el-table-column label="创建时间" width="180" class-name="col-p2" label-class-name="col-p2">
        <template #default="{ row }">{{ fmtTime(row.mtime) }}</template>
      </el-table-column>
      <el-table-column label="操作" width="180" align="right">
        <template #default="{ row }">
          <el-button link size="small" @click="download(row.name)">下载</el-button>
          <el-button link size="small" @click="restore(row.name)">恢复</el-button>
          <el-button link size="small" @click="remove(row.name)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import http from "../api/http";

interface BackupInfo {
  name: string;
  size: number;
  mtime: number;
}

const keep = 7;
const rows = ref<BackupInfo[]>([]);
const loading = ref(false);
const creating = ref(false);

function fmtSize(n: number) {
  if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + " MB";
  if (n >= 1024) return (n / 1024).toFixed(1) + " KB";
  return n + " B";
}

function fmtTime(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/backups");
    rows.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取备份列表失败");
  } finally {
    loading.value = false;
  }
}

async function createNow() {
  creating.value = true;
  try {
    const { data } = await http.post("/backups");
    ElMessage.success(`已创建备份 ${data.name}`);
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "备份失败");
  } finally {
    creating.value = false;
  }
}

async function download(name: string) {
  try {
    const resp = await http.get("/backups/download", {
      params: { name },
      responseType: "blob",
    });
    const url = URL.createObjectURL(resp.data);
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    a.click();
    URL.revokeObjectURL(url);
  } catch (e: any) {
    ElMessage.error("下载失败");
  }
}

async function restore(name: string) {
  try {
    await ElMessageBox.confirm(
      `将用备份 ${name} 整体覆盖当前数据库，重启面板服务后生效。确定继续？`,
      "恢复确认",
      { type: "warning", confirmButtonText: "恢复", cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  try {
    const { data } = await http.post("/backups/restore", { name });
    ElMessage.success(data.message ?? "恢复已登记，重启服务后生效");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "登记恢复失败");
  }
}

async function remove(name: string) {
  try {
    await ElMessageBox.confirm(`确定删除备份 ${name}？`, "删除确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete("/backups", { data: { name } });
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
</style>
