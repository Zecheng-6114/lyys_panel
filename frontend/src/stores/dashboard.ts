import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 仪表盘卡片白名单（与后端 api.rs 的 DASHBOARD_CARDS 保持一致）。
/// 顺序即默认展示顺序，也决定卡片在栅格里的先后。
export const DASH_CARDS = [
  "cpu",
  "mem",
  "disk",
  "swap",
  "diskio",
  "net",
  "load",
  "procs",
  "partitions",
  "uptime",
  "cores",
  "sysinfo",
  // 趋势图也是一张卡片：可拖拽、可缩放、可隐藏，与其他卡片同一套交互
  "chart",
] as const;
export type DashCard = (typeof DASH_CARDS)[number];

/// 卡片展示名（自定义配置弹窗里用）
export const CARD_LABELS: Record<DashCard, string> = {
  cpu: "CPU",
  mem: "内存",
  disk: "磁盘",
  swap: "交换区",
  diskio: "磁盘 I/O",
  net: "网络",
  load: "负载",
  procs: "进程",
  partitions: "分区",
  uptime: "运行时长",
  cores: "CPU 规格",
  sysinfo: "系统",
  chart: "负载趋势",
};

/// 可选尺寸档位（自定义弹窗里的选项，数组顺序即展示顺序）。
/// 前几档给数值卡，后两档是整宽 —— 趋势图这类横向内容需要。
export const CARD_SIZES = [
  { key: "1x1", label: "小", w: 1, h: 1 },
  { key: "2x1", label: "宽", w: 2, h: 1 },
  { key: "2x2", label: "大", w: 2, h: 2 },
  { key: "4x1", label: "整宽", w: 4, h: 1 },
  { key: "4x2", label: "通栏", w: 4, h: 2 },
] as const;

/// 栅格常量：行高与间距必须和 .cards 的 CSS 保持一致 ——
/// 拖拽缩放要靠它们做像素换算（见 Dashboard.onPointerMove 的分母）；
/// 跨度上限与后端校验同源。
/// 改这两个数之前先看 Dashboard.vue 里 .cards 的 grid-auto-rows / gap，
/// 两边漏改一边，缩放就会按错误的格宽换算、拖一下跳两格。
export const ROW_H = 88;
export const GAP = 12;
export const MAX_CARD_W = 4;
export const MAX_CARD_H = 3;

/// 一张卡片的配置：显示与否、排在第几、占多大
export interface CardConfig {
  id: DashCard;
  w: number;
  h: number;
}

/// 各卡片的默认尺寸。只有趋势图不是 1×1：折线图要横向空间，1 格宽看不清趋势。
const DEFAULT_SIZE: Partial<Record<DashCard, { w: number; h: number }>> = {
  chart: { w: 4, h: 2 },
};

/// 取某张卡片的默认尺寸（未列出的都是 1×1）
export function defaultSize(id: DashCard): { w: number; h: number } {
  return DEFAULT_SIZE[id] ?? { w: 1, h: 1 };
}

/// 跨度钳制：超出范围一律退回 1 —— 坏数据不该把布局弄坏
function span(v: unknown, max: number): number {
  const n = typeof v === "number" ? Math.floor(v) : 1;
  return n >= 1 && n <= max ? n : 1;
}

const ALL: readonly string[] = DASH_CARDS;

/// 仪表盘自定义（4.2）：卡片显隐 / 顺序 / 尺寸，配置存服务端 settings 表
/// （与主题定制同一存储机制，GET 全员可读、保存仅 admin）。
export const useDashboardStore = defineStore("dashboard", () => {
  /// 当前配置；null = 未定制（全部卡片按默认顺序与默认尺寸展示）
  const cards = ref<CardConfig[] | null>(null);

  /// 把任何输入规整成合法配置（返回 null 表示按默认展示）。
  ///
  /// 元素兼容两种写法 —— 尺寸是后加的，**老配置必须一直读得懂**，
  /// 否则老用户一打开页面就会以为自己的布局丢了：
  ///   - 字符串（最老，等价 1×1）
  ///   - `{id, w, h}`
  function normalize(list: unknown): CardConfig[] | null {
    if (!Array.isArray(list)) return null;
    const seen = new Set<string>();
    const out: CardConfig[] = [];
    for (const raw of list) {
      let id: unknown;
      let w = 1;
      let h = 1;
      if (typeof raw === "string") {
        id = raw;
      } else if (raw && typeof raw === "object") {
        const o = raw as Record<string, unknown>;
        id = o.id;
        w = span(o.w, MAX_CARD_W);
        h = span(o.h, MAX_CARD_H);
      } else {
        continue;
      }
      if (typeof id !== "string" || !ALL.includes(id) || seen.has(id)) continue;
      seen.add(id);
      out.push({ id: id as DashCard, w, h });
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
  async function save(next: CardConfig[] | null) {
    await http.post("/dashboard-config", next ? { cards: next } : null);
    cards.value = normalize(next);
  }

  return { cards, load, save };
});
