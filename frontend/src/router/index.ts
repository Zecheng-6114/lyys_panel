import { createRouter, createWebHistory } from "vue-router";
import type { RouteRecordRaw } from "vue-router";

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
        path: "docker",
        name: "docker",
        component: () => import("../views/Docker.vue"),
      },
      // AI 助手功能暂时停用（用户决定），路由注释；恢复时放开下面 4 行
      // {
      //   path: "ai",
      //   name: "ai",
      //   component: () => import("../views/Ai.vue"),
      // },
    ],
  },
];

const router = createRouter({
  history: createWebHistory(),
  routes,
});

// 全局前置守卫：无 token 时重定向到登录页
router.beforeEach((to) => {
  const token = localStorage.getItem("panel_token");
  if (!to.meta.public && !token) {
    return { name: "login" };
  }
  if (to.name === "login" && token) {
    return { name: "dashboard" };
  }
});

export default router;
