<template>
  <div class="sessions">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <span class="spacer" />
      <span class="hint">{{ auth.isAdmin() ? "管理员可查看并踢出全部会话" : "仅显示你自己的在线会话" }}</span>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      height="var(--panel-table-height)"
    >
      <el-table-column label="用户" prop="username" v-bind="col(120)" />
      <el-table-column label="会话" prop="jti_prefix" v-bind="col(110)" class-name="mono" v-if="!hideColP3" />
      <el-table-column label="来源 IP" prop="ip" v-bind="col(140)" class-name="mono" />
      <el-table-column label="客户端" v-bind="col(220, true)" show-overflow-tooltip>
        <template #default="{ row }">{{ row.ua || "-" }}</template>
      </el-table-column>
      <el-table-column label="签发时间" v-bind="col(170)" v-if="!hideColP3">
        <template #default="{ row }">{{ fmt(row.iat) }}</template>
      </el-table-column>
      <el-table-column label="过期时间" v-bind="col(170)" v-if="!hideColP2">
        <template #default="{ row }">{{ fmt(row.exp) }}</template>
      </el-table-column>
      <el-table-column label="操作" v-bind="col(140)" align="right">
        <template #default="{ row }">
          <el-tag v-if="row.current" size="small" type="info">当前</el-tag>
          <el-button
            v-else-if="auth.isAdmin()"
            link
            size="small"
            @click="kick(row)"
          >
            踢出
          </el-button>
        </template>
      </el-table-column>
    </el-table>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { onMounted, ref } from "vue";
import http from "../api/http";
import { useAuthStore } from "../stores/auth";

interface SessionRow {
  jti_prefix: string;
  user_id: number;
  username: string;
  ua: string;
  ip: string;
  iat: number;
  exp: number;
  current: boolean;
}

const auth = useAuthStore();
const rows = ref<SessionRow[]>([]);
const loading = ref(false);

function fmt(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/sessions");
    rows.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取会话列表失败");
  } finally {
    loading.value = false;
  }
}

async function kick(row: SessionRow) {
  try {
    await ElMessageBox.confirm(
      `踢出用户「${row.username}」的全部在线会话？其当前登录将立即失效。`,
      "确认",
      { confirmButtonText: "踢出", cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  try {
    await http.post("/sessions/kick", { user_id: row.user_id });
    ElMessage.success("已踢出");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "踢出失败");
  }
}

onMounted(load);
</script>

<style scoped>
.hint {
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
}
</style>
