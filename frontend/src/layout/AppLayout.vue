<template>
  <div class="layout" :class="{ 'menu-open': menuOpen }">
    <aside class="sidebar">
      <div class="brand">LYYS<span> Panel</span></div>
      <el-menu
        :default-active="route.path"
        router
        class="side-menu"
        :ellipsis="false"
      >
        <el-menu-item index="/dashboard">仪表盘</el-menu-item>
        <el-menu-item index="/processes">进程</el-menu-item>
        <el-menu-item index="/services">服务</el-menu-item>
        <el-menu-item index="/logs">日志</el-menu-item>
        <el-menu-item index="/files">文件</el-menu-item>
        <el-menu-item index="/packages">软件</el-menu-item>
        <el-menu-item index="/cron">计划任务</el-menu-item>
        <el-menu-item index="/network">网络</el-menu-item>
        <el-menu-item index="/docker">Docker</el-menu-item>
        <el-menu-item index="/ops">深度运维</el-menu-item>
        <el-menu-item index="/ai">AI 助手</el-menu-item>
        <el-menu-item index="/sessions">在线会话</el-menu-item>
        <el-menu-item v-if="auth.isAdmin()" index="/users">账号管理</el-menu-item>
        <el-menu-item v-if="auth.isAdmin()" index="/backups">备份管理</el-menu-item>
        <el-menu-item v-if="auth.isAdmin()" index="/alerts">告警通知</el-menu-item>
        <el-menu-item v-if="auth.isAdmin()" index="/settings">系统设置</el-menu-item>
        <el-menu-item v-if="auth.isAdmin()" index="/update">面板更新</el-menu-item>
      </el-menu>
      <div class="version">v{{ appVersion }}</div>
    </aside>
    <!-- 窄屏下侧边栏是覆盖式抽屉，这层遮罩用来点空白处关闭 -->
    <div class="scrim" @click="menuOpen = false" />
    <div class="main">
      <header class="topbar">
        <button
          class="menu-btn"
          type="button"
          aria-label="打开菜单"
          @click="menuOpen = !menuOpen"
        >
          <span class="bars" />
        </button>
        <div class="page-title">{{ pageTitle }}</div>
        <div class="top-actions">
          <!-- 服务器电源操作：危险动作，仅 admin，二次确认 -->
          <el-dropdown v-if="auth.isAdmin()" trigger="click" @command="onPower">
            <button class="mini-btn icon-btn" type="button" aria-label="服务器电源" title="服务器电源">
              <!-- 电源图标 -->
              <svg
                viewBox="0 0 24 24"
                width="17"
                height="17"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
              >
                <path d="M18.36 6.64a9 9 0 1 1-12.73 0" />
                <line x1="12" y1="2" x2="12" y2="12" />
              </svg>
            </button>
            <template #dropdown>
              <el-dropdown-menu>
                <el-dropdown-item command="reboot">重启服务器</el-dropdown-item>
                <el-dropdown-item command="shutdown">关机</el-dropdown-item>
              </el-dropdown-menu>
            </template>
          </el-dropdown>
          <button class="mini-btn" type="button" @click="openPwdDialog">改密</button>
          <button
            v-if="auth.isAdmin()"
            class="mini-btn icon-btn"
            type="button"
            aria-label="界面设置"
            title="界面设置"
            @click="settingsOpen = true"
          >
            <!-- 齿轮图标 -->
            <svg
              viewBox="0 0 24 24"
              width="17"
              height="17"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
            >
              <circle cx="12" cy="12" r="3" />
              <path
                d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"
              />
            </svg>
          </button>
          <button class="mini-btn" type="button" @click="logout">退出</button>
        </div>
      </header>
      <section class="content">
        <router-view />
      </section>
    </div>
    <SettingsDialog v-model="settingsOpen" />

    <!-- 2.2 修改密码：首登强制改密（不可关闭）或用户主动改密 -->
    <el-dialog
      v-model="pwdOpen"
      :title="auth.mustChange ? '修改初始密码' : '修改密码'"
      width="400px"
      :close-on-click-modal="!auth.mustChange"
      :close-on-press-escape="!auth.mustChange"
      :show-close="!auth.mustChange"
    >
      <el-form label-width="72px" size="small">
        <el-form-item label="旧密码">
          <el-input v-model="pwdForm.old" type="password" show-password autocomplete="current-password" />
        </el-form-item>
        <el-form-item label="新密码">
          <el-input v-model="pwdForm.new1" type="password" show-password autocomplete="new-password" placeholder="至少 8 位" />
        </el-form-item>
        <el-form-item label="确认密码">
          <el-input v-model="pwdForm.new2" type="password" show-password autocomplete="new-password" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button v-if="auth.mustChange" @click="logout">退出</el-button>
        <el-button v-else @click="pwdOpen = false">取消</el-button>
        <el-button :loading="pwdLoading" @click="submitPwd">确认修改</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import http from "../api/http";
