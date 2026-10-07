<template>
  <div class="backups">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button :loading="creating" @click="createNow">立即备份</el-button>
      <el-button :loading="uploading" @click="openUpload">上传导入</el-button>
      <el-button :loading="remoteBusy" @click="uploadRemote">上传最新到远端</el-button>
      <el-button @click="openConfig">设置</el-button>
      <span class="hint">每日自动备份，保留最近 {{ cfg.keep }} 份；恢复需重启服务生效</span>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      height="var(--panel-table-height)"
    >
      <el-table-column label="文件名" prop="name" v-bind="col(240, true)">
        <template #default="{ row }">
          <span class="mono">{{ row.name }}</span>
        </template>
      </el-table-column>
      <el-table-column label="大小" v-bind="col(120)">
        <template #default="{ row }">{{ fmtSize(row.size) }}</template>
      </el-table-column>
      <el-table-column label="创建时间" v-bind="col(180)" v-if="!hideColP2">
        <template #default="{ row }">{{ fmtTime(row.mtime) }}</template>
      </el-table-column>
      <el-table-column label="操作" v-bind="col(180)" align="right">
        <template #default="{ row }">
          <el-button link size="small" @click="download(row.name)">下载</el-button>
          <el-button link size="small" @click="restore(row.name)">恢复</el-button>
          <el-button link size="small" @click="remove(row.name)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-dialog v-model="showConfig" title="备份设置" width="520px">
      <el-form label-width="110px" size="small">
        <el-form-item label="备份目录">
          <el-input v-model="form.dir" placeholder="留空 = 数据目录下 backups/" />
        </el-form-item>
        <el-form-item label="保留份数">
          <el-input-number v-model="form.keep" :min="1" :max="100" style="width: 100%" />
        </el-form-item>

        <el-divider content-position="left">远端备份（WebDAV）</el-divider>
        <el-form-item label="启用">
          <el-switch v-model="form.remote.enabled" />
        </el-form-item>
        <template v-if="form.remote.enabled">
          <el-form-item label="WebDAV 地址">
            <el-input v-model="form.remote.url" placeholder="https://dav.example.com/panel/" />
          </el-form-item>
          <el-form-item label="用户名">
            <el-input v-model="form.remote.username" placeholder="可留空（匿名或令牌在地址里）" />
          </el-form-item>
          <el-form-item label="密码">
            <el-input
              v-model="form.remote.password"
              type="password"
              show-password
              :placeholder="cfg.remote.password_set ? '已设置，留空则不修改' : '可留空'"
            />
          </el-form-item>
          <el-form-item label="上传加密">
            <el-switch v-model="form.remote.encrypt" />
          </el-form-item>
          <el-form-item v-if="form.remote.encrypt" label="加密口令">
            <el-input
              v-model="form.remote.passphrase"
              type="password"
              show-password
              :placeholder="
                cfg.remote.passphrase_set ? '已设置，留空则不修改' : '至少 8 位，导入时需用它解密'
              "
            />
          </el-form-item>
        </template>
      </el-form>
      <div class="hint">
        备份目录须为绝对路径，修改后新备份写入新目录，旧目录的备份不会迁移。远端为
        WebDAV 目录（Nextcloud / 群晖 / 坚果云等），每次备份后自动 PUT 一份过去；启用加密
        时远端只留密文，读回需在「上传导入」里填口令。
      </div>
      <template #footer>
        <el-button @click="showConfig = false">取消</el-button>
        <el-button type="primary" :loading="saving" @click="saveConfig">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showUpload" title="上传导入备份" width="460px">
      <el-form label-width="90px" size="small">
        <el-form-item label="解密口令">
          <el-input
            v-model="uploadPass"
            type="password"
            show-password
            placeholder="上传的是 .enc 加密副本时填写，普通 .db 留空"
          />
        </el-form-item>
      </el-form>
      <div class="hint">普通 .db 备份直接选文件即可；从远端下载的加密副本必须填对口令。</div>
      <input ref="fileEl" type="file" accept=".db,.enc" style="display: none" @change="onFile" />
      <template #footer>
        <el-button @click="showUpload = false">取消</el-button>
        <el-button type="primary" :loading="uploading" @click="pickFile">选择文件并导入</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";
