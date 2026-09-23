<template>
  <div class="dk">
    <!-- 未安装（或只装了守护进程缺 CLI）：给出安装入口 -->
    <div v-if="status && !status.installed" class="panel">
      <div class="panel-title">
        {{ status.daemon ? "Docker 命令行工具缺失" : "未检测到 Docker" }}
      </div>
      <p class="hint">
        <template v-if="status.daemon">
          已检测到 Docker 守护进程正在运行，但缺 <code>docker-cli</code> 包
          （Debian 把它列为 <code>docker.io</code> 的推荐包，容易被漏装）。
          点安装即可装上缺的部分，已装好的不会被重装。
        </template>
        <template v-else>
          这台机器上还没有 Docker。点下面的按钮会执行
          <code>apt-get install docker.io docker-cli docker-compose</code>
          并启动服务，通常耗时一到三分钟。
        </template>
      </p>
      <el-button type="primary" :loading="installing" @click="install">
        {{ installing ? "安装中…" : "安装 Docker" }}
      </el-button>
      <pre v-if="installLog" class="log-box">{{ installLog }}</pre>
    </div>

    <!-- 已装但守护进程没起来 -->
    <div v-else-if="status && !status.running" class="panel">
      <div class="panel-title">Docker 未运行</div>
      <p class="hint">{{ status.error }}</p>
      <el-button type="primary" :loading="starting" @click="startDaemon">
        启动 docker 服务
      </el-button>
    </div>

    <template v-else>
      <el-tabs v-model="tab" class="dk-tabs">
        <!-- 容器 -->
        <el-tab-pane label="容器" name="containers">
          <div class="toolbar">
            <el-input
              v-model="cKeyword"
              placeholder="搜索容器名或镜像"
              clearable
              style="width: 240px"
            />
            <el-button @click="loadContainers">刷新</el-button>
          </div>
          <el-table
            :data="filteredContainers"
            height="calc(100vh - 220px)"
            size="small"
          >
            <el-table-column label="状态" width="90">
              <template #default="{ row }">
                <span :class="['dot', row.state === 'running' ? 'dot-on' : 'dot-off']" />
                {{ row.state }}
              </template>
            </el-table-column>
            <el-table-column prop="name" label="名称" min-width="160" />
            <el-table-column prop="image" label="镜像" min-width="180" show-overflow-tooltip />
            <el-table-column prop="status" label="运行时长" width="150" />
            <el-table-column prop="ports" label="端口" min-width="160" show-overflow-tooltip />
            <el-table-column prop="cpu" label="CPU" width="90" />
            <el-table-column prop="mem" label="内存" width="140" />
            <el-table-column label="操作" width="200">
              <template #default="{ row }">
                <el-button link size="small" @click="containerAct(row, 'start')">启动</el-button>
                <el-button link size="small" @click="containerAct(row, 'stop')">停止</el-button>
                <el-button link size="small" @click="containerAct(row, 'restart')">重启</el-button>
                <el-button link size="small" @click="openLogs(row)">日志</el-button>
                <el-button link size="small" @click="containerAct(row, 'remove')">删除</el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-tab-pane>

        <!-- 镜像 -->
        <el-tab-pane label="镜像" name="images">
          <div class="toolbar">
            <el-input
              v-model="pullName"
              placeholder="镜像名，如 nginx:alpine"
              style="width: 280px"
            />
            <el-button :loading="pulling" @click="pullImage">拉取</el-button>
            <el-button @click="loadImages">刷新</el-button>
          </div>
          <el-table :data="images" height="calc(100vh - 220px)" size="small">
            <el-table-column prop="repository" label="仓库" min-width="180" />
            <el-table-column prop="tag" label="标签" width="120" />
            <el-table-column prop="id" label="镜像 ID" width="140" />
            <el-table-column prop="size" label="大小" width="110" />
            <el-table-column prop="created" label="创建于" width="140" />
            <el-table-column label="操作" width="90">
              <template #default="{ row }">
                <el-button link size="small" @click="removeImage(row)">删除</el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-tab-pane>

        <!-- Compose -->
        <el-tab-pane label="Compose" name="compose">
          <div class="toolbar">
            <span v-if="status" class="hint-inline">
              compose 形式：{{ status.compose === "v2" ? "docker compose（CLI 插件）"
                : status.compose === "v1" ? "docker-compose（独立命令，v2 内核）" : "不可用" }}
            </span>
            <el-button @click="loadCompose">刷新</el-button>
          </div>
          <el-table :data="projects" height="calc(100vh - 220px)" size="small">
            <el-table-column prop="name" label="项目" min-width="180" />
            <el-table-column prop="status" label="状态" width="180" />
            <el-table-column prop="containers" label="容器数" width="90" />
            <el-table-column prop="config_files" label="配置文件" min-width="220" show-overflow-tooltip />
            <el-table-column label="操作" width="180">
              <template #default="{ row }">
                <el-button link size="small" @click="composeAct(row, 'up')">启动</el-button>
                <el-button link size="small" @click="composeAct(row, 'restart')">重启</el-button>
                <el-button link size="small" @click="composeAct(row, 'down')">停止并移除</el-button>
              </template>
            </el-table-column>
          </el-table>
        </el-tab-pane>
      </el-tabs>
    </template>

    <el-dialog v-model="logOpen" :title="`日志 · ${logName}`" width="70%">
      <pre class="log-box">{{ logText }}</pre>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import http from "../api/http";

