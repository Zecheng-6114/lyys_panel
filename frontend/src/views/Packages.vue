<template>
  <div class="packages">
    <div class="toolbar">
      <el-radio-group v-model="mode">
        <el-radio-button value="installed">已安装</el-radio-button>
        <el-radio-button value="upgradable">可升级</el-radio-button>
        <el-radio-button value="search">在线搜索</el-radio-button>
      </el-radio-group>
      <el-input
        v-if="mode !== 'upgradable'"
        v-model="keyword"
        placeholder="包名关键字"
        clearable
        style="width: 220px"
        @keyup.enter="load"
      />
      <el-button @click="load">查询</el-button>
      <div class="spacer"></div>
      <el-button :loading="acting" @click="doUpdate">刷新索引</el-button>
      <!-- 操作按钮按标签页显示：安装针对在线搜索结果，升级针对可升级包，卸载针对已安装包 -->
      <el-button
        v-if="mode === 'search'"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doInstall"
      >
        安装所选
      </el-button>
      <el-button
        v-if="mode === 'upgradable'"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doUpgrade"
      >
        升级所选
      </el-button>
      <el-button
        v-if="mode === 'installed'"
        :loading="acting"
        :disabled="selected.length === 0"
        @click="doRemove"
      >
        卸载所选
      </el-button>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      row-key="name"
      class="ptable"
      height="var(--panel-table-height)"
      @selection-change="(v: Pkg[]) => (selected = v)"
    >
      <el-table-column type="selection" width="36" />
      <el-table-column label="包名" prop="name" min-width="200" />
      <el-table-column label="版本" width="220">
        <template #default="{ row }"><span class="mono">{{ row.version }}</span></template>
      </el-table-column>
      <el-table-column label="架构" prop="arch" width="100" />
      <el-table-column label="描述" prop="description" min-width="300" show-overflow-tooltip />
    </el-table>

    <el-dialog v-model="showOutput" title="apt 输出" width="720px" top="6vh">
      <pre class="output">{{ output }}</pre>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import http from "../api/http";

interface Pkg {
  name: string;
  version: string;
  arch: string;
  description: string;
}

const mode = ref<"installed" | "upgradable" | "search">("installed");
const keyword = ref("");
const rows = ref<Pkg[]>([]);
const selected = ref<Pkg[]>([]);
const loading = ref(false);
const acting = ref(false);
const showOutput = ref(false);
const output = ref("");

async function load() {
  loading.value = true;
  try {
    if (mode.value === "installed") {
      const { data } = await http.get("/packages", {
        params: { filter: keyword.value || undefined },
      });
      rows.value = data;
    } else if (mode.value === "upgradable") {
      const { data } = await http.get("/packages/upgradable");
      rows.value = data;
    } else {
      if (!keyword.value.trim()) {
        ElMessage.warning("请输入搜索关键字");
        return;
      }
      const { data } = await http.get("/packages/search", {
        params: { filter: keyword.value.trim() },
      });
      rows.value = data;
    }
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "查询软件包失败");
  } finally {
    loading.value = false;
  }
}

watch(mode, () => {
  selected.value = [];
  if (mode.value === "upgradable") load();
  else rows.value = [];
});

async function doUpdate() {
  acting.value = true;
  try {
    const { data } = await http.post(
      "/packages/action",
      { action: "update", names: [] },
      { timeout: 120000 }
    );
    output.value = data.output;
    showOutput.value = true;
    ElMessage.success("索引已刷新");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "刷新索引失败");
  } finally {
    acting.value = false;
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
  acting.value = true;
  try {
    const { data } = await http.post(
      "/packages/action",
      { action: "install", names },
      { timeout: 600000 }
    );
    output.value = data.output;
    showOutput.value = true;
    ElMessage.success("安装完成");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "安装失败");
  } finally {
    acting.value = false;
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
  acting.value = true;
  try {
    const { data } = await http.post(
      "/packages/action",
      { action: "upgrade", names },
      { timeout: 600000 }
    );
    output.value = data.output;
    showOutput.value = true;
    ElMessage.success("升级完成");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "升级失败");
  } finally {
    acting.value = false;
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
  acting.value = true;
  try {
    const { data } = await http.post(
      "/packages/action",
      { action: "remove", names },
      { timeout: 600000 }
    );
    output.value = data.output;
    showOutput.value = true;
    ElMessage.success("卸载完成");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "卸载失败");
  } finally {
    acting.value = false;
  }
}

onMounted(load);
</script>

<style scoped>
.output {
  background: var(--el-bg-color);
  border-radius: 6px;
  padding: 12px 16px;
  max-height: 60vh;
  overflow: auto;
  font-family: var(--panel-mono);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
  margin: 0;
}
</style>
