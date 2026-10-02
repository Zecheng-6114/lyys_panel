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
        <!-- 菜单由 navGroups 单一数据源渲染；顶栏标题也从它派生，
             菜单与标题不再可能各写一份而彼此脱节 -->
        <template v-for="g in navGroups" :key="g.label">
          <el-menu-item-group v-if="!g.admin || auth.isAdmin()" :title="g.label">
            <el-menu-item
              v-for="item in g.items"
              :key="item.path"
              :index="item.path"
              :style="{ animationDelay: `${(navOrder.get(item.path) ?? 0) * 18}ms` }"
            >
              <el-icon><component :is="item.icon" /></el-icon>
              <span>{{ item.title }}</span>
            </el-menu-item>
          </el-menu-item-group>
        </template>
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
        <div class="page-head">
          <!-- 分组小字与侧栏分组呼应：不只看出「在哪一页」，也知道属于哪一组 -->
          <span v-if="pageGroup" class="page-group">{{ pageGroup }}</span>
          <div class="page-title">{{ pageTitle }}</div>
        </div>
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
                <el-dropdown-item command="panel-restart">重启面板服务</el-dropdown-item>
                <el-dropdown-item command="reboot" divided>重启服务器</el-dropdown-item>
                <el-dropdown-item command="shutdown">关机</el-dropdown-item>
              </el-dropdown-menu>
            </template>
          </el-dropdown>
          <button class="mini-btn" type="button" @click="openPwdDialog">改密</button>
          <button class="mini-btn" type="button" @click="logout">退出</button>
        </div>
      </header>
      <section class="content">
        <router-view />
      </section>
    </div>

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

    <!-- 全局 AI 助手悬浮球（4.5：取代独立页面，点击展开对话面板） -->
    <AiBall />
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch, type Component } from "vue";
import { useRoute, useRouter } from "vue-router";
import http from "../api/http";
import AiBall from "../components/AiBall.vue";
import { useThemeStore } from "../stores/theme";
import { useAuthStore } from "../stores/auth";
import {
  Odometer,
  Cpu,
  SetUp,
  Document,
  Folder,
  Box,
  Timer,
  Connection,
  Ship,
  Tools,
  ChatLineRound,
  User,
  CopyDocument,
  Bell,
  Setting,
  Download,
  Tickets,
} from "@element-plus/icons-vue";

const route = useRoute();
const router = useRouter();
const theme = useThemeStore();
const auth = useAuthStore();
const appVersion = __APP_VERSION__;

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

interface NavItem {
  path: string;
  title: string;
  icon: Component;
}
interface NavGroup {
  label: string;
  /** 整组仅 admin 可见 */
  admin?: boolean;
  items: NavItem[];
}

/* 侧边栏导航的**单一数据源**：菜单渲染与顶栏标题都从这里派生。
 * 写在两处必然会有漏项（此前 /tasks 标题就漏了），同源则结构上不可能不一致。
 *
 * 顺序按运维操作流排：看（监控）→ 管（资源）→ 调（调度）→ 护（运维）→ 配（系统）。
 * 「实例」与「软件」同属负载资源，从原来的第 10 位并入资源组。
 * 末组整体 admin-only —— 让权限边界与分组边界重合，普通用户看到的
 * 每一组都是完整的，不会出现「某组里少一项」的碎裂感。 */
const navGroups: NavGroup[] = [
  {
    label: "监控",
    items: [
      { path: "/dashboard", title: "仪表盘", icon: Odometer },
      { path: "/processes", title: "进程", icon: Cpu },
      { path: "/services", title: "服务", icon: SetUp },
      { path: "/logs", title: "日志", icon: Document },
    ],
  },
  {
    label: "资源",
    items: [
      { path: "/files", title: "文件", icon: Folder },
      { path: "/packages", title: "软件", icon: Box },
      { path: "/instances", title: "实例", icon: Ship },
      { path: "/network", title: "网络", icon: Connection },
    ],
  },
  {
    label: "调度",
    items: [
      { path: "/tasks", title: "任务", icon: Tickets },
      { path: "/cron", title: "计划任务", icon: Timer },
    ],
  },
  {
    label: "运维",
    items: [
      { path: "/ops", title: "深度运维", icon: Tools },
      { path: "/sessions", title: "在线会话", icon: ChatLineRound },
    ],
  },
  {
    label: "系统",
    admin: true,
    items: [
      { path: "/users", title: "账号管理", icon: User },
      { path: "/backups", title: "备份管理", icon: CopyDocument },
      { path: "/alerts", title: "告警通知", icon: Bell },
      { path: "/settings", title: "系统设置", icon: Setting },
      { path: "/update", title: "面板更新", icon: Download },
    ],
  },
];

// 拍平一份用于按当前路由反查标题与所属分组
const flatNav = navGroups.flatMap((g) =>
  g.items.map((item) => ({ ...item, group: g.label })),
);
/// path → 在导航中的序号，供侧栏入场动画按顺序错开（跨分组连续编号）
const navOrder = new Map(flatNav.map((item, i) => [item.path, i]));
const currentNav = computed(() => flatNav.find((i) => i.path === route.path));
const pageTitle = computed(() => currentNav.value?.title ?? "LYYS Panel");
const pageGroup = computed(() => currentNav.value?.group ?? "");

