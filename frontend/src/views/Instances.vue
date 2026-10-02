<template>
  <div class="inst">
    <!-- Docker 未就绪时给引导，但不遮住服务实例列表：没装 Docker 的机器也要能看实例 -->
    <div v-if="status && !status.running" class="panel">
      <div class="panel-title">
        {{ status.installed ? "Docker 未运行" : status.daemon ? "Docker 命令行工具缺失" : "未检测到 Docker" }}
      </div>
      <p class="hint">
        <template v-if="status.installed">
          {{ status.error }}
        </template>
        <template v-else-if="status.daemon">
          已检测到 Docker 守护进程正在运行，但缺命令行工具（{{ cliPackageHint }}）。
          点安装即可装上缺的部分，已装好的不会被重装。
        </template>
        <template v-else>
          这台机器上还没有 Docker。点下面的按钮会执行
          <code>{{ installCmd }}</code>
          并启动服务，通常耗时一到三分钟。下面的实例列表不受影响。
        </template>
      </p>
      <el-button v-if="status.installed" type="primary" :loading="starting" @click="startDaemon">
        启动 docker 服务
      </el-button>
      <el-button v-else type="primary" :loading="installing" @click="install">
        {{ installing ? "提交中…" : "安装 Docker" }}
      </el-button>
      <p v-if="!status.installed" class="hint">
        安装与拉取都是后台作业，提交后可在「任务」页实时查看输出。
      </p>
    </div>

    <el-tabs v-model="tab" class="inst-tabs">
      <el-tab-pane label="实例" name="instances">
        <div class="toolbar">
          <el-input
            v-model="keyword"
            placeholder="搜索实例名 / 镜像 / 描述"
            clearable
            style="width: 260px"
          />
          <el-button @click="loadInstances">刷新</el-button>
          <span class="hint-inline">
            共 {{ instances.length }} 个（容器 {{ containerCount }} · 服务 {{ serviceCount }}）
          </span>
        </div>

        <p v-if="!filtered.length" class="empty">
          {{ instances.length ? "没有匹配的实例" : "暂无实例" }}
        </p>

        <div v-else class="inst-grid">
          <div v-for="i in filtered" :key="i.id" class="inst-card">
            <div class="inst-head">
              <span :class="['dot', i.state === 'running' ? 'dot-on' : 'dot-off']" />
              <span class="inst-name" :title="i.name">{{ i.name }}</span>
              <el-tag size="small" :type="i.kind === 'container' ? 'primary' : 'info'" effect="plain">
                {{ i.kind === "container" ? "容器" : "服务" }}
              </el-tag>
            </div>

            <div class="inst-detail" :title="i.detail">{{ i.detail }}</div>

            <div class="inst-meta">
              <span v-if="i.ports" :title="i.ports" class="meta-ports">{{ i.ports }}</span>
              <span>CPU {{ i.cpu }}</span>
              <span>内存 {{ i.mem }}</span>
              <span v-if="i.kind === 'service'">{{ i.pids.length }} 个进程</span>
            </div>

            <div class="inst-actions">
              <el-button v-if="i.kind === 'container'" link size="small" @click="openFiles(i)">
                文件管理
              </el-button>
              <el-button link size="small" @click="openLogs(i)">日志</el-button>
              <el-button link size="small" @click="openProcesses(i)">进程</el-button>
              <el-dropdown trigger="click" @command="(c: string) => instAct(i, c)">
                <el-button link size="small">
                  更多<el-icon><ArrowDown /></el-icon>
                </el-button>
                <template #dropdown>
                  <el-dropdown-menu>
                    <el-dropdown-item command="start">启动</el-dropdown-item>
                    <el-dropdown-item command="stop">停止</el-dropdown-item>
                    <el-dropdown-item command="restart">重启</el-dropdown-item>
                    <el-dropdown-item v-if="i.kind === 'container'" command="remove" divided>
                      删除
                    </el-dropdown-item>
                  </el-dropdown-menu>
                </template>
              </el-dropdown>
            </div>
          </div>
        </div>
      </el-tab-pane>

      <el-tab-pane label="镜像" name="images" :disabled="!dockerReady">
        <div class="toolbar">
          <el-input
            v-model="pullName"
            placeholder="镜像名，如 nginx:alpine"
            style="width: 280px"
          />
          <el-button :loading="pulling" @click="pullImage">拉取</el-button>
          <el-button @click="loadImages">刷新</el-button>
        </div>
        <el-table :data="images" height="calc(100vh - 240px)" size="small">
          <el-table-column prop="repository" label="仓库" min-width="180" />
          <el-table-column prop="tag" label="标签" width="120" />
          <el-table-column prop="id" label="镜像 ID" width="140" class-name="col-p2" label-class-name="col-p2" />
          <el-table-column prop="size" label="大小" width="110" />
          <el-table-column prop="created" label="创建于" width="140" class-name="col-p3" label-class-name="col-p3" />
          <el-table-column label="操作" width="90">
            <template #default="{ row }">
              <el-button link size="small" @click="removeImage(row)">删除</el-button>
            </template>
          </el-table-column>
        </el-table>
      </el-tab-pane>

      <el-tab-pane label="Compose" name="compose" :disabled="!dockerReady">
        <div class="toolbar">
          <span v-if="status" class="hint-inline">
            compose 形式：{{ status.compose === "v2" ? "docker compose（CLI 插件）"
              : status.compose === "v1" ? "docker-compose（独立命令，v2 内核）" : "不可用" }}
          </span>
          <el-button @click="loadCompose">刷新</el-button>
        </div>
        <el-table :data="projects" height="calc(100vh - 240px)" size="small">
          <el-table-column prop="name" label="项目" min-width="180" />
          <el-table-column prop="status" label="状态" width="180" />
          <el-table-column prop="containers" label="容器数" width="90" class-name="col-p2" label-class-name="col-p2" />
          <el-table-column prop="config_files" label="配置文件" min-width="220" show-overflow-tooltip class-name="col-p3" label-class-name="col-p3" />
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
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { ArrowDown } from "@element-plus/icons-vue";
import http from "../api/http";
import { pkgMetaSafe, type PkgMeta } from "../api/meta";
import { submitJob } from "../api/jobs";

