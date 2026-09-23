<template>
  <div class="svc">
    <div class="toolbar">
      <el-input
        v-model="keyword"
        placeholder="搜索服务名"
        clearable
        style="width: 240px"
      />
      <el-button @click="load">刷新</el-button>
    </div>
    <el-table :data="filtered" height="var(--panel-table-height)" size="small">
      <el-table-column prop="name" label="服务" min-width="200" />
      <el-table-column label="状态" width="120">
        <template #default="{ row }">
          <span :class="['dot', isActive(row) ? 'dot-on' : 'dot-off']" />
          {{ row.active || "未知" }}
        </template>
      </el-table-column>
      <el-table-column prop="sub" label="子状态" width="120" />
      <el-table-column prop="description" label="描述" min-width="220" show-overflow-tooltip />
      <el-table-column label="操作" width="220">
        <template #default="{ row }">
          <el-button link size="small" @click="act(row.name, 'start')">启动</el-button>
          <el-button link size="small" @click="act(row.name, 'stop')">停止</el-button>
          <el-button link size="small" @click="act(row.name, 'restart')">重启</el-button>
          <el-button link size="small" @click="act(row.name, 'reload')">重载</el-button>
        </template>
      </el-table-column>
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import http from "../api/http";

interface Svc {
  name: string;
  load: string;
  active: string;
  sub: string;
  description: string;
}

const rows = ref<Svc[]>([]);
const keyword = ref("");

const filtered = computed(() => {
  const k = keyword.value.trim().toLowerCase();
  if (!k) return rows.value;
  return rows.value.filter((s) => s.name.toLowerCase().includes(k));
});

function isActive(s: Svc) {
  return s.active === "active";
}

async function load() {
  const { data } = await http.get("/services");
  rows.value = data;
}

async function act(name: string, action: string) {
  try {
    const { data } = await http.post("/services", { name, action });
    if (data.ok) {
      ElMessage.success(`${action} ${name} 成功`);
    } else {
      ElMessage.error(data.stderr || `${action} ${name} 失败`);
    }
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  }
}

onMounted(load);
</script>