import { useThemeStore } from "../stores/theme";
import { useAuthStore } from "../stores/auth";
import SettingsDialog from "../components/SettingsDialog.vue";

const route = useRoute();
const router = useRouter();
const theme = useThemeStore();
const auth = useAuthStore();
const appVersion = __APP_VERSION__;

// 界面设置弹窗（主题定制）
const settingsOpen = ref(false);
// 进入布局（已登录）时拉取服务端主题定制配置与当前账号信息（角色/强制改密）
onMounted(async () => {
  theme.load();
  try {
    await auth.load();
  } catch {
    /* 401 由拦截器跳转登录 */
    return;
  }
  if (auth.mustChange) pwdOpen.value = true;
});

// 首登强制改密（2.2）；顶栏「改密」按钮也复用此弹窗
const pwdOpen = ref(false);
const pwdLoading = ref(false);
const pwdForm = reactive({ old: "", new1: "", new2: "" });

function openPwdDialog() {
  pwdForm.old = pwdForm.new1 = pwdForm.new2 = "";
  pwdOpen.value = true;
}

async function submitPwd() {
  if (pwdForm.new1.length < 8) {
    ElMessage.warning("新密码至少 8 位");
    return;
  }
  if (pwdForm.new1 !== pwdForm.new2) {
    ElMessage.warning("两次输入的新密码不一致");
    return;
  }
  pwdLoading.value = true;
  try {
    await http.post("/account/password", {
      old_password: pwdForm.old,
      new_password: pwdForm.new1,
    });
    ElMessage.success("密码已修改");
    pwdOpen.value = false;
    pwdForm.old = pwdForm.new1 = pwdForm.new2 = "";
    await auth.load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "修改失败");
  } finally {
    pwdLoading.value = false;
  }
}

/// 窄屏下侧边栏是抽屉，默认收起。宽屏时 CSS 忽略这个状态。
const menuOpen = ref(false);
// 选中菜单后自动收起，否则抽屉会挡住刚打开的页面
watch(() => route.path, () => {
  menuOpen.value = false;
});

const titles: Record<string, string> = {
  "/dashboard": "仪表盘",
  "/processes": "进程",
  "/services": "服务",
  "/logs": "日志",
  "/files": "文件",
  "/packages": "软件",
  "/cron": "计划任务",
  "/network": "网络",
  "/docker": "Docker",
  "/ops": "深度运维",
  "/ai": "AI 助手",
  "/sessions": "在线会话",
  "/users": "账号管理",
  "/backups": "备份管理",
  "/alerts": "告警通知",
  "/update": "面板更新",
  "/settings": "系统设置",
};
const pageTitle = computed(() => titles[route.path] ?? "LYYS Panel");