import { submitJob } from "../api/jobs";
import { useJobsStore } from "../stores/jobs";

interface BackupInfo {
  name: string;
  size: number;
  mtime: number;
}

const jobWatch = useJobsStore();
const rows = ref<BackupInfo[]>([]);
const loading = ref(false);
const creating = ref(false);
const uploading = ref(false);
const saving = ref(false);
const fileEl = ref<HTMLInputElement | null>(null);

/** 生效中的配置（密钥只回「是否已设置」，用于占位提示） */
const cfg = reactive({
  dir: "",
  keep: 7,
  remote: {
    enabled: false,
    url: "",
    username: "",
    password_set: false,
    encrypt: false,
    passphrase_set: false,
  },
});
/** 设置弹窗表单草稿（密码/口令为空 = 沿用已存值） */
const form = reactive({
  dir: "",
  keep: 7,
  remote: {
    enabled: false,
    url: "",
    username: "",
    password: "",
    encrypt: false,
    passphrase: "",
  },
});
const showConfig = ref(false);
const showUpload = ref(false);
const uploadPass = ref("");
const remoteBusy = ref(false);

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

/** 备份改为后台作业（P2-1）：VACUUM 在大库上耗时较久，不该让请求挂着等 */
async function createNow() {
  creating.value = true;
  try {
    const id = await submitJob("backup_create");
    // 登记：跑完会弹通知，不用守着「任务」页
    jobWatch.watch(id);
    ElMessage.success("已加入任务队列，可在「任务」页查看进度");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "提交备份失败");
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

/** 把后端配置视图写回 cfg（远端密码/口令只回「是否已设置」，不回明文） */
function applyView(data: any) {
  cfg.dir = data.dir ?? "";
  cfg.keep = data.keep ?? 7;
  const r = data.remote ?? {};
  cfg.remote.enabled = !!r.enabled;
  cfg.remote.url = r.url ?? "";
  cfg.remote.username = r.username ?? "";
  cfg.remote.password_set = !!r.password_set;
  cfg.remote.encrypt = !!r.encrypt;
  cfg.remote.passphrase_set = !!r.passphrase_set;
}

async function loadConfig() {
  try {
    const { data } = await http.get("/backups/config");
    applyView(data);
  } catch {
    // 读取失败保留默认值，不打扰用户
  }
}

function openConfig() {
  form.dir = cfg.dir;
  form.keep = cfg.keep;
  form.remote.enabled = cfg.remote.enabled;
  form.remote.url = cfg.remote.url;
  form.remote.username = cfg.remote.username;
  form.remote.encrypt = cfg.remote.encrypt;
  // 密码/口令留空 = 沿用已存值
  form.remote.password = "";
  form.remote.passphrase = "";
  showConfig.value = true;
}

async function saveConfig() {
  saving.value = true;
  try {
    const { data } = await http.post("/backups/config", {
      dir: form.dir,
      keep: form.keep,
      remote: { ...form.remote },
    });
    applyView(data.config);
    showConfig.value = false;
    ElMessage.success("已保存");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  } finally {
    saving.value = false;
  }
}

/** 打开「上传导入」弹窗，口令每次重置 */
function openUpload() {
  uploadPass.value = "";
  showUpload.value = true;
}

/** 把最新一份本地备份立即投递到远端（需先在设置中启用） */
async function uploadRemote() {
  remoteBusy.value = true;
  try {
    const { data } = await http.post("/backups/remote/upload");
    ElMessage.success(`已上传 ${data.name} 到远端`);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "远端上传失败");
  } finally {
    remoteBusy.value = false;
  }
}

function pickFile() {
  fileEl.value?.click();
}

async function onFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  uploading.value = true;
  try {
    const fd = new FormData();
    fd.append("file", file);
    if (uploadPass.value.trim()) fd.append("passphrase", uploadPass.value.trim());
    const { data } = await http.post("/backups/upload", fd);
    ElMessage.success(`已导入 ${data.name}，可在列表中恢复`);
    showUpload.value = false;
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "导入失败");
  } finally {
    uploading.value = false;
  }
}

onMounted(() => {
  load();
  loadConfig();
});
</script>

<style scoped>
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-left: var(--sp-1);
}
</style>
