<template>
  <div class="update">
    <div class="toolbar">
      <el-button :loading="checking" @click="check">检查更新</el-button>
      <span class="hint">更新源：GitHub Release（Zecheng-6114/lyys_panel）</span>
    </div>

    <div class="status-block">
      <div class="status-row">
        <span class="label">当前版本</span>
        <span class="mono">v{{ status?.current ?? appVersion }}</span>
      </div>
      <div v-if="status" class="status-row">
        <span class="label">最新版本</span>
        <span class="mono">{{ status.latest ? "v" + status.latest : "—" }}</span>
        <span v-if="status.error" class="err">{{ status.error }}</span>
        <span v-else-if="status.has_update" class="dot-wrap"
          ><i class="dot dot-on" />有新版本可用</span
        >
        <span v-else class="dot-wrap"><i class="dot dot-off" />已是最新</span>
      </div>
      <div v-if="status?.has_update && !status.error" class="actions">
        <el-button :loading="installing" @click="install">下载并安装</el-button>
        <span class="hint">安装完成后需重启面板服务生效</span>
      </div>
    </div>

    <div class="section-title">手动上传（内网环境旁路）</div>
    <div class="upload-row">
      <input
        ref="fileInput"
        type="file"
        accept=".bin,application/octet-stream"
        @change="onFileChange"
      />
      <el-button :loading="uploading" :disabled="!fileName" @click="upload">
        上传并安装
      </el-button>
    </div>
    <div class="hint">
      选择在其他机器构建的 lyys-panel 二进制（Linux x86_64），服务端校验 ELF
      格式与体积后原子替换，重启服务后生效。
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref } from "vue";
import http from "../api/http";

interface UpdateStatus {
  current: string;
  latest: string | null;
  has_update: boolean;
  error: string | null;
}

const appVersion = __APP_VERSION__;
const status = ref<UpdateStatus | null>(null);
const checking = ref(false);
const installing = ref(false);
const uploading = ref(false);
const fileInput = ref<HTMLInputElement | null>(null);
const fileName = ref("");

async function check() {
  checking.value = true;
  try {
    const { data } = await http.get("/update/check");
    status.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "检查更新失败");
  } finally {
    checking.value = false;
  }
}

async function install() {
  try {
    await ElMessageBox.confirm(
      `将从 GitHub 下载 v${status.value?.latest} 并替换当前二进制，重启服务后生效。确定继续？`,
      "安装确认",
      { type: "warning", confirmButtonText: "安装", cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  installing.value = true;
  try {
    const { data } = await http.post("/update/install", null, { timeout: 300000 });
    ElMessage.success(data.message ?? "已安装，重启服务后生效");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "安装失败");
  } finally {
    installing.value = false;
  }
}

function onFileChange() {
  const f = fileInput.value?.files?.[0];
  fileName.value = f ? f.name : "";
}

async function upload() {
  const f = fileInput.value?.files?.[0];
  if (!f) return;
  try {
    await ElMessageBox.confirm(
      `将用上传的 ${f.name} 替换当前二进制，重启服务后生效。确定继续？`,
      "安装确认",
      { type: "warning", confirmButtonText: "安装", cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  uploading.value = true;
  try {
    const buf = await f.arrayBuffer();
    const { data } = await http.post("/update/upload", buf, {
      headers: { "Content-Type": "application/octet-stream" },
      timeout: 300000,
    });
    ElMessage.success(data.message ?? "已安装，重启服务后生效");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "上传失败");
  } finally {
    uploading.value = false;
  }
}
</script>

<style scoped>
.status-block {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-bottom: 20px;
}
.status-row {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
}
.label {
  color: var(--el-text-color-secondary);
  width: 64px;
  flex: none;
}
.err {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.actions {
  display: flex;
  align-items: center;
  gap: 12px;
}
.section-title {
  font-size: 14px;
  font-weight: 500;
  margin-bottom: 10px;
}
.upload-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 8px;
}
.upload-row input[type="file"] {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
</style>
