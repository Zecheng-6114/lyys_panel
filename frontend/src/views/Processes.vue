<template>
  <div class="proc">
    <div class="toolbar">
      <!-- 有实例限定时范围开关失效：从实例卡片点进来的意图就是"看这个实例的进程"，
           再叠一层系统/应用筛选只会让人困惑 -->
      <el-radio-group v-model="scope" :disabled="!!instance" @change="load">
        <el-radio-button value="system">系统进程</el-radio-button>
        <el-radio-button value="app">应用进程</el-radio-button>
        <el-radio-button value="all">全部</el-radio-button>
      </el-radio-group>
      <el-input
        v-model="keyword"
        placeholder="搜索进程名 / PID / 路径"
        clearable
        style="width: 220px"
      />
      <el-button @click="load">刷新</el-button>
      <el-switch v-model="auto" active-text="自动刷新（5s）" />
      <el-tag v-if="instance" closable type="info" size="small" @close="clearInstance">
        {{ instanceLabel }}
      </el-tag>
    </div>

    <el-table :data="filtered" height="var(--panel-table-height)" size="small">
      <el-table-column label="PID" width="90">
        <template #default="{ row }"><span class="mono">{{ row.pid }}</span></template>
      </el-table-column>
      <el-table-column prop="name" label="名称" min-width="140" />
      <el-table-column
        label="可执行路径"
        min-width="240"
        show-overflow-tooltip
        class-name="col-p2"
        label-class-name="col-p2"
      >
        <template #default="{ row }">
          <span v-if="row.exe" class="mono">{{ row.exe }}</span>
          <span v-else class="dim">（内核线程）</span>
        </template>
      </el-table-column>
      <el-table-column label="CPU" width="90">
        <template #default="{ row }"><span class="mono">{{ row.cpu.toFixed(1) }}%</span></template>
      </el-table-column>
      <el-table-column label="内存" width="100">
        <template #default="{ row }"><span class="mono">{{ fmtBytes(row.mem) }}</span></template>
      </el-table-column>
      <el-table-column prop="user" label="用户" width="110" class-name="col-p2" label-class-name="col-p2" />
      <el-table-column prop="status" label="状态" width="90" class-name="col-p3" label-class-name="col-p3" />
      <el-table-column label="操作" width="100">
        <template #default="{ row }">
          <el-button link size="small" @click="kill(row.pid)">结束</el-button>
        </template>
      </el-table-column>
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import http from "../api/http";

interface Proc {
  pid: number;
  name: string;
  cpu: number;
  mem: number;
  user: string;
  status: string;
  /** 可执行文件绝对路径；内核线程为空 */
  exe: string;
}

const route = useRoute();
const router = useRouter();

const rows = ref<Proc[]>([]);
const keyword = ref("");
const auto = ref(false);
/**
 * 默认只看系统进程。进程页的用途是服务器维护，被托管的负载在实例页单独呈现；
 * 需要全量时切一下即可，不必换页面。
 */
const scope = ref("system");
/** 从实例卡片跳进来时带的实例 id；给了就以它为准 */
const instance = ref(String(route.query.instance ?? ""));
let timer: number | undefined;

const instanceLabel = computed(() => {
  const id = instance.value;
  if (id.startsWith("container:")) {
    return `已限定：容器 ${id.slice("container:".length, "container:".length + 12)}`;
  }
  if (id.startsWith("app:")) return `已限定：${id.slice("app:".length)}`;
  return `已限定：${id}`;
});

const filtered = computed(() => {
  const k = keyword.value.trim().toLowerCase();
  if (!k) return rows.value;
  return rows.value.filter(
    (p) =>
      p.name.toLowerCase().includes(k) ||
      String(p.pid).includes(k) ||
      p.exe.toLowerCase().includes(k),
  );
});

function fmtBytes(n: number) {
  const units = ["B", "K", "M", "G"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)}${units[i]}`;
}

async function load() {
  const params: Record<string, string> = {};
  if (instance.value) {
    params.instance = instance.value;
  } else {
    params.scope = scope.value;
  }
  const { data } = await http.get("/processes", { params });
  rows.value = data;
}

/** 清掉实例限定，回到整机的系统进程视图 */
function clearInstance() {
  instance.value = "";
  router.replace({ path: "/processes" });
  load();
}

async function kill(pid: number) {
  try {
    await ElMessageBox.confirm(`确定结束进程 ${pid}？`, "确认", {
      confirmButtonText: "结束",
      cancelButtonText: "取消",
      type: "warning",
    });
  } catch {
    return;
  }
  try {
    await http.post("/processes", { pid });
    ElMessage.success("已结束");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "结束失败");
  }
}

// 从另一个实例跳过来时重新限定（组件复用时 query 变了但不会重新挂载）
watch(
  () => route.query.instance,
  (v) => {
    instance.value = String(v ?? "");
    load();
  },
);

// 自动刷新开关：开启后每 5 秒拉取一次进程列表
watch(auto, (on) => {
  if (on) {
    timer = window.setInterval(load, 5000);
  } else if (timer) {
    clearInterval(timer);
    timer = undefined;
  }
});

onMounted(load);
onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
});
</script>

<style scoped>
.dim {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
</style>
