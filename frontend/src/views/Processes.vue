<template>
  <div class="proc">
    <div class="toolbar">
      <el-input
        v-model="keyword"
        placeholder="搜索进程名 / PID"
        clearable
        style="width: 240px"
      />
      <el-button @click="load">刷新</el-button>
      <el-switch v-model="auto" active-text="自动刷新（5s）" />
    </div>
    <el-table :data="filtered" height="var(--panel-table-height)" size="small">
      <el-table-column label="PID" width="90">
        <template #default="{ row }"><span class="mono">{{ row.pid }}</span></template>
      </el-table-column>
      <el-table-column prop="name" label="名称" min-width="160" />
      <el-table-column label="CPU" width="100">
        <template #default="{ row }"><span class="mono">{{ row.cpu.toFixed(1) }}%</span></template>
      </el-table-column>
      <el-table-column label="内存" width="110">
        <template #default="{ row }"><span class="mono">{{ fmtBytes(row.mem) }}</span></template>
      </el-table-column>
      <el-table-column prop="user" label="用户" width="120" />
      <el-table-column prop="status" label="状态" width="100" />
      <el-table-column label="操作" width="90">
        <template #default="{ row }">
          <el-button link size="small" @click="kill(row.pid)">
            结束
          </el-button>
        </template>
      </el-table-column>
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import http from "../api/http";

interface Proc {
  pid: number;
  name: string;
  cpu: number;
  mem: number;
  user: string;
  status: string;
}

const rows = ref<Proc[]>([]);
const keyword = ref("");
const auto = ref(false);
let timer: number | undefined;

const filtered = computed(() => {
  const k = keyword.value.trim().toLowerCase();
  if (!k) return rows.value;
  return rows.value.filter(
    (p) => p.name.toLowerCase().includes(k) || String(p.pid).includes(k),
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
  const { data } = await http.get("/processes");
  rows.value = data;
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
