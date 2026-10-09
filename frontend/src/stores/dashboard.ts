import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 趋势图卡的分组：一张卡只画一组同量纲的指标。
///
/// 🔴 为什么按组拆成多张卡，而不是一张图里画全部：
/// 单位的量级差得太远（磁盘写入偶发 7.7M/s、网络常年 0、CPU 与内存都在个位数
/// 百分比），塞进一张图要么互相压平、要么需要多套轴 —— 实测都被读成
/// 「意大利面条」。每张卡只放一组指标，各自量程、各自单位，读数才成立。
export type ChartKind = "load" | "network" | "disk";

export const CHART_KINDS: ChartKind[] = ["load", "network", "disk"];

/// 该卡属于哪个趋势分组（不是趋势卡则为 null）
export function chartKindOf(id: DashCard | string): ChartKind | null {
  if (id === "chart") return "load";
  if (id === "chartnet") return "network";
  if (id === "chartdisk") return "disk";
  return null;
}

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
  // 趋势图也是卡片：可拖拽、可缩放、可隐藏，与其他卡片同一套交互。
  // **一组指标一张卡**，见 ChartKind 的说明。
  { id: "chart", label: "负载趋势", list: false },
  { id: "chartnet", label: "网络趋势", list: false },
  { id: "chartdisk", label: "磁盘 I/O 趋势", list: false },
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

/// 该卡是不是趋势图卡
export function isChartCard(id: DashCard | string): boolean {
  return chartKindOf(id) !== null;
}

/// 可选尺寸档位（自定义弹窗里的选项，数组顺序即展示顺序）。
/// 前三档给数值卡，整宽那几档是横向内容（趋势图）需要的。
/// 最后三档只有趋势图够得着 —— 弹窗按卡片的高度上限过滤（见 sizesFor）。
///
/// 🔴 label **必须写明真实格数**（`1×1` 而不是「小」）。
/// 改成数字前用的是「小 / 宽 / 大 / 整宽 / 通栏 / 通栏高 / 通栏加高 / 通栏满高」——
/// 名字里没有任何度量信息，选完之前根本不知道卡片会变成多大；
/// 而且「通栏高 / 通栏加高 / 通栏满高」三者完全分不出差别（实测反馈：没法用）。
export const CARD_SIZES = [
  { key: "1x1", label: "1×1", w: 1, h: 1 },
  { key: "2x1", label: "2×1", w: 2, h: 1 },
  { key: "2x2", label: "2×2", w: 2, h: 2 },
  { key: "4x1", label: "4×1", w: 4, h: 1 },
  { key: "4x2", label: "4×2", w: 4, h: 2 },
  { key: "4x3", label: "4×3", w: 4, h: 3 },
  { key: "4x4", label: "4×4", w: 4, h: 4 },
  { key: "4x6", label: "4×6", w: 4, h: 6 },
  // 排行榜卡用得到的 3 行档（1–3 列）。上面那几个 4 开头的是给趋势图/数值卡
  // 整宽用的；没有这三档，排行榜的「合法档位」里一个都匹配不上、下拉会空着。
  { key: "1x3", label: "1×3", w: 1, h: 3 },
  { key: "2x3", label: "2×3", w: 2, h: 3 },
  { key: "3x3", label: "3×3", w: 3, h: 3 },
] as const;

/// 任意宽高 → 人类可读的格数标签（拖拽时显示实时尺寸用）。
/// 不在档位里的组合也要能显示（拖拽可以产生 3×2 这种中间态）。
export function sizeLabel(w: number, h: number): string {
  return `${w}×${h}`;
}

/// 行高与间距必须和 .cards 的 CSS 保持一致。
/// 行高不再参与任何「像素→格数」换算（已取消拖拽改高度），只作为 CSS 的说明留存。
export const ROW_H = 88;
export const GAP = 12;
export const MAX_CARD_W = 4;
/// 高度上限（供历史配置的钳制用）：趋势 / 列表 3 行，数值卡 1 行
export const MAX_CARD_H = 3;
export const MAX_CARD_H_LIST = 3;
export const MAX_CARD_H_CHART = 3;
/// 趋势图**允许**的最大高度（行）。与后端 `api.rs` 的 `MAX_CARD_H_CHART` 同源（都是 6）。
///
/// 与 `MAX_CARD_H_CHART`（默认高度 = 3 行）刻意分开：默认给 3 行（一整屏看得全），
/// 但用户想拉到 6 行（下拉里的「4×6 通栏满高」）应当允许 —— 后端本来就收 6，
/// 之前把前端上限也压成 3，等于把这一档悄悄砍了。
export const MAX_CARD_H_TREND_MAX = 6;

