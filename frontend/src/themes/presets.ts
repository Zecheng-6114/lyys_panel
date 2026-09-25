import type { ThemeConfig } from "../stores/theme";

/// 内置主题预设。深色/浅色就是两个独立主题（预设），不存在模式切换。
export interface ThemePreset {
  id: string;
  label: string;
  config: ThemeConfig;
}

export const PRESETS: ThemePreset[] = [
  {
    id: "light",
    label: "浅色",
    config: {
      version: 1,
      name: "浅色",
      colors: {
        primary: "#111111",
        bg_page: "#f5f5f5",
        bg_card: "#ffffff",
        text: "#1a1a1a",
      },
    },
  },
  {
    id: "dark",
    label: "深色",
    config: {
      version: 1,
      name: "深色",
      colors: {
        // 深色主题：主色取近白（反色块 = 浅底深字），派生色由 store 按暗底方向计算
        primary: "#e8e8e8",
        bg_page: "#141414",
        bg_card: "#1d1d1d",
        text: "#e8e8e8",
      },
    },
  },
  {
    id: "paper",
    label: "柔和纸色",
    config: {
      version: 1,
      name: "柔和纸色",
      colors: {
        primary: "#3d3a34",
        bg_page: "#f5f2ea",
        bg_card: "#fbf9f4",
        text: "#332f28",
      },
    },
  },
  {
    id: "contrast",
    label: "高对比",
    config: {
      version: 1,
      name: "高对比",
      radius: 0,
      colors: {
        primary: "#000000",
        bg_page: "#ffffff",
        bg_card: "#ffffff",
        text: "#000000",
      },
    },
  },
];