interface DockerStatus {
  installed: boolean;
  running: boolean;
  daemon: boolean;
  version: string;
  compose: string;
  error: string;
}
/** 实例：容器或 systemd 服务，两者共用同一张卡片 */
interface Instance {
  id: string;
  kind: "container" | "service";
  name: string;
  /** 容器为镜像名；服务为单元描述 */
  detail: string;
  state: string;
  ports: string;
  project: string;
  cpu: string;
  mem: string;
  pids: number[];
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

const router = useRouter();

const status = ref<DockerStatus | null>(null);
/** 发行版能力：安装提示里的命令名与包名随发行版不同 */
const pkgMeta = ref<PkgMeta>({ family: "", pretty: "", manager: "", rolling: false });
const installCmd = computed(() =>
  pkgMeta.value.manager === "pacman"
    ? "pacman -S docker docker-compose"
    : "apt-get install docker.io docker-cli docker-compose"
);
const cliPackageHint = computed(() =>
  pkgMeta.value.manager === "pacman"
    ? "Arch 的守护进程与命令行工具都在 docker 包里"
    : "Debian 把它列为 docker.io 的推荐包，容易被漏装"
);

const tab = ref("instances");
const dockerReady = computed(() => !!status.value?.running);

const instances = ref<Instance[]>([]);
const keyword = ref("");
const images = ref<Image[]>([]);
const projects = ref<Project[]>([]);
const pullName = ref("");

const installing = ref(false);
const starting = ref(false);
const pulling = ref(false);

const containerCount = computed(
  () => instances.value.filter((i) => i.kind === "container").length,
);
const serviceCount = computed(() => instances.value.length - containerCount.value);

const filtered = computed(() => {
  const k = keyword.value.trim().toLowerCase();
  if (!k) return instances.value;
  return instances.value.filter(
    (i) =>
      i.name.toLowerCase().includes(k) ||
      i.detail.toLowerCase().includes(k) ||
      i.ports.toLowerCase().includes(k),
  );
});

/** 实例 id 里抠出容器短 ID，供 docker 相关接口使用 */
function containerId(i: Instance) {
  return i.kind === "container" ? i.id.slice("container:".length) : "";
}

async function loadStatus() {
  const { data } = await http.get("/docker/status");
  status.value = data;
}

async function loadInstances() {
  const { data } = await http.get("/instances");
  instances.value = data;
}

async function loadImages() {
  const { data } = await http.get("/docker/images");
  images.value = data;
}

async function loadCompose() {
  const { data } = await http.get("/docker/compose");
  projects.value = data;
}

/**
 * 安装 Docker 改为后台作业（P2-1）。
 *
 * 这条链要装包、启守护进程、再自检，几分钟起步；原来的同步请求既看不到进度，
 * 也容易被中途的网关超时切断，用户只能对着一句"安装失败"猜。
 */
async function install() {
  installing.value = true;
  try {
    await submitJob("docker_install");
    ElMessage.success("已加入任务队列，可在「任务」页查看进度");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "提交安装失败");
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
    await loadInstances();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "启动失败");
  } finally {
    starting.value = false;
  }
}