interface DockerStatus {
  installed: boolean;
  running: boolean;
  daemon: boolean;
  version: string;
  compose: string;
  error: string;
}
interface Container {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  ports: string;
  running_for: string;
  cpu: string;
  mem: string;
  project: string;
}
interface Image {
  id: string;
  repository: string;
  tag: string;
  size: string;
  created: string;
}
interface Project {
  name: string;
  status: string;
  config_files: string;
  working_dir: string;
  containers: number;
}

const status = ref<DockerStatus | null>(null);
const tab = ref("containers");

const containers = ref<Container[]>([]);
const cKeyword = ref("");
const images = ref<Image[]>([]);
const projects = ref<Project[]>([]);
const pullName = ref("");

const installing = ref(false);
const installLog = ref("");
const starting = ref(false);
const pulling = ref(false);

const logOpen = ref(false);
const logName = ref("");
const logText = ref("");

const filteredContainers = computed(() => {
  const k = cKeyword.value.trim().toLowerCase();
  if (!k) return containers.value;
  return containers.value.filter(
    (c) => c.name.toLowerCase().includes(k) || c.image.toLowerCase().includes(k),
  );
});

async function loadStatus() {
  const { data } = await http.get("/docker/status");
  status.value = data;
}

async function install() {
  installing.value = true;
  installLog.value = "";
  try {
    const { data } = await http.post("/docker/install");
    installLog.value = data.output || "安装完成";
    ElMessage.success("Docker 安装完成");
    await loadStatus();
    await loadAll();
  } catch (e: any) {
    installLog.value = e.response?.data?.error ?? "安装失败";
    ElMessage.error("安装失败");
  } finally {
    installing.value = false;
  }
}

// 守护进程没起来时，直接复用服务页的 systemctl 接口启动它
async function startDaemon() {
  starting.value = true;
  try {
    await http.post("/services", { name: "docker", action: "start" });
    await loadStatus();
    await loadAll();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "启动失败");
  } finally {
    starting.value = false;
  }
}

async function loadContainers() {
  const { data } = await http.get("/docker/containers");
  containers.value = data;
}

async function loadImages() {
  const { data } = await http.get("/docker/images");
  images.value = data;
}

async function loadCompose() {
  const { data } = await http.get("/docker/compose");
  projects.value = data;
}

async function loadAll() {
  await Promise.all([loadContainers(), loadImages(), loadCompose()]);
}

async function containerAct(row: Container, action: string) {
  // 删除是不可逆操作，先确认
  if (action === "remove") {
    try {
      await ElMessageBox.confirm(`删除容器 ${row.name}？`, "确认", {
        type: "warning",
        confirmButtonText: "删除",
        cancelButtonText: "取消",
      });
    } catch {
      return;
    }
  }
  try {
    await http.post("/docker/container/action", { id: row.id, action });
    ElMessage.success("操作成功");
    await loadContainers();
    if (tab.value === "compose") await loadCompose();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  }
}

async function openLogs(row: Container) {
  logName.value = row.name;
  logText.value = "加载中…";
  logOpen.value = true;
  try {
    const { data } = await http.get("/docker/logs", {
      params: { id: row.id, tail: 300 },
    });
    logText.value = data.logs || "（无日志）";
  } catch (e: any) {
    logText.value = e.response?.data?.error ?? "读取日志失败";
  }
}

async function pullImage() {
  const name = pullName.value.trim();
  if (!name) {
    ElMessage.warning("请输入镜像名");
    return;
  }
  pulling.value = true;
  try {
    await http.post("/docker/image/action", { action: "pull", target: name });
    ElMessage.success("拉取完成");
    pullName.value = "";
    await loadImages();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "拉取失败");
  } finally {
    pulling.value = false;
  }
}

async function removeImage(row: Image) {
  try {
    await ElMessageBox.confirm(`删除镜像 ${row.repository}:${row.tag}？`, "确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.post("/docker/image/action", { action: "remove", target: row.id });
    ElMessage.success("已删除");
    await loadImages();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

async function composeAct(row: Project, action: string) {
  try {
    await http.post("/docker/compose/action", { name: row.name, action });
    ElMessage.success("操作成功");
    await Promise.all([loadCompose(), loadContainers()]);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  }
}

// 切页签时按需加载，避免首屏发一堆请求
watch(tab, (v) => {
  if (!status.value?.running) return;
  if (v === "images") loadImages();
  if (v === "compose") loadCompose();
});

onMounted(async () => {
  await loadStatus();
  if (status.value?.running) await loadAll();
});
</script>

<style scoped>
.panel {
  padding: 24px;
  border-radius: var(--radius);
  background: var(--el-bg-color-page);
}
.panel-title {
  font-size: 16px;
  font-weight: 600;
  margin-bottom: 8px;
}
.hint {
  color: var(--el-text-color-secondary);
  line-height: 1.7;
  margin: 0 0 16px;
}
.hint-inline {
  color: var(--el-text-color-secondary);
  font-size: 13px;
  margin-right: 12px;
}
.log-box {
  max-height: 50vh;
  overflow: auto;
  margin: 12px 0 0;
  padding: 12px;
  border-radius: var(--radius);
  background: var(--el-fill-color-light);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
}
.dk-tabs {
  margin-top: -8px;
}
</style>
