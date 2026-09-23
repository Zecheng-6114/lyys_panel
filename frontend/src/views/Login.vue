<template>
  <div class="login-wrap">
    <div class="login-card">
      <div class="login-brand">LYYS<span> Panel</span></div>
      <div class="login-sub">服务器运维面板</div>
      <el-form :model="form" @submit.prevent="submit">
        <el-form-item>
          <el-input
            v-model="form.username"
            placeholder="用户名"
            size="large"
            autocomplete="username"
          />
        </el-form-item>
        <el-form-item>
          <el-input
            v-model="form.password"
            type="password"
            placeholder="密码"
            size="large"
            show-password
            autocomplete="current-password"
            @keyup.enter="submit"
          />
        </el-form-item>
        <el-button
          type="primary"
          size="large"
          class="login-btn"
          :loading="loading"
          @click="submit"
        >
          登 录
        </el-button>
      </el-form>
    </div>
  </div>
</template>

<script setup lang="ts">
import { reactive, ref } from "vue";
import { useRouter } from "vue-router";
import http from "../api/http";

const router = useRouter();
const loading = ref(false);
const form = reactive({ username: "admin", password: "" });

async function submit() {
  if (!form.password) {
    ElMessage.warning("请输入密码");
    return;
  }
  loading.value = true;
  try {
    const { data } = await http.post("/login", form);
    localStorage.setItem("panel_token", data.token);
    router.push("/dashboard");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "登录失败");
  } finally {
    loading.value = false;
  }
}
</script>

<style scoped>
.login-wrap {
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--el-bg-color-page);
}
.login-card {
  /* 窄屏上不硬顶 320px，否则小屏（如 320px 宽）会溢出屏幕 */
  width: 100%;
  max-width: 320px;
  box-sizing: border-box;
  background: var(--el-bg-color);
  border-radius: 6px;
  padding: 32px;
}
@media (max-width: 768px) {
  .login-card {
    padding: 24px 20px;
  }
}
.login-brand {
  font-size: 18px;
  font-weight: 600;
  letter-spacing: 0.5px;
}
.login-brand span {
  font-weight: 400;
  color: var(--el-text-color-secondary);
}
.login-sub {
  font-size: 13px;
  color: var(--el-text-color-secondary);
  margin: 4px 0 24px;
}
.login-btn {
  width: 100%;
}
</style>