// 登出（P1-1）：先请求服务端吊销当前 token（旧 token 立即失效），
// 再清本地凭证。吊销请求失败（如后端已不可达）不阻断本地登出，
// token 本身有 24h 过期兜底。
/// 服务器电源操作（admin）：二次确认后执行；机器断电/重启后无法收到
/// 响应，直接给出提示即可。
async function onPower(cmd: "reboot" | "shutdown") {
  const label = cmd === "reboot" ? "重启服务器" : "关机";
  try {
    await ElMessageBox.confirm(
      `该操作影响整台服务器上的所有服务，且执行后面板将不可用（${
        cmd === "reboot" ? "需等待系统重启完成" : "需手动开机"
      }）。确定${label}？`,
      `${label}确认`,
      { type: "warning", confirmButtonText: label, cancelButtonText: "取消" },
    );
  } catch {
    return;
  }
  try {
    await http.post("/power", { action: cmd });
  } catch (e: any) {
    // 指令发出后机器立即断电/重启，响应大概率回不来（无 response 的网络错误）。
    // 这不代表失败：只有拿到明确错误响应（4xx/5xx）才算指令被拒绝。
    if (!e?.response) {
      ElMessage.warning(
        cmd === "reboot" ? "服务器重启中，约 1-2 分钟后重新访问面板" : "服务器关机中，需手动开机",
      );
      return;
    }
    ElMessage.error(e?.response?.data?.error ?? `${label}指令失败`);
    return;
  }
  ElMessage.warning(
    cmd === "reboot" ? "服务器重启中，约 1-2 分钟后重新访问面板" : "服务器已关机，需手动开机",
  );
}

async function logout() {
  const token = localStorage.getItem("panel_token");
  if (token) {
    try {
      await http.post("/logout", null, {
        headers: { Authorization: `Bearer ${token}` },
        timeout: 3000,
      });
    } catch {
      // 忽略：本地照常登出
    }
  }
  localStorage.removeItem("panel_token");
  auth.clear();
  router.push("/login");
}
</script>

<style scoped>
.layout {
  display: flex;
  height: 100%;
  /* 顶栏高度：既是 .topbar 的实际高度，也是 .content 圆角的纵向基准。 */
  --topbar-h: 52px;
  /* 侧边栏宽度：.sidebar 的实际宽度，也是 .content 圆角的横向基准。
     不要在别处硬编码这个数字 —— .sidebar 的 width 从它派生。 */
  --sidebar-w: 180px;
  /* 内凹圆角露出的底色 = 侧边栏/顶栏的面板色，用 .layout 自己的背景承载。 */
  background: var(--el-bg-color);
}
.sidebar {
  width: var(--sidebar-w);
  flex: none;
  display: flex;
  flex-direction: column;
  background: var(--el-bg-color);
  /* 右边留 20px：与 .content 的 padding 对齐，使菜单项与内容区左边线成一条竖线。 */
  padding: 16px 20px 16px 12px;
}
/* 交汇处的内凹圆角 = .content 的 border-top-left-radius，见下方 .content 规则。
   曾经用「额外 span + 径向渐变」实现，绕了十几轮且反复出错；
   .content 自带圆角一步到位，模板里这个 span 已删除。 */
.brand {
  font-size: 15px;
  font-weight: 600;
  letter-spacing: 0.5px;
  padding: 0 8px 16px;
}
.brand span {
  font-weight: 400;
  color: var(--el-text-color-secondary);
}
.side-menu {
  border-right: none;
  background: transparent;
  /* 占满剩余高度，把版本号顶到侧边栏底部 */
  flex: 1;
  min-height: 0;
  /* 紧凑菜单项：按钮间留足间距，文字贴紧按钮 */
  --el-menu-item-height: 32px;
  --el-menu-base-level-padding: 12px;
}
.side-menu :deep(.el-menu-item) {
  height: 32px;
  line-height: 32px;
  margin: 6px 0;
  padding: 0 12px;
  border-radius: var(--radius);
  font-size: 13px;
}
.side-menu :deep(.el-menu-item:hover) {
  background: var(--el-fill-color-light);
}
.side-menu :deep(.el-menu-item.is-active) {
  /* 反色块用主色而非文本色：定制只改文本色时不该把选中块一起染色 */
  background: var(--el-color-primary);
  color: var(--el-bg-color);
}
/* 版本号贴在侧边栏左下角：左内边距与菜单项文字对齐（菜单项本身 padding 0 12px） */
.version {
  flex: none;
  padding: 8px 0 0 12px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  user-select: none;
}
/* 纯图标按钮：正方形，图标居中。文字版 mini-btn 的左右内边距在这里不适用 */
.icon-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 32px;
  height: 32px;
  padding: 0;
  line-height: 0;
}
/* 宽屏不需要汉堡按钮与遮罩 —— 它们只在窄屏的抽屉模式下出现 */
.menu-btn {
  display: none;
}
.scrim {
  display: none;
}
.menu-btn {
  align-items: center;
  justify-content: center;
  margin-right: 10px;
  padding: 0;
  width: 32px;
  height: 32px;
  font-size: 16px;
  line-height: 1;
  color: var(--el-text-color-primary);
  background: transparent;
  border: none;
  border-radius: var(--radius);
  cursor: pointer;
}
.menu-btn:hover {
  background: var(--el-fill-color-light);
}
/* 汉堡图标用三条 CSS 线，而不是 ☰ 字符：U+2630 的基线位置随字体变化，
   字符本身没法做到稳定的垂直居中。画线则上下两条对称分布在主线两侧，
   整体几何中心就是按钮中心。 */
