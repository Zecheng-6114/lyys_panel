<template>
  <div class="packages">
    <div class="toolbar">
      <el-radio-group v-model="mode">
        <el-radio-button value="installed">已安装</el-radio-button>
        <!-- Arch 系是滚动更新发行版，没有「可升级」这个中间态 -->
        <el-radio-button value="upgradable">{{ rolling ? "滚动更新" : "可升级" }}</el-radio-button>
        <el-radio-button value="search">在线搜索</el-radio-button>
      </el-radio-group>
      <el-input
        v-if="mode !== 'upgradable'"
        v-model="keyword"
        :placeholder="mode === 'search' ? '包名或描述关键字' : '包名关键字'"
        clearable
        style="width: 220px"
        @keyup.enter="load"
      />
      <el-button :loading="loading" @click="load">查询</el-button>
      <label v-if="mode === 'search'" class="opt">
        <el-checkbox v-model="hideInstalled" size="small">只看未安装</el-checkbox>
      </label>
      <div class="spacer"></div>
      <!-- 刷新索引仅 Debian 系提供：Arch 下单独同步数据库会造成部分升级 -->
      <el-button v-if="!rolling" :loading="acting" @click="doUpdate">刷新索引</el-button>
      <el-button
        v-if="mode === 'search'"
        type="primary"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doInstall"
      >
        安装所选
      </el-button>
      <el-button
        v-if="mode === 'installed'"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doRemove"
      >
        卸载所选
      </el-button>
      <!-- Arch：一步滚动更新。列表为空也允许执行——它自带同步，
           索引没同步过时列表本就是空的，不能因此把唯一的出口锁死 -->
      <el-button
        v-if="mode === 'upgradable' && rolling"
        type="primary"
        :loading="acting"
        @click="doSysUpgrade"
      >
        滚动更新
      </el-button>
      <el-button
        v-if="mode === 'upgradable' && !rolling"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doUpgrade"
      >
        升级所选
      </el-button>
    </div>

    <div class="statusline">
      <span class="hint">{{ modeHint }}</span>
      <span class="spacer"></span>
      <span v-if="rows.length" class="count">
        共 {{ displayRows.length }} 个{{ truncated ? "（已截断，请用关键字收窄）" : "" }}
      </span>
    </div>

    <el-table
      ref="tableEl"
      v-loading="loading"
      :data="pagedRows"
      size="small"
      row-key="name"
      class="ptable"
      height="var(--pkg-table-height)"
      :empty-text="emptyText"
      @selection-change="(v: Pkg[]) => (selected = v)"
    >
      <el-table-column v-if="selectable" type="selection" width="36" reserve-selection />
      <el-table-column label="包名" prop="name" v-bind="col(200, true)" />
      <el-table-column label="版本" v-bind="col(250)" v-if="!hideColP2">
        <template #default="{ row }">
          <template v-if="row.version.includes(' -> ')">
            <span class="mono ver-old">{{ row.version.split(" -> ")[0] }}</span>
            <span class="arrow">→</span>
            <span class="mono ver-new">{{ row.version.split(" -> ")[1] }}</span>
          </template>
          <span v-else class="mono">{{ row.version || "—" }}</span>
        </template>
      </el-table-column>
      <el-table-column
        v-if="hasRepo && !hideColP3"
        label="仓库"
        prop="repo"
        v-bind="col(120)"
      />
      <el-table-column v-if="mode === 'search'" label="状态" v-bind="col(190)">
        <template #default="{ row }">
          <template v-if="row.installed">
            <span class="mono ver-old">{{ row.installed }}</span>
            <span class="tag-done">已安装</span>
          </template>
          <span v-else class="tag-todo">未安装</span>
        </template>
      </el-table-column>
      <el-table-column
        v-if="hasArch && !hideColP3"
        label="架构"
        prop="arch"
        v-bind="col(100)"
      />
      <el-table-column label="描述" prop="description" v-bind="col(300, true)" show-overflow-tooltip />
    </el-table>

    <!-- 分页：表格组件不做虚拟滚动，一次渲染两千行必然卡；分页让渲染量恒定 -->
    <div v-if="displayRows.length" class="pager">
      <el-pagination
        v-model:current-page="page"
        v-model:page-size="pageSize"
        :page-sizes="PAGE_SIZES"
        :total="displayRows.length"
        layout="sizes, prev, pager, next, jumper"
        size="small"
        background
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { computed, onMounted, ref, watch } from "vue";
import http from "../api/http";
import { pkgMetaSafe, type PkgMeta } from "../api/meta";
import { submitJob, type JobKind } from "../api/jobs";
import { useJobsStore } from "../stores/jobs";

interface Pkg {
  name: string;
  version: string;
  arch: string;
  repo: string;
  installed: string | null;
  description: string;
}

