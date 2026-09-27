import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 当前登录账号状态（2.x 账号体系）。
/// token 存 localStorage；角色/强制改密标记以服务端 /me 为准，
/// 进入布局时刷新一次，改角色/改密后即时生效。
export const useAuthStore = defineStore("auth", () => {
  const username = ref("");
  const role = ref("viewer");
  const mustChange = ref(false);
  const loaded = ref(false);

  const isAdmin = () => role.value === "admin";
  const canWrite = () => role.value === "admin" || role.value === "operator";

  async function load() {
    const { data } = await http.get("/me");
    username.value = data.username;
    role.value = data.role;
    mustChange.value = data.must_change;
    loaded.value = true;
  }

  function clear() {
    username.value = "";
    role.value = "viewer";
    mustChange.value = false;
    loaded.value = false;
  }

  return { username, role, mustChange, loaded, isAdmin, canWrite, load, clear };
});
