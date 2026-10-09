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
  /* 外壳已把整页锁成不滚（见 theme.css），登录页是唯一没有 .content 包裹的
     页面，所以由它自己兜住：极矮屏下卡片纵向居中会被裁，这里放行内部滚动。 */
  overflow: auto;
}
.login-card {
  /* 320 → 336，内距 32 → 20（纵向）、32 → 28（横向）。登录是唯一「一屏只有一张卡」的界面，
     内距一放大，那张卡就整块往外撑、中间反而更空 —— 收紧之后内容区
     宽 280，账号/密码输入框跟着变宽，一屏信息密度反而上来了。
     窄屏不硬顶这个数，否则 320px 宽的屏会溢出。 */
  width: 100%;
  max-width: 336px;
  box-sizing: border-box;
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: var(--sp-4) var(--sp-5);
  /* 与全站卡片同一档投影：登录卡浮在页面底色上，边界一眼就交代清楚 */
  box-shadow: var(--panel-card-shadow);
}
@media (max-width: 768px) {
  .login-card {
    /* 窄屏比基准再收一档：320px 宽的屏上少占一圈留白 */
    padding: var(--sp-3) var(--sp-4);
  }
}
.login-brand {
  font-size: var(--fs-lg);
  font-weight: 600;
  letter-spacing: 0.5px;
}
.login-brand span {
  font-weight: 400;
  color: var(--el-text-color-secondary);
}
.login-sub {
  font-size: var(--fs-base);
  color: var(--el-text-color-secondary);
  margin: var(--sp-1) 0 var(--sp-5);
}
.login-btn {
  width: 100%;
}
</style>
