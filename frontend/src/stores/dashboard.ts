import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 仪表盘卡片：id + 展示名 + 是否列表卡。
///
/// 顺序即默认展示顺序，也决定卡片在栅格里的先后。
/// 🔴 必须与后端 api.rs 的 DASHBOARD_CARDS 一一对应（那边的 id 列表）。
/// 高度上限由 `maxH()` 按「趋势图 / 列表卡 / 数值卡」三类给出，
/// 不在这里逐张写 —— 那样加一种卡就要在两处维护同一个数字。
export const CARD_META = [
  { id: "cpu", label: "CPU", list: false },
  { id: "mem", label: "内存", list: false },
  { id: "disk", label: "磁盘", list: false },
  { id: "swap", label: "交换区", list: false },
  { id: "diskio", label: "磁盘 I/O", list: false },
  { id: "net", label: "网络", list: false },
  { id: "load", label: "负载", list: false },
  { id: "procs", label: "进程", list: false },
  { id: "partitions", label: "分区", list: false },
  { id: "uptime", label: "运行时长", list: false },
  { id: "cores", label: "CPU 规格", list: false },
  { id: "sysinfo", label: "系统", list: false },
  // 排行榜卡：卡内是 5 行列表，高度需求见 MAX_CARD_H_LIST
  { id: "topcpu", label: "CPU Top 5 进程", list: true },
  { id: "topmem", label: "内存 Top 5 进程", list: true },
  { id: "topdisk", label: "磁盘占用 Top 5 目录", list: true },
  // 趋势图也是一张卡片：可拖拽、可缩放、可隐藏，与其他卡片同一套交互
  { id: "chart", label: "负载趋势", list: false },
] as const;

export const DASH_CARDS = CARD_META.map((c) => c.id) as unknown as readonly DashCard[];
export type DashCard = (typeof CARD_META)[number]["id"];

/// 卡片展示名（自定义配置弹窗里用）
export const CARD_LABELS = Object.fromEntries(
  CARD_META.map((c) => [c.id, c.label]),
) as Record<DashCard, string>;

/// 该卡是不是横向列表（排行榜）：模板与尺寸上限都要区别对待
export function isListCard(id: DashCard | string): boolean {
  return CARD_META.some((c) => c.id === id && c.list);
}

/// 可选尺寸档位（自定义弹窗里的选项，数组顺序即展示顺序）。
/// 前三档给数值卡，整宽那几档是横向内容（趋势图）需要的。
/// 最后三档只有趋势图够得着 —— 弹窗按卡片的高度上限过滤（见 sizesFor）。
export const CARD_SIZES = [
  { key: "1x1", label: "小", w: 1, h: 1 },
  { key: "2x1", label: "宽", w: 2, h: 1 },
  { key: "2x2", label: "大", w: 2, h: 2 },
  { key: "4x1", label: "整宽", w: 4, h: 1 },
  { key: "4x2", label: "通栏", w: 4, h: 2 },
  { key: "4x3", label: "通栏高", w: 4, h: 3 },
  { key: "4x4", label: "通栏加高", w: 4, h: 4 },
  { key: "4x6", label: "通栏满高", w: 4, h: 6 },
] as const;

/// 行高与间距必须和 .cards 的 CSS 保持一致 ——
/// 拖拽缩放要靠它们做像素换算（见 Dashboard.onPointerMove 的分母）；
/// 跨度上限与后端校验同源。
/// 改这两个数之前先看 Dashboard.vue 里 .cards 的 grid-auto-rows / gap，
/// 两边漏改一边，缩放就会按错误的格宽换算、拖一下跳两格。
export const ROW_H = 88;
export const GAP = 12;
export const MAX_CARD_W = 4;
/// 数值卡的高度上限：3 行（约 288px）。再高只是把一块空白拉长 ——
/// 卡里就标签 + 数值 + 进度条，内容并不跟着长。
export const MAX_CARD_H = 3;
/// 排行榜卡的高度上限：卡里是 5 行列表，需要的高度落在「2 行不够、3 行正好」。
/// 直接用 3 行，并与后端 `MAX_CARD_H` 同源 —— 排行榜是列表卡，本来就该有
/// 比数值卡更宽的高度余量。
export const MAX_CARD_H_LIST = 3;
/// 趋势图卡单独放宽的高度上限：折线图越高越好读，3 行太局促。
/// 宽度不用单独放宽 —— 4 列就是栅格整宽。
/// 🔴 与后端 api.rs 的 MAX_CARD_H_CHART 同源，改一处必须改另一处。
export const MAX_CARD_H_CHART = 6;