.menu-btn .bars {
  position: relative;
  display: block;
  width: 16px;
  height: 2px;
  background: currentColor;
  border-radius: 1px;
}
.menu-btn .bars::before,
.menu-btn .bars::after {
  content: "";
  position: absolute;
  left: 0;
  width: 16px;
  height: 2px;
  background: currentColor;
  border-radius: 1px;
}
.menu-btn .bars::before {
  top: -6px;
}
.menu-btn .bars::after {
  top: 6px;
}

/* ===== 窄屏：侧边栏改为覆盖式抽屉 ===== */
@media (max-width: 768px) {
  .sidebar {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    z-index: 20;
    transform: translateX(-100%);
    transition: transform 0.22s ease;
    box-sizing: border-box;
  }
  .layout.menu-open .sidebar {
    transform: translateX(0);
  }
  .menu-btn {
    display: inline-flex;
    /* 触摸目标不低于 44px */
    width: 44px;
    height: 44px;
  }
  .scrim {
    display: block;
    position: fixed;
    inset: 0;
    z-index: 15;
    background: rgba(0, 0, 0, 0.45);
  }
  /* 抽屉收起时遮罩不该留着挡点击 */
  .layout:not(.menu-open) .scrim {
    display: none;
  }
  .topbar {
    /* 12px → 8px：窄屏屏宽有限，顶栏不该再让出这么多边距 */
    padding: 0 8px;
  }
  /* 图标保持居中（靠左对齐会让它偏离按钮中心）。要贴边就整体左移按钮 ——
     44px 的触摸区域里，图标两侧各有 14px 留白，用负 margin 把这份留白
     让出去，图标就落到接近屏幕边缘的位置，而按钮本身仍然居中、点击区域不减。 */
  .menu-btn {
    margin-left: -12px;
  }
  .top-actions {
    /* 同理，抵消按钮自身的右内边距，让文字更靠右 */
    margin-right: -6px;
  }
  .top-actions .mini-btn {
    min-height: 44px;
    padding: 0 8px;
  }
  .top-actions .icon-btn {
    /* 纯图标按钮在窄屏做成 44px 见方，触摸区域与右侧文字按钮等高 */
    width: 44px;
    padding: 0;
  }
  .content {
    padding: 12px;
    /* 内凹圆角是为「侧边栏右边界 × 顶栏下沿」设计的。窄屏侧边栏不再常驻，
       交汇点不存在，留着会在左上角留下一个无来由的缺口。 */
    border-top-left-radius: 0;
  }
}
.main {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.topbar {
  height: var(--topbar-h);
  flex: none;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 20px;
  background: var(--el-bg-color);
}
.page-title {
  font-size: 16px;
  font-weight: 500;
}
.top-actions {
  display: flex;
  gap: 8px;
}
.content {
  flex: 1;
  overflow: auto;
  padding: 20px;
  /* 侧边栏右边界 × 顶栏下沿交汇处的内凹圆角 —— 就这一行。
     .content 的左上角正好压在交汇点上，把它磨圆，露出的就是下层面板色
     （.layout 背景 / .sidebar），交汇处自然沿圆弧内凹。
     不需要额外控件、渐变、伪元素或 SVG。 */
  border-top-left-radius: var(--radius);
  background: var(--el-bg-color-page);
}
</style>