// ---------- 尺寸模型：按卡片类型定死，用户不可调 ----------
//
// 🔴 为什么取消「自由尺寸」：
// 之前每张卡的宽高都可以拖，于是高度千奇百怪（1/2/3/6 行混排），栅格必然
// 留出补不上的洞 —— 那时用 `grid-auto-flow: dense` 去回填，结果是卡片顺序
// 在视觉上被打乱、整页看起来是「错开的马赛克」。参照物（Grafana / SAP Fiori
// 分析卡）都不让用户自由拖面板高度：面板按类型取固定高度，栅格才对得齐。
//
// 现在：数值卡 1×1、趋势图整宽 3 行、排行榜 1/3 宽 3 行。
// 栅格用默认的 row 流式排列即可 —— 高度只有 1 和 3 两种，同高元素成排出现，
// 数学上不会留洞，**不需要 dense**。
export function isTrendCard(id: DashCard | string): boolean {
  return isChartCard(id);
}

/// 某张卡在指定列数下的跨度。
///
/// 宽度不按 4 列写死：窄屏只有 1–2 列时，趋势图的「整宽」就是 1–2 格。
export function spanFor(id: DashCard | string, cols: number): { w: number; h: number } {
  const c = Math.max(1, cols);
  if (isTrendCard(id)) return { w: c, h: MAX_CARD_H_CHART };
  if (isListCard(id)) {
    // 排行榜要横向空间放「名次 / 名称 / 数值」，又不该独占一整行：
    // 3 列时正好三张一排，2 列时两张一排，1 列时整宽。
    return { w: Math.max(1, Math.floor(c / 3)), h: MAX_CARD_H_LIST };
  }
  return { w: 1, h: 1 };
}

/// 默认尺寸（4 列基准，供后端校验与历史配置参考）
export function defaultSize(id: DashCard): { w: number; h: number } {
  return spanFor(id, MAX_CARD_W);
}

/// 卡片几何的允许区间。自由布局下不再有"类型档位" —— 只剩**最小尺寸**
/// （防止拖成看不见的点）与量级上限（与后端校验对齐，见 api.rs 的 MAX_CARD_W/H）。
/// 该不该成排、留不留洞，都归用户自己摆（要整齐用 view 里的「自动排列」）。
export function sizeLimits(_id: DashCard | string): {
  wMin: number;
  wMax: number;
  hMin: number;
  hMax: number;
} {
  return { wMin: MIN_CARD_W, wMax: 100, hMin: MIN_CARD_H, hMax: 4096 };
}

/// 卡片的最小尺寸：宽 8%（1440 宽时约 96px，再窄标题就截完了）、高 64px。
/// 🔴 64 不是一个喜欢的数 —— 它是**一行卡内容区可用高度**（88 − 上下内距 12×2），
/// 再矮卡里的"标签 + 读数 + 明细 + 量度条"就会被裁。
/// 取一个数并钳到 [lo, hi]；不是有限数就用 fallback（老配置缺字段时用）
function spanIn(v: unknown, lo: number, hi: number, fallback: number): number {
  if (typeof v !== "number" || !Number.isFinite(v)) return fallback;
  const n = Math.floor(v);
  return n < lo ? lo : n > hi ? hi : n;
}

export const MIN_CARD_W = 8;
export const MIN_CARD_H = 64;

