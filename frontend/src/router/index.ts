import { createRouter, createWebHistory } from "vue-router";
import type { RouteRecordRaw } from "vue-router";
import { useAuthStore } from "../stores/auth";
import { basePath } from "../base";

const routes: RouteRecordRaw[] = [
  {
    path: "/login",
    name: "login",
    component: () => import("../views/Login.vue"),
    meta: { public: true },
  },
  {
    path: "/",
    component: () => import("../layout/AppLayout.vue"),
    children: [
      { path: "", redirect: "/dashboard" },
      {
        path: "dashboard",
        name: "dashboard",
        component: () => import("../views/Dashboard.vue"),
      },
      {
        path: "processes",
        name: "processes",
        component: () => import("../views/Processes.vue"),
      },
      {
        path: "services",
        name: "services",
        component: () => import("../views/Services.vue"),
      },
      {
        path: "logs",
        name: "logs",
        component: () => import("../views/Logs.vue"),
      },
      {
        path: "files",
        name: "files",
        component: () => import("../views/Files.vue"),
      },
      {
        path: "packages",
        name: "packages",
        component: () => import("../views/Packages.vue"),
      },
      {
        // P2-1 作业队列：长操作的后台进度与输出
        path: "tasks",
        name: "tasks",
        component: () => import("../views/Tasks.vue"),
      },
      {
        path: "cron",
        name: "cron",
        component: () => import("../views/Cron.vue"),
      },
      {
        path: "network",
        name: "network",
        component: () => import("../views/Network.vue"),
      },
      {
        // 防火墙管理：改规则/启停影响主机暴露面，仅 admin
        path: "firewall",
        name: "firewall",
        component: () => import("../views/Firewall.vue"),
        meta: { adminOnly: true },
      },
      {
        // 实例视图：容器与 systemd 服务的统一入口，块内直达日志 / 进程
        path: "instances",
        name: "instances",
        component: () => import("../views/Instances.vue"),
      },
      {
        // 4.3 深度运维：SMART 磁盘健康、unit 文件查看、容器日志流
        path: "ops",
        name: "ops",
        component: () => import("../views/Ops.vue"),
      },
      {
        path: "sessions",
        name: "sessions",
        component: () => import("../views/Sessions.vue"),
      },
      {
        // 交互式终端：等价于任意命令执行，仅 admin
        path: "terminal",
        name: "terminal",
        component: () => import("../views/Terminal.vue"),
        meta: { adminOnly: true },
      },
      {
        // 2.2 账号管理：仅 admin
        path: "users",
        name: "users",
        component: () => import("../views/Users.vue"),
        meta: { adminOnly: true },
      },
      // 3.1/3.2/3.3：运维管理页，仅 admin
      {
        path: "backups",
        name: "backups",
        component: () => import("../views/Backups.vue"),
        meta: { adminOnly: true },
      },
      {
        path: "update",
        name: "update",
        component: () => import("../views/Update.vue"),
        meta: { adminOnly: true },
      },
      {
        path: "alerts",
        name: "alerts",
        component: () => import("../views/Alerts.vue"),
        meta: { adminOnly: true },
      },
      {
        // 系统设置（当前仅 AI API 配置）：保存仅 admin，GET /ai/config 登录即可
        path: "settings",
        name: "settings",
        component: () => import("../views/Settings.vue"),
        meta: { adminOnly: true },
      },
    ],
  },
];

const router = createRouter({
  // 基路径取自服务端注入的 <base href>：启用安全入口时路由也需带上同一前缀
  history: createWebHistory(basePath()),
  routes,
});

// 全局前置守卫：无 token 时重定向到登录页；adminOnly 路由按角色拦截（2.1）
router.beforeEach(async (to) => {
  const token = localStorage.getItem("panel_token");
  if (!to.meta.public && !token) {
    return { name: "login" };
  }
  if (to.name === "login" && token) {
    return { name: "dashboard" };
  }
  if (to.meta.adminOnly && token) {
    const auth = useAuthStore();
    if (!auth.loaded) {
      try {
        await auth.load();
      } catch {
        /* 拉取失败交给 401 拦截器处理 */
      }
    }
    if (auth.role !== "admin") {
      return { name: "dashboard" };
    }
  }
});

export default router;
