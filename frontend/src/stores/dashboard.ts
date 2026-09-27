import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 仪表盘卡片白名单与默认顺序（与后端 api.rs 的 DASHBOARD_CARDS 保持一致）
export const DASH_CARDS = ["cpu", "mem", "disk", "net"] as const;
export type DashCard = (typeof DASH_CARDS)[number];

/// 卡片展示名（自定义配置弹窗里用）
export const CARD_LABELS: Record<DashCard, string> = {
  cpu: "CPU",
  mem: "内存",
  disk: "磁盘",
  net: "网络",
};

const ALL: readonly string[] = DASH_CARDS;

/// 仪表盘自定义（4.2）：卡片显隐/顺序，配置存服务端 settings 表
/// （与主题定制同一存储机制，GET 全员可读、保存仅 admin）。
export const useDashboardStore = defineStore("dashboard", () => {
  /// 当前配置；null = 未定制（全部卡片按默认顺序展示）
  const cards = ref<DashCard[] | null>(null);

  /// 把任意数组规整成合法配置：过滤未知卡片、去重；空数组视为未定制
  function normalize(list: unknown): DashCard[] | null {
    if (!Array.isArray(list)) return null;
    const seen = new Set<string>();
    const out: DashCard[] = [];
    for (const c of list) {
      if (typeof c === "string" && ALL.includes(c) && !seen.has(c)) {
        seen.add(c);
        out.push(c as DashCard);
      }
    }
    return out.length ? out : null;
  }

  /// 登录后从服务端拉取配置；失败/未定制都按默认展示，不打断页面
  async function load() {
    try {
      const { data } = await http.get("/dashboard-config");
      cards.value = normalize(data?.config?.cards);
    } catch {
      cards.value = null;
    }
  }

  /// 保存配置到服务端并即时生效；null = 恢复默认（服务端删除配置）
  async function save(next: DashCard[] | null) {
    await http.post("/dashboard-config", next ? { cards: next } : null);
    cards.value = normalize(next);
  }

  return { cards, load, save };
});
