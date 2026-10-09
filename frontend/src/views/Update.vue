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
        <span class="mono">{{ status.latest ?? "—" }}</span>
        <span v-if="status.error" class="err">{{ status.error }}</span>
        <span v-else-if="status.has_update" class="dot-wrap"
          ><i class="dot dot-on" />有新版本可用</span
        >
        <span v-else class="dot-wrap"><i class="dot dot-off" />已是最新</span>
      </div>
      <div v-if="status?.has_update && !status.error" class="actions">
        <el-button type="primary" :loading="installing" @click="install">下载并安装</el-button>
        <span class="hint">安装完成后需重启面板服务生效（右上角电源菜单）</span>
      </div>
    </div>

    <div class="section-title">手动上传（内网环境旁路）</div>
    <div class="upload-row">
      <el-button @click="pickFile">选择文件</el-button>
      <input
        ref="fileInput"
        type="file"
        style="display: none"
        @change="onFileChange"
      />
      <span class="filename mono">{{ fileName || "未选择文件" }}</span>
      <el-button :loading="uploading" :disabled="!fileName" @click="upload">
        上传并安装
      </el-button>
    </div>
    <div class="hint">
      选择在其他机器构建的 lyys-panel 二进制（Linux x86_64 或 aarch64，需与本机
      架构一致），服务端校验 ELF 格式与体积后原子替换，重启服务后生效。
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

/// 安装成功后的收尾：询问是否立即重启面板。请求发出后进程会被
/// systemctl 重启，本请求可能因服务中断而失败——两种情况都轮询
/// health 直到服务恢复。
async function promptRestart() {
  try {
    await ElMessageBox.confirm(
      "新版本二进制已就位，是否立即重启面板服务生效？",
      "安装完成",
      {
        type: "warning",
        confirmButtonText: "立即重启",
        cancelButtonText: "稍后手动重启",
      },
    );
  } catch {
    ElMessage.info("已安装，稍后可通过右上角电源菜单重启面板生效");
    return;
  }
  http.post("/power", { action: "panel-restart" }).catch(() => {});
  // 给旧进程一点退出时间，再开始探测
  await new Promise((r) => setTimeout(r, 1500));
  for (let i = 0; i < 20; i++) {
    try {
      await http.get("/health", { timeout: 2000 });
      ElMessage.success("面板已重启，新版本已生效");
      return;
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  ElMessage.warning("重启指令已发出，若页面无法访问请稍后刷新");
}

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
      `将从 GitHub 下载 ${status.value?.latest} 并替换当前二进制，重启服务后生效。确定继续？`,
      "安装确认",
      { type: "warning", confirmButtonText: "安装", cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  installing.value = true;
  try {
    await http.post("/update/install", null, { timeout: 300000 });
    await promptRestart();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "安装失败");
  } finally {
    installing.value = false;
  }
}

function pickFile() {
  fileInput.value?.click();
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
    await http.post("/update/upload", buf, {
      headers: { "Content-Type": "application/octet-stream" },
      timeout: 300000,
    });
    await promptRestart();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "上传失败");
  } finally {
    uploading.value = false;
  }
}
</script>

<style scoped>
/* 间距统一：区块间 16px、块内 12px、行内 8px 三档 */
.status-block {
  background: var(--panel-card-bg, var(--el-bg-color));
  border-radius: var(--radius);
  /* 白底实心块，与卡片 / 表格同档（不加影就整片贴在页面底色上） */
  box-shadow: var(--panel-shadow-1);
  padding: var(--sp-4) var(--sp-5);
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
  margin-bottom: var(--sp-4);
}
.status-row {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  font-size: var(--fs-base);
}
.label {
  color: var(--el-text-color-secondary);
  width: 64px;
  flex: none;
}
.err {
  color: var(--el-text-color-secondary);
  font-size: var(--fs-sm);
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-2);
}
.actions {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
.section-title {
  font-size: var(--fs-md);
  font-weight: 600;
  /* 父容器不是带 gap 的 flex，标题上方留白得自己带（R4） */
  margin: var(--sp-4) 0 var(--sp-2);
}
.upload-row {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  margin-bottom: var(--sp-3);
}
.filename {
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.hint {
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
}
</style>
