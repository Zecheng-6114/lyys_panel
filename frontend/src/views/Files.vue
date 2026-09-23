<template>
  <div class="files">
    <div class="toolbar">
      <div class="pathbar">
        <!-- 根目录 / 可点击回根；后续分隔符为纯文本，目录名单独可点击 -->
        <span class="crumb" @click="load('/')">/</span>
        <template v-for="(seg, i) in segments" :key="i">
          <span v-if="i > 0" class="sep">/</span>
          <span class="crumb" @click="go(segments.slice(0, i + 1).join('/'))">{{ seg }}</span>
        </template>
      </div>
      <div class="spacer"></div>
      <div v-if="uploading" class="upload-progress">
        <span>上传 {{ uploadDone }}/{{ uploadTotal }}</span>
        <el-progress
          :percentage="uploadPercent"
          :stroke-width="6"
          :show-text="false"
          class="upload-bar"
        />
      </div>
      <el-button @click="load(current)">刷新</el-button>
      <el-button @click="showMkdir = true">新建目录</el-button>
      <el-button @click="showNewFile = true">新建文件</el-button>
      <el-button @click="pickUpload">上传</el-button>
      <input
        ref="fileInput"
        type="file"
        multiple
        style="display: none"
        @change="onUpload"
      />
    </div>

    <el-table
      v-loading="loading"
      :data="entries"
      size="small"
      row-key="path"
      class="ftable"
      height="var(--panel-table-height)"
      :row-class-name="rowClassName"
      @row-click="onRowClick"
    >
      <el-table-column label="名称" min-width="260">
        <template #default="{ row }">
          <span :class="row.is_dir ? 'dirname' : ''">{{ row.name }}</span>
          <span v-if="row.is_symlink" class="linkmark">@</span>
        </template>
      </el-table-column>
      <el-table-column label="大小" width="110" align="right">
        <template #default="{ row }">{{ row.is_dir ? "-" : fmtSize(row.size) }}</template>
      </el-table-column>
      <el-table-column label="权限" width="90">
        <template #default="{ row }">{{ row.mode }}</template>
      </el-table-column>
      <el-table-column label="修改时间" width="170">
        <template #default="{ row }">{{ fmtTime(row.mtime) }}</template>
      </el-table-column>
      <el-table-column label="操作" width="230" align="right">
        <template #default="{ row }">
          <el-button v-if="!row.is_dir" link size="small" @click="editFile(row)">编辑</el-button>
          <el-button v-if="!row.is_dir" link size="small" @click="downloadFile(row)">下载</el-button>
          <el-button link size="small" @click="startRename(row)">重命名</el-button>
          <el-button link size="small" @click="removeEntry(row)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-dialog v-model="showMkdir" title="新建目录" width="440px">
      <el-input v-model="mkdirName" placeholder="目录名（当前目录下）" @keyup.enter="doMkdir" />
      <template #footer>
        <el-button @click="showMkdir = false">取消</el-button>
        <el-button @click="doMkdir">创建</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showNewFile" title="新建文件" width="560px">
      <el-input v-model="newFileName" placeholder="文件名（当前目录下）" />
      <el-input
        v-model="newFileContent"
        type="textarea"
        :rows="10"
        placeholder="文件内容（可为空）"
        style="margin-top: 10px"
      />
      <template #footer>
        <el-button @click="showNewFile = false">取消</el-button>
        <el-button @click="doNewFile">创建</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showEdit" :title="editPath" width="760px" top="6vh">
      <el-input v-model="editContent" type="textarea" :rows="22" class="editor" />
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button :loading="saving" @click="doSave">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showRename" title="重命名" width="440px">
      <el-input v-model="renameTo" placeholder="新名称" @keyup.enter="doRename" />
      <template #footer>
        <el-button @click="showRename = false">取消</el-button>
        <el-button @click="doRename">确定</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import http from "../api/http";

interface FEntry {
  name: string;
  path: string;
  is_dir: boolean;
  is_symlink: boolean;
  size: number;
  mode: string;
  mtime: number;
}

const current = ref("/");
const entries = ref<FEntry[]>([]);
const loading = ref(false);

const showMkdir = ref(false);
const mkdirName = ref("");
const showNewFile = ref(false);
const newFileName = ref("");
const newFileContent = ref("");
const showEdit = ref(false);
const editPath = ref("");
const editContent = ref("");
const saving = ref(false);
const showRename = ref(false);
const renameFrom = ref("");
const renameTo = ref("");
const fileInput = ref<HTMLInputElement>();

// 上传进度：按文件数 + 当前文件已上传字节数计算总进度
const uploading = ref(false);
const uploadTotal = ref(0);
const uploadDone = ref(0);
const uploadPercent = ref(0);

const segments = computed(() => current.value.split("/").filter((s) => s.length > 0));

function joinPath(dir: string, name: string) {
  return dir === "/" ? `/${name}` : `${dir}/${name}`;
}

async function load(path: string) {
  loading.value = true;
  try {
    const { data } = await http.get("/files/list", { params: { path } });
    current.value = data.path;
    entries.value = data.entries;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取目录失败");
  } finally {
    loading.value = false;
  }
}

function go(path: string) {
  load("/" + path);
}

// 目录行加 dir-row 类，提示整行可点击进入
function rowClassName({ row }: { row: FEntry }) {
  return row.is_dir ? "dir-row" : "";
}