/** 搜索/列表的条数上限：与后端 clamp 范围（5000）留出余量 */
const SEARCH_LIMIT = 300;
const LIST_LIMIT = 2000;
/** 每页行数档位 */
const PAGE_SIZES = [50, 100, 200, 500];

const jobWatch = useJobsStore();

const mode = ref<"installed" | "upgradable" | "search">("installed");
const keyword = ref("");
const rows = ref<Pkg[]>([]);
const selected = ref<Pkg[]>([]);
const loading = ref(false);
const acting = ref(false);
const hideInstalled = ref(false);
const meta = ref<PkgMeta>({ family: "", pretty: "", manager: "", rolling: false });

const rolling = computed(() => meta.value.rolling);
/** 滚动更新页不提供逐包勾选：Arch 不支持部分升级 */
const selectable = computed(() => !(mode.value === "upgradable" && rolling.value));
const hasRepo = computed(() => rows.value.some((r) => !!r.repo));
const hasArch = computed(() => rows.value.some((r) => !!r.arch));
const displayRows = computed(() =>
  mode.value === "search" && hideInstalled.value
    ? rows.value.filter((r) => !r.installed)
    : rows.value
);
/** 当前页与每页行数。表格一次只渲染这么多行，滚动与交互不再随包数变卡 */
const page = ref(1);
const pageSize = ref(100);
/** el-table 实例：换页签时清掉跨页保留的勾选 */
const tableEl = ref<{ clearSelection: () => void } | null>(null);

const pagedRows = computed(() => {
  const start = (page.value - 1) * pageSize.value;
  return displayRows.value.slice(start, start + pageSize.value);
});

// 行数或每页条数变化时把页码收进有效范围（过滤、重查、改每页条数都会触发）
watch([() => displayRows.value.length, pageSize], () => {
  const pages = Math.max(1, Math.ceil(displayRows.value.length / pageSize.value));
  if (page.value > pages) page.value = pages;
});
/** 命中上限即视为被截断（后端按 limit 截断，不额外回传是否截断的标志） */
const truncated = computed(() =>
  mode.value === "search"
    ? rows.value.length >= SEARCH_LIMIT
    : mode.value === "installed" && rows.value.length >= LIST_LIMIT
);


const modeHint = computed(() => {
  if (mode.value === "installed") {
    return "本机已安装的全部软件包";
  }
  if (mode.value === "upgradable") {
    return rolling.value
      ? `滚动更新 = 同步数据库 + 全量升级${meta.value.pretty ? `（${meta.value.pretty}）` : ""}，Arch 系不支持只升部分包`
      : "只升级勾选的包，其余保持不变";
  }
  return rolling.value
    ? `在本地软件源索引中搜索${meta.value.manager ? `（${meta.value.manager}）` : ""}，索引随「滚动更新」一并刷新`
    : `在已同步的软件源中搜索${meta.value.manager ? `（${meta.value.manager}）` : ""}`;
});

const emptyText = computed(() => {
  if (loading.value) {
    return "";
  }
  if (mode.value === "upgradable") {
    return rolling.value
      ? "没有待更新的软件包（列表取自本地索引；若长期未同步过索引，列表为空属正常，直接点「滚动更新」即可）"
      : "没有可升级的软件包";
  }
  if (mode.value === "search") {
    return keyword.value.trim() ? "没有匹配的软件包" : "输入关键字后点查询";
  }
  return "没有查询到软件包";
});

async function load() {
  if (mode.value === "search" && !keyword.value.trim()) {
    rows.value = [];
    ElMessage.warning("请输入搜索关键字");
    return;
  }
  loading.value = true;
  page.value = 1;
  try {
    if (mode.value === "installed") {
      const { data } = await http.get("/packages", {
        params: { filter: keyword.value.trim() || undefined, limit: LIST_LIMIT },
      });
      rows.value = data;
    } else if (mode.value === "upgradable") {
      const { data } = await http.get("/packages/upgradable");
      rows.value = data;
    } else {
      const { data } = await http.get("/packages/search", {
        params: { filter: keyword.value.trim(), limit: SEARCH_LIMIT },
      });
      rows.value = data;
    }
  } catch (e: any) {
    rows.value = [];
    ElMessage.error(e.response?.data?.error ?? "查询软件包失败");
  } finally {
    loading.value = false;
  }
}

// 切换标签页一律重新拉取：以前只有「可升级」会自动加载，
// 切回「已安装」看到的是上一次清空后的空表（表现为 No Data）
watch(mode, () => {
  selected.value = [];
  tableEl.value?.clearSelection();
  page.value = 1;
  hideInstalled.value = false;
  rows.value = [];
  if (mode.value === "search" && !keyword.value.trim()) {
    return;
  }
  load();
});

/** 动作名 → 中文提示，失败提示里不出现英文动作标识 */
const ACTION_LABEL: Record<string, string> = {
  update: "刷新索引",
  install: "安装",
  upgrade: "升级",
  sysupgrade: "滚动更新",
  remove: "卸载",
};

