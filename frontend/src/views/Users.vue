<template>
  <div class="users">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button @click="openCreate">新增用户</el-button>
      <span class="spacer" />
      <span class="hint">viewer 只读 · operator 可执行操作 · admin 全权</span>
    </div>

    <el-table
      v-loading="loading"
      :data="rows"
      size="small"
      height="var(--panel-table-height)"
    >
      <el-table-column label="ID" prop="id" v-bind="col(60)" class-name="mono" v-if="!hideColP3" />
      <el-table-column label="用户名" prop="username" v-bind="col(140, true)" />
      <el-table-column label="角色" v-bind="col(120)">
        <template #default="{ row }">
          <span class="dot" :class="row.role === 'admin' ? 'dot-on' : 'dot-off'" />
          {{ roleLabel(row.role) }}
        </template>
      </el-table-column>
      <el-table-column label="待改密" v-bind="col(90)" v-if="!hideColP2">
        <template #default="{ row }">{{ row.must_change ? "是" : "否" }}</template>
      </el-table-column>
      <el-table-column label="操作" v-bind="col(220)" align="right">
        <template #default="{ row }">
          <el-button link size="small" @click="openEdit(row)">编辑</el-button>
          <el-button link size="small" @click="kick(row)">踢下线</el-button>
          <el-button link size="small" @click="remove(row)">删除</el-button>
        </template>
      </el-table-column>
    </el-table>

    <el-dialog v-model="showEdit" :title="editId === null ? '新增用户' : '编辑用户'" width="440px">
      <el-form label-width="72px" size="small">
        <el-form-item label="用户名">
          <el-input v-model="form.username" placeholder="字母、数字、下划线、连字符" />
        </el-form-item>
        <el-form-item label="角色">
          <el-select v-model="form.role" style="width: 100%">
            <el-option label="viewer（只读）" value="viewer" />
            <el-option label="operator（可操作）" value="operator" />
            <el-option label="admin（全权）" value="admin" />
          </el-select>
        </el-form-item>
        <el-form-item :label="editId === null ? '密码' : '重设密码'">
          <el-input
            v-model="form.password"
            type="password"
            show-password
            :placeholder="editId === null ? '至少 8 位' : '留空则不修改'"
          />
        </el-form-item>
        <el-form-item v-if="editId !== null" label=" ">
          <span class="hint">修改角色或重设密码会立即踢掉该用户全部会话</span>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button @click="save">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";

interface UserRow {
  id: number;
  username: string;
  role: string;
  must_change: boolean;
}

const rows = ref<UserRow[]>([]);
const loading = ref(false);
const showEdit = ref(false);
const editId = ref<number | null>(null);
const form = reactive({ username: "", role: "viewer", password: "" });

function roleLabel(r: string) {
  return r === "admin" ? "管理员" : r === "operator" ? "操作员" : "访客";
}

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/users");
    rows.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取用户列表失败");
  } finally {
    loading.value = false;
  }
}

function openCreate() {
  editId.value = null;
  form.username = "";
  form.role = "viewer";
  form.password = "";
  showEdit.value = true;
}

function openEdit(row: UserRow) {
  editId.value = row.id;
  form.username = row.username;
  form.role = row.role;
  form.password = "";
  showEdit.value = true;
}

async function save() {
  try {
    if (editId.value === null) {
      await http.post("/users", {
        username: form.username,
        password: form.password,
        role: form.role,
      });
      ElMessage.success("用户已创建（首次登录需改密）");
    } else {
      await http.put(`/users/${editId.value}`, {
        username: form.username,
        role: form.role,
        new_password: form.password || null,
      });
      ElMessage.success("用户已更新");
    }
    showEdit.value = false;
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function kick(row: UserRow) {
  try {
    await ElMessageBox.confirm(`踢出用户「${row.username}」的全部在线会话？`, "确认", {
      confirmButtonText: "踢出",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.post("/sessions/kick", { user_id: row.id });
    ElMessage.success("已踢出");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "踢出失败");
  }
}

async function remove(row: UserRow) {
  try {
    await ElMessageBox.confirm(`删除用户「${row.username}」？该操作不可恢复。`, "确认", {
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete(`/users/${row.id}`);
    ElMessage.success("用户已删除");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

onMounted(load);
</script>

<style scoped>
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  margin-right: 6px;
  vertical-align: middle;
}
.dot-on {
  background: var(--el-text-color-primary);
}
.dot-off {
  background: transparent;
  box-shadow: inset 0 0 0 1.5px var(--el-text-color-secondary);
}
</style>