/// 某张卡的高度上限：趋势图放得开，排行榜按列表卡，其余沿用数值卡的 3 行
export function maxH(id: DashCard | string): number {
  if (id === "chart") return MAX_CARD_H_CHART;
  if (isListCard(id)) return MAX_CARD_H_LIST;
  return MAX_CARD_H;
}

/// 弹窗里某张卡可选的尺寸档位：按该卡的高度上限过滤 ——
/// 趋势图能看到整栏 3 / 4 / 6 行，数值卡仍到 3 行为止
export function sizesFor(id: DashCard | string) {
  const limit = maxH(id);
  return CARD_SIZES.filter((s) => s.h <= limit);
}

/// 一张卡片的配置：显示与否、排在第几、占多大
export interface CardConfig {
  id: DashCard;
  w: number;
  h: number;
}

/// 各卡片的默认尺寸。趋势图要横向空间（1 格宽看不清趋势）；
/// 排行榜卡要三行高才放得下 5 行列表（1 行高会压住最后一行）。
const DEFAULT_SIZE: Partial<Record<DashCard, { w: number; h: number }>> = {
  chart: { w: 4, h: 2 },
  topcpu: { w: 1, h: MAX_CARD_H_LIST },
  topmem: { w: 1, h: MAX_CARD_H_LIST },
  topdisk: { w: 1, h: MAX_CARD_H_LIST },
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
  ///
  /// `addMissing` 只用于**从服务端加载**时：把「保存配置时还不存在的卡片」
  /// 补到末尾，否则新做的卡片（排行榜等）永远不显示，用户只能去自定义弹窗里
  /// 手动勾选 —— 而「新增了卡片却看不到」会被当成功能没做完。
  ///
  /// 🔴 必须是「当时已知的卡片」而不是「当前全部卡片」：用户主动隐藏的卡片
  /// 同样不在 `cards` 里，若按当前全集去补，每次打开页面都会把他特意隐藏的卡片
  /// 又塞回来。所以保存时把「已知集合大小」写进 known，加载时只补这个边界之后
  /// 新增的卡片。
  function normalize(list: unknown, known?: number): CardConfig[] | null {
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
        // 上限按卡片类型给：此刻 id 还没过白名单，先看它是不是字符串。
        // 非法 id 会在下面被挡掉，这里的取值只影响钳制结果
        h = span(o.h, typeof id === "string" ? maxH(id) : MAX_CARD_H);
      } else {
        continue;
      }
      if (typeof id !== "string" || !ALL.includes(id) || seen.has(id)) continue;
      seen.add(id);
      out.push({ id: id as DashCard, w, h });
    }
    if (!out.length) return null;
    // 🔴 缺省必须是 **0**，不能是「当前全集」：
    // 旧配置里没有 known（这个字段是本次才加的），若拿当前全集当边界，
    // 循环区间为空 —— 等于一张都不补，新卡片永远不显示（实测就是这个问题）。
    // 语义：known = 「保存时已知的卡片数量」，缺省 = 完全未知 → 补全部缺失的。
    const boundary = known ?? 0;
    for (let i = boundary; i < DASH_CARDS.length; i++) {
      const id = DASH_CARDS[i];
      if (!seen.has(id)) out.push({ id, ...defaultSize(id) });
    }
    return out;
  }

  /// 保存时一并写入当前已知的卡片数量（见 normalize 的 known 参数）
  function knownCount(): number {
    return DASH_CARDS.length;
  }

  /// 登录后从服务端拉取配置；失败/未定制都按默认展示，不打断页面
  async function load() {
    try {
      const { data } = await http.get("/dashboard-config");
      cards.value = normalize(data?.config?.cards, data?.config?.known);
    } catch {
      cards.value = null;
    }
  }

  /// 保存配置到服务端并即时生效；null = 恢复默认（服务端删除配置）
  async function save(next: CardConfig[] | null) {
    await http.post(
      "/dashboard-config",
      next ? { cards: next, known: knownCount() } : null,
    );
    cards.value = normalize(next, knownCount());
  }

  return { cards, load, save };
});