/** 动作 → 作业类型（P2-1）：写操作一律提交后台作业，不再同步等待 */
const ACTION_KIND: Record<string, JobKind> = {
  update: "pkg_update",
  install: "pkg_install",
  upgrade: "pkg_upgrade",
  sysupgrade: "pkg_sysupgrade",
  remove: "pkg_remove",
};

/** 提交成功的统一提示：具体进度在「任务」页看，这里不再转圈等待 */
const QUEUED_HINT = "已加入任务队列，可在「任务」页查看进度";

async function doAction(action: string, names: string[]) {
  const kind = ACTION_KIND[action];
  if (!kind) {
    ElMessage.error(`未知操作：${action}`);
    return false;
  }
  acting.value = true;
  try {
    const id = await submitJob(kind, { action, names });
    // 登记：跑完会弹通知，不用守着「任务」页
    jobWatch.watch(id);
    return true;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? `${ACTION_LABEL[action] ?? action}提交失败`);
    return false;
  } finally {
    acting.value = false;
  }
}

async function doUpdate() {
  if (await doAction("update", [])) {
    ElMessage.success(QUEUED_HINT);
  }
}

/** Arch 全量滚动更新：同步数据库 + 升级所有包，一步到位 */
async function doSysUpgrade() {
  try {
    await ElMessageBox.confirm(
      `将同步软件源并升级全部已安装软件包${meta.value.pretty ? `（${meta.value.pretty}）` : ""}。` +
        "Arch 系不支持只升一部分包，这是唯一受支持的全量升级方式，耗时可能较长。",
      "滚动更新确认",
      { type: "warning", confirmButtonText: "开始更新", cancelButtonText: "取消" }
    );
  } catch {
    return;
  }
  if (await doAction("sysupgrade", [])) {
    ElMessage.success(QUEUED_HINT);
  }
}

async function doInstall() {
  const names = selected.value.map((p) => p.name);
  try {
    await ElMessageBox.confirm(`确定安装 ${names.length} 个软件包？`, "安装确认", {
      confirmButtonText: "安装",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  if (await doAction("install", names)) {
    ElMessage.success(QUEUED_HINT);
  }
}

async function doUpgrade() {
  const names = selected.value.map((p) => p.name);
  try {
    await ElMessageBox.confirm(`确定升级 ${names.length} 个软件包？`, "升级确认", {
      confirmButtonText: "升级",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  if (await doAction("upgrade", names)) {
    ElMessage.success(QUEUED_HINT);
  }
}

async function doRemove() {
  const names = selected.value.map((p) => p.name);
  try {
    await ElMessageBox.confirm(
      `确定卸载 ${names.length} 个软件包（${names.join(", ")}）？`,
      "卸载确认",
      { type: "warning", confirmButtonText: "卸载", cancelButtonText: "取消" }
    );
  } catch {
    return;
  }
  if (await doAction("remove", names)) {
    ElMessage.success(QUEUED_HINT);
  }
}

onMounted(async () => {
  // 先取发行版能力再渲染数据：Arch 的标签与按钮与 Debian 不同
  meta.value = await pkgMetaSafe();
  await load();
});
</script>

<style scoped>
.packages {
  /* 本页比普通列表页多出「状态行 + 分页条」两块，表格可用高度从全局值派生扣除
     （两行按标尺各折一档）。原来写的 calc(100vh - 196px) 与顶栏 / 内容内距脱钩，
     标尺一变这里就跟着错位。 */
  --pkg-table-height: calc(
    var(--panel-table-height) - var(--sp-6) * 2 - var(--sp-5)
  );
}

.statusline {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  /* 上边负偏移吃掉工具栏下边距的双份留白，属刻意为之，保留 */
  margin: -4px 0 var(--sp-2);
  font-size: 12px;
  color: var(--el-text-color-secondary);
}

.opt {
  margin-left: var(--sp-1);
}

/* 分页条：贴右对齐，与表格同宽 */
.pager {
  display: flex;
  justify-content: flex-end;
  margin-top: var(--sp-3);
}

.count {
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
}

.arrow {
  margin: 0 var(--sp-2);
  color: var(--el-text-color-secondary);
}

/* 旧版本淡出、新版本加重：升级前后的对比一眼可辨 */
.ver-old {
  color: var(--el-text-color-secondary);
}

.ver-new {
  color: var(--el-text-color-primary);
  font-weight: 600;
}

.tag-done,
.tag-todo {
  display: inline-block;
  margin-left: var(--sp-2);
  padding: 0 var(--sp-2);
  border-radius: var(--radius);
  font-size: 11px;
  line-height: 18px;
  vertical-align: middle;
}

.tag-done {
  color: var(--el-text-color-secondary);
  box-shadow: inset 0 0 0 1px var(--el-text-color-secondary);
}

.tag-todo {
  color: var(--el-text-color-placeholder);
}
</style>