/// 按类型自动排列成一份绝对几何。**「自动排列」按钮与旧配置迁移共用这一份。**
///
/// 规则与从前的栅格一致（这样从栅格搬过来的布局看起来没变）：
///   数值卡 1 列、排行榜 1 列 3 行、趋势卡整宽 —— 依次填满 4 列。
/// 几何：列宽 25% 起、卡宽 24%（留 1% ≈ 12px 作横向间距）；行高 88 + 间距 12 = 100px，
/// 卡片高 = 88 / 188 / 288（1/2/3 行）✓ 与 `.card` 的内距预算一致。
/// ⚠️ 宽度用百分数、间距也用百分数（1%），是为了**宽度保持整数**（后端校验只收整数）——
/// 用像素间距的话，1440 下算出的 24.17% 会被后端拒掉。
export function autoLayout(ids: readonly DashCard[]): CardConfig[] {
  const COLS = 4;
  const COL_PITCH = 25; // 每列起点（%）
  const COL_W = 24; // 卡片宽（%），留 1% 作横向间距
  const ROW_PITCH = 100; // 行高 88 + 纵向间距 12
  const heights = (id: DashCard) =>
    isTrendCard(id) ? 3 : isListCard(id) ? 3 : 1;
  const widths = (id: DashCard) => (isTrendCard(id) ? COLS : 1);
  const out: CardConfig[] = [];
  let col = 0;
  let y = 0;
  let rowH = 1;
  for (const id of ids) {
    const w = widths(id);
    const rows = heights(id);
    if (col + w > COLS) {
      // 本行放不下 → 换行
      y += rowH * ROW_PITCH;
      col = 0;
      rowH = 1;
    }
    const full = w >= COLS;
    out.push({
      id,
      x: full ? 0 : col * COL_PITCH,
      y,
      w: full ? 100 : COL_W,
      h: rows * 88 + (rows - 1) * 12,
    });
    col += w;
    rowH = Math.max(rowH, rows);
    if (col >= COLS) {
      y += rowH * ROW_PITCH;
      col = 0;
      rowH = 1;
    }
  }
  return out;
}

/// 一张卡片的配置：id + 占几列（w）几行（h）。
/// 🔴 尺寸现在由**卡片类型**决定（见 spanFor / defaultSize），这里存的 w/h 只作
/// 历史兼容与后端结构校验用 —— 读写时一律不采纳，统一按类型重算。
export interface CardConfig {
  id: DashCard;
  /// 距画布左边的**百分数**（0–100）。
  /// 🔴 用百分数而不是像素：换屏宽时布局按比例缩放，窄屏不会溢出。
  /// 代价是横向步长 = 容器宽度的 1%（1440 宽时约 12px）—— 比从前"一整列 294px"细得多。
  /// ⚠️ 暂时可选：自由布局的渲染侧（view）正在改，改完之前老字段仍是唯一真源。
  x?: number;
  /// 距画布顶部的**像素**（同上，暂可选）
  y?: number;
  /// 宽度：**百分数**（0–100）
  w: number;
  /// 高度：**像素**
  h: number;
}

/// 高度上限（保留导出：历史配置的钳制要用它校验）
export function maxH(id: DashCard | string): number {
  return spanFor(id, MAX_CARD_W).h;
}

/// 历史遗留：档位选择已随「自由尺寸」一起移除。
/// 保留空实现是为了不必同时改后端校验与老配置的读取路径。
/// 弹窗里某张卡可选的尺寸档位：**按该类型的允许区间过滤**（见 sizeLimits），
/// 保证选得到的组合都合法 —— 否则保存时会被钳掉，而回显又对不上（下拉显示
/// 的档位与卡片实际跨度不一致，看着就像「改了没生效」）。
export function sizesFor(id: DashCard | string) {
  const lim = sizeLimits(id);
  return CARD_SIZES.filter(
    (s) => s.w >= lim.wMin && s.w <= lim.wMax && s.h >= lim.hMin && s.h <= lim.hMax,
  );
}