// 点击整行进入目录；点到操作列按钮上时不触发导航
function onRowClick(row: FEntry, _col: unknown, event: PointerEvent) {
  if (!row.is_dir) return;
  if ((event.target as HTMLElement).closest("button")) return;
  load(row.path);
}

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function fmtTime(ts: number): string {
  if (!ts) return "-";
  const d = new Date(ts * 1000);
  const p = (x: number) => String(x).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

async function doMkdir() {
  const name = mkdirName.value.trim();
  if (!name) return;
  try {
    await http.post("/files/mkdir", { path: joinPath(current.value, name) });
    showMkdir.value = false;
    mkdirName.value = "";
    ElMessage.success("已创建目录");
    load(current.value);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "创建目录失败");
  }
}

async function doNewFile() {
  const name = newFileName.value.trim();
  if (!name) return;
  try {
    await http.post("/files/write", {
      path: joinPath(current.value, name),
      content: newFileContent.value,
    });
    showNewFile.value = false;
    newFileName.value = "";
    newFileContent.value = "";
    ElMessage.success("已创建文件");
    load(current.value);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "创建文件失败");
  }
}

async function editFile(row: FEntry) {
  try {
    const { data } = await http.get("/files/read", { params: { path: row.path } });
    editPath.value = row.path;
    editContent.value = data.content;
    showEdit.value = true;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取文件失败");
  }
}

async function doSave() {
  saving.value = true;
  try {
    await http.post("/files/write", { path: editPath.value, content: editContent.value });
    showEdit.value = false;
    ElMessage.success("已保存");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  } finally {
    saving.value = false;
  }
}

function startRename(row: FEntry) {
  renameFrom.value = row.path;
  renameTo.value = row.name;
  showRename.value = true;
}

async function doRename() {
  const name = renameTo.value.trim();
  if (!name) return;
  const dir = renameFrom.value.slice(0, renameFrom.value.lastIndexOf("/")) || "/";
  try {
    await http.post("/files/rename", { from: renameFrom.value, to: joinPath(dir, name) });
    showRename.value = false;
    ElMessage.success("已重命名");
    load(current.value);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "重命名失败");
  }
}

async function removeEntry(row: FEntry) {
  try {
    await ElMessageBox.confirm(
      `确定删除${row.is_dir ? "目录（含全部内容）" : "文件"}：${row.name}？`,
      "删除确认",
      { type: "warning", confirmButtonText: "删除", cancelButtonText: "取消" }
    );
  } catch {
    return;
  }
  try {
    await http.post("/files/delete", { path: row.path });
    ElMessage.success("已删除");
    load(current.value);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

async function downloadFile(row: FEntry) {
  try {
    const resp = await http.get("/files/download", {
      params: { path: row.path },
      responseType: "blob",
    });
    const url = URL.createObjectURL(resp.data);
    const a = document.createElement("a");
    a.href = url;
    a.download = row.name;
    a.click();
    // 立即 revoke 会让部分浏览器下载中断，延后释放
    setTimeout(() => URL.revokeObjectURL(url), 5000);
  } catch {
    ElMessage.error("下载失败");
  }
}

function pickUpload() {
  fileInput.value?.click();
}

async function onUpload(ev: Event) {
  const input = ev.target as HTMLInputElement;
  const files = input.files;
  if (!files || files.length === 0) return;
  const list = Array.from(files);
  uploading.value = true;
  uploadTotal.value = list.length;
  uploadDone.value = 0;
  uploadPercent.value = 0;
  for (const f of list) {
    const form = new FormData();
    form.append("dir", current.value);
    form.append("file", f);
    try {
      await http.post("/files/upload", form, {
        // 上传不限时：大小上限由后端 100MB 限制兜底，超时只会让大文件必然失败
        timeout: 0,
        onUploadProgress: (e) => {
          if (!e.total) return;
          uploadPercent.value = Math.round(
            ((uploadDone.value + e.loaded / e.total) / uploadTotal.value) * 100,
          );
        },
      });
      ElMessage.success(`已上传 ${f.name}`);
    } catch (e: any) {
      ElMessage.error(e.response?.data?.error ?? `上传 ${f.name} 失败`);
    }
    uploadDone.value += 1;
    uploadPercent.value = Math.round((uploadDone.value / uploadTotal.value) * 100);
  }
  input.value = "";
  uploading.value = false;
  load(current.value);
}

onMounted(() => load("/"));
</script>

<style scoped>
.pathbar {
  font-size: 13px;
  font-family: var(--panel-mono);
  display: flex;
  align-items: center;
  min-width: 0;
  overflow: hidden;
  white-space: nowrap;
}
.crumb {
  cursor: pointer;
  padding: 3px 2px;
  border-radius: 4px;
  color: var(--el-text-color-regular);
}
.crumb:hover {
  color: var(--el-text-color-primary);
  background: var(--el-fill-color-light);
}
.sep {
  color: var(--el-text-color-secondary);
  user-select: none;
}
.upload-progress {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  white-space: nowrap;
}
.upload-bar {
  width: 140px;
}
.dirname {
  font-weight: 500;
}
.ftable :deep(.dir-row) {
  cursor: pointer;
}
.linkmark {
  margin-left: 4px;
  color: var(--el-text-color-secondary);
}
.editor :deep(.el-textarea__inner) {
  font-family: var(--panel-mono);
  font-size: 12px;
  line-height: 1.6;
}
</style>