/**
 * 两处入口都带上实例范围，目标页据此把自己限定在这个实例里 ——
 * 从卡片点进去看到的必须是这个实例的东西，而不是整机视图。
 * 容器才有独立的文件树；服务只提供日志与进程入口。
 */
function openFiles(i: Instance) {
  router.push({ path: "/files", query: { instance: i.id } });
}

function openLogs(i: Instance) {
  // 容器有独立日志流，服务走 journal 单元；两种都由日志页按实例 id 自行分流
  router.push({ path: "/logs", query: { instance: i.id } });
}

function openProcesses(i: Instance) {
  router.push({ path: "/processes", query: { instance: i.id } });
}

/** 卡片「更多」的入口：容器走 docker 接口，服务复用「服务」页的 systemctl 接口 */
function instAct(i: Instance, action: string) {
  return i.kind === "container" ? containerAct(i, action) : serviceAct(i, action);
}

/** 服务 unit 名从实例 id 里取 */
async function serviceAct(i: Instance, action: string) {
  const unit = i.id.slice("service:".length);
  try {
    const { data } = await http.post("/services", { name: unit, action });
    if (!data.ok) {
      ElMessage.error(data.stderr || `${action} ${unit} 失败`);
      return;
    }
    ElMessage.success("操作成功");
    await loadInstances();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  }
}

async function containerAct(i: Instance, action: string) {
  // 删除是不可逆操作，先确认
  if (action === "remove") {
    try {
      await ElMessageBox.confirm(`删除容器 ${i.name}？`, "确认", {
        type: "warning",
        confirmButtonText: "删除",
        cancelButtonText: "取消",
      });
    } catch {
      return;
    }
  }
  try {
    await http.post("/docker/container/action", { id: containerId(i), action });
    ElMessage.success("操作成功");
    await loadInstances();
    if (tab.value === "compose") await loadCompose();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
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
    await submitJob("docker_pull", { target: name });
    ElMessage.success("已加入任务队列，可在「任务」页查看进度");
    pullName.value = "";
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "提交拉取失败");
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
    await Promise.all([loadCompose(), loadInstances()]);
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  }
}

// 切页签时按需加载，避免首屏发一堆请求
watch(tab, (v) => {
  if (!dockerReady.value) return;
  if (v === "images") loadImages();
  if (v === "compose") loadCompose();
});

onMounted(async () => {
  pkgMeta.value = await pkgMetaSafe();
  await loadStatus();
  // 实例列表始终拉取：Docker 没起来时它至少还有 systemd 服务
  await loadInstances();
  if (dockerReady.value) await Promise.all([loadImages(), loadCompose()]);
});
</script>

<style scoped>
.panel {
  padding: 20px 24px;
  border-radius: var(--radius);
  background: var(--el-bg-color-page);
  margin-bottom: 16px;
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
  margin-left: 8px;
}
.empty {
  color: var(--el-text-color-secondary);
  padding: 32px 0;
  text-align: center;
}
.inst-tabs {
  margin-top: -8px;
}
.inst-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 12px;
}
.inst-card {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 14px 16px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: var(--radius);
  background: var(--el-bg-color);
}
.inst-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.inst-name {
  flex: 1;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.inst-detail {
  color: var(--el-text-color-secondary);
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.inst-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  font-size: 12px;
  color: var(--el-text-color-regular);
}
.meta-ports {
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.inst-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 2px;
  padding-top: 8px;
  border-top: 1px solid var(--el-border-color-lighter);
}
</style>