/// 默认卡片顺序：**按类型分组排布**，让同高元素成排出现、栅格自动排满。
///
/// 🔴 它同时决定**默认渲染顺序** —— view 的 visibleCards 在未定制时按本列表生成
/// （见 stores 的 defaultConfig 用法）。当前卡片只有三种尺寸：数值卡 1×1、
/// 排行榜 1/3 宽 × 3 行、趋势图整宽 × 3 行。据此排布，4 列栅格正好零空洞：
///   · 第 1 排：4 张数值卡铺满一行；
///   · 第 2–4 排：3 张排行榜占左 3 列（各 3 行），右列纵向叠 3 张数值卡
///     —— 3 + 3 = 6 张，把这块 4×4 区域铺满、无洞；
///   · 之后：3 张趋势图各占整宽、各 3 行。
/// 已并入别的卡片、默认不渲染的散卡（swap / partitions / uptime / procs / cores）
/// 排在末尾：它们不占格，位置只影响自定义弹窗里的列出顺序。
///
/// 早先的顺序（数值卡 → 趋势图 → 排行榜）会把排行榜挤到 7 张数值卡之后，
/// 末排剩 3 格力排行榜、空出第 4 格 —— 这正是要消除的洞。
export const DEFAULT_ORDER: readonly DashCard[] = [
  // 第 1 排：4 张 KPI（各 1×1，正好一行）
  "cpu",
  "mem",
  "disk",
  "diskio",
  // 第 2–4 排：3 张排行榜（左 3 列、各 3 行）+ 右列 3 张 KPI
  "topcpu",
  "topmem",
  "topdisk",
  "net",
  "load",
  "sysinfo",
  // 趋势图：整宽、各 3 行
  "chart",
  "chartnet",
  "chartdisk",
  // 已并入其它卡片、默认不渲染的散卡（保留 id 对齐后端 DASHBOARD_CARDS）
  "swap",
  "partitions",
  "uptime",
  "procs",
  "cores",
];

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
    /// 全部合法 id 的顺序（含没有几何的老配置）—— 重排时沿用这个顺序
    const order: DashCard[] = [];
    /// 见到过"没有绝对几何"的条目（老配置或新补的卡）→ 结束时整份重排
    let legacy = false;
    for (const raw of list) {
      // 兼容三种历史写法：字符串（最老，只有 id）或 {id, w, h}。
      // 宽高仍可由用户调整，但一律**钳到该类型的允许区间**（见 sizeLimits）——
      // 缺 w/h 的老配置用 defaultSize 兜底。
      const obj = (typeof raw === "object" && raw !== null ? raw : {}) as {
        id?: unknown;
        x?: unknown;
        y?: unknown;
        w?: unknown;
        h?: unknown;
      };
      const id = typeof raw === "string" ? raw : obj.id;
      if (typeof id !== "string" || !ALL.includes(id) || seen.has(id)) continue;
      seen.add(id);
      order.push(id as DashCard);
      // 只有**四项几何齐全**才算新格式；缺任何一项按老配置处理。
      // 🔴 不做逐项换算：老格式的 w/h 是「跨几格」，与新格式的「百分数 / 像素」
      // 单位完全不同，硬换会得到一堆错位（甚至 2px 宽的）卡片 —— 整份重排更干净。
      if (
        typeof obj.x === "number" &&
        typeof obj.y === "number" &&
        typeof obj.w === "number" &&
        typeof obj.h === "number"
      ) {
        const w = spanIn(obj.w, MIN_CARD_W, 100, 100);
        out.push({
          id: id as DashCard,
          // 🔴 x 的上界是「100 − 宽」，不是 100：x 是左边缘的百分比，
          // 上界取 100 的话卡片会整块落到画布右侧外面（用户报过）。
          x: spanIn(obj.x, 0, 100 - w, 0),
          y: spanIn(obj.y, 0, 4096, 0),
          w,
          h: spanIn(obj.h, MIN_CARD_H, 4096, 88),
        });
      } else {
        legacy = true;
      }
    }
    if (!out.length) return null;
    // 🔴 缺省必须是 **0**，不能是「当前全集」：
    // 旧配置里没有 known（这个字段是本次才加的），若拿当前全集当边界，
    // 循环区间为空 —— 等于一张都不补，新卡片永远不显示（实测就是这个问题）。
    // 语义：known = 「保存时已知的卡片数量」，缺省 = 完全未知 → 补全部缺失的。
    const boundary = known ?? 0;
    for (let i = boundary; i < DASH_CARDS.length; i++) {
      const id = DASH_CARDS[i];
      if (seen.has(id)) continue;
      seen.add(id);
      order.push(id);
      // 新补进来的卡没有几何 → 连同老配置一起整份重排
      legacy = true;
    }
    // 老配置 / 有新增卡 → 按顺序整份自动排列；否则原样保留用户摆的位置
    if (legacy || out.length !== order.length) return autoLayout(order);
    return out;
    // 🔴 这里**不再**按类型重排。
    // 曾经有一版在这里 sort 成 DEFAULT_ORDER 的顺序（理由是「同高元素成排、栅格才排得满」），
    // 后果是：**每次保存，用户刚拖出来的位置立刻被打回类型分组** —— 松手卡片就自己跳走，
    // 拖动等于没拖。成排的收益归**默认布局**（见 view 的 defaultConfig / DEFAULT_ORDER），
    // 用户手排的顺序一经保存就必须原样保留；栅格留的洞由 `.cards` 的
    // `grid-auto-flow: dense` 回填。
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
