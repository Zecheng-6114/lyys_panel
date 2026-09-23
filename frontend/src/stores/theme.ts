import { defineStore } from "pinia";
import { ref } from "vue";

// 主题 store：亮/暗切换，默认跟随系统，持久化到 localStorage
export const useThemeStore = defineStore("theme", () => {
  const stored = localStorage.getItem("panel_theme");
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const isDark = ref(stored ? stored === "dark" : prefersDark);

  function apply() {
    document.documentElement.classList.toggle("dark", isDark.value);
  }

  function toggle() {
    isDark.value = !isDark.value;
    localStorage.setItem("panel_theme", isDark.value ? "dark" : "light");
    apply();
  }

  apply();
  return { isDark, toggle };
});