// 登出（P1-1）：先请求服务端吊销当前 token（旧 token 立即失效），
// 再清本地凭证。吊销请求失败（如后端已不可达）不阻断本地登出，
// token 本身有 24h 过期兜底。
/// 电源菜单（admin）：面板重启只影响本进程、轮询到恢复即报成功；
/// 服务器重启/关机是整机危险操作，二次确认，指令发出后机器断电
/// 响应回不来（无 response 的网络错误不代表失败）。
async function onPower(cmd: "panel-restart" | "reboot" | "shutdown") {
  if (cmd === "panel-restart") {
    try {
      await ElMessageBox.confirm(
        "将重启面板服务（lyys-panel），期间页面短暂不可用，服务器其他服务不受影响。确定继续？",
        "重启确认",
        { type: "warning", confirmButtonText: "重启", cancelButtonText: "取消" },
      );
    } catch {
      return;
    }
    http.post("/power", { action: cmd }).catch(() => {});
    await new Promise((r) => setTimeout(r, 1500));
    for (let i = 0; i < 20; i++) {
      try {
        await http.get("/health", { timeout: 2000 });
        ElMessage.success("面板已重启");
        return;
      } catch {
        await new Promise((r) => setTimeout(r, 1000));
      }
    }
    ElMessage.warning("重启指令已发出，若页面无法访问请稍后刷新");
    return;
  }
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
  /* 分组后条目增多，矮屏必须能滚动，否则底部几组会被裁掉且无法触达 */
  overflow-y: auto;
  /* 紧凑菜单项：按钮间留足间距，文字贴紧按钮 */
  --el-menu-item-height: 32px;
  --el-menu-base-level-padding: 12px;
}
/* 侧栏滚动条做得极窄且默认透明：导航区不该常驻一条深色竖条，
   只在鼠标移入侧栏时才显形提示可滚动 */
.side-menu::-webkit-scrollbar {
  width: 4px;
}
.side-menu::-webkit-scrollbar-track {
  background: transparent;
}
.side-menu::-webkit-scrollbar-thumb {
  background: transparent;
  border-radius: var(--radius);
}
.side-menu:hover::-webkit-scrollbar-thumb {
  background: var(--el-fill-color-darker);
}
/* 分组标题：小字 + 宽字距，只靠字号与颜色分层 —— 主题约定无边框，
   所以不用分隔线，分组感由标题留白承担 */
.side-menu :deep(.el-menu-item-group__title) {
  padding: 14px 12px 4px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.08em;
  line-height: 1.4;
  color: var(--el-text-color-placeholder);
}
/* 首组紧贴品牌区，不需要额外上间距 */
.side-menu :deep(.el-menu-item-group:first-child .el-menu-item-group__title) {
  padding-top: 2px;
}
.side-menu :deep(.el-menu-item) {
  height: 32px;
  line-height: 32px;
  /* 组内收紧到 2px，组与组之间的层次交给分组标题的留白 */
  margin: 2px 0;
  padding: 0 12px;
  border-radius: var(--radius);
  font-size: 13px;
  transition: background-color 160ms ease-out;
}
.side-menu :deep(.el-menu-item:hover) {
  background: var(--el-fill-color-light);
}
/* 键盘可达性：EP 默认不给菜单项可见焦点，补一个内描边
   （用负 offset 内收，外描边会被侧栏边缘裁掉） */
.side-menu :deep(.el-menu-item:focus-visible) {
  outline: 2px solid var(--el-color-primary);
  outline-offset: -2px;
}
/* 侧栏入场：菜单项自上而下依次自左浮入（延迟由模板内联给出）。
 * 菜单只在挂载时构建一次，路由切换不会重建，所以全程只播一遍，
 * 不会在每次跳转时反复打扰。分组标题与品牌不动，留着当视觉锚点。 */
@media (prefers-reduced-motion: no-preference) {
  .side-menu :deep(.el-menu-item) {
    animation: nav-in 240ms cubic-bezier(0.16, 1, 0.3, 1) backwards;
  }
}
@keyframes nav-in {
  from {
    opacity: 0;
    transform: translateX(-6px);
  }
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
  /* 手机垂直空间紧张：分组标题与条目的间距收紧。
     分组后条目总数未变但多了 5 个组标题，抽屉必然要滚动 ——
     这里只是把滚出去的条目数从 5 个减到 3 个，让「系统」组不至于整组看不见。 */
  .side-menu :deep(.el-menu-item-group__title) {
    padding: 10px 12px 2px;
  }
  .side-menu :deep(.el-menu-item) {
    margin: 1px 0;
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
.page-head {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
/* 分组标识：与侧栏同一份数据源，形成「侧栏在哪一组 → 顶栏再确认一次」的闭环 */
.page-group {
  flex: none;
  padding: 2px 8px;
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.04em;
  color: var(--el-text-color-secondary);
  background: var(--el-fill-color-light);
  border-radius: var(--radius);
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
