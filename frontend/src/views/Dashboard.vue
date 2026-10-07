<template>
  <div class="dash">
    <div class="toolbar">
      <div class="spacer" />
      <!-- 4.2 卡片自定义入口（admin 专属，保存走 RequireRole<2> 接口） -->
      <button v-if="auth.isAdmin()" class="mini-btn" type="button" @click="openCustom">
        自定义卡片
      </button>
    </div>
    <div
      ref="gridEl"
      class="cards"
      :style="{ gridTemplateColumns: `repeat(${cols}, 1fr)` }"
    >
      <!-- 4.2 卡片按用户配置的顺序与尺寸渲染（先到先得，自动排列）。
           按住卡片拖动换位；右下角拖拽改尺寸；松手即存服务端。 -->
      <div
        v-for="(card, i) in visibleCards"
        :key="card.id"
        class="card"
        :class="{
          'card--lg': cellH(card) > 1,
          'card--dragging': drag.id === card.id,
          'card--drop': dropIndex === i && drag.id !== null && drag.id !== card.id,
        }"
        :data-index="i"
        :style="{
          gridColumn: `span ${cellW(card)}`,
          gridRow: `span ${cellH(card)}`,
          // 入场交错：靠内联 delay 而非 CSS 变量，省掉一层自定义属性
          animationDelay: `${i * 35}ms`,
          // 拖拽中卡片跟着指针走（不改布局，只做视觉位移）
          transform:
            drag.id === card.id ? `translate(${drag.dx}px, ${drag.dy}px)` : undefined,
        }"
        @pointerdown="onCardPointerDown($event, i)"
      >
        <!-- 趋势图卡：头部放标题与时间窗，图表撑满剩余高度 -->
        <template v-if="card.isChart">
          <div class="chart-head">
            <span class="card-label">{{ card.label }}（{{ rangeLabel }}）</span>
            <div class="range-group">
              <button
                v-for="r in RANGES"
                :key="r.key"
                class="mini-btn mini-btn--sm"
                :class="{ 'mini-btn--on': rangeKey === r.key }"
                type="button"
                @click="setRange(r.key)"
              >
                {{ r.label }}
              </button>
            </div>
          </div>
          <div id="dash-chart" class="chart" />
        </template>
        <template v-else>
          <div class="card-label">{{ card.label }}</div>
          <div class="card-value">{{ card.value }}</div>
          <!-- 进度条只表达「距离上限还有多少」：有上限的指标才画槽，
               速率/计数/静态信息不画（详见 .bar--none 的说明） -->
          <div class="bar" :class="{ 'bar--none': card.bar === null }">
            <i v-if="card.bar !== null" :style="{ width: card.bar + '%' }" />
          </div>
        </template>
        <!-- 右下角缩放柄。触摸设备上它也是唯一安全的拖拽起点：
             整卡拖会跟页面滚动抢手势，只有手柄设了 touch-action:none。 -->
        <div
          class="card-resize"
          role="button"
          tabindex="-1"
          :aria-label="`拖动调整「${card.label}」的大小`"
          @pointerdown="onResizeStart($event, i)"
        />
      </div>
    </div>

    <!-- 4.2 卡片显隐/排序/尺寸配置 -->
    <el-dialog v-model="customOpen" title="自定义仪表盘卡片" width="420px">
      <div class="custom-list">
        <div v-for="(c, i) in draftCards" :key="c.id" class="custom-row">
          <el-checkbox v-model="draftOn[c.id]" :label="CARD_LABELS[c.id]" />
          <div class="custom-btns">
            <!-- 尺寸：小 / 宽 / 大 / 整宽 / 通栏。只对勾选中的卡片有意义，未勾选时置灰 -->
            <el-select
              :model-value="sizeKey(c)"
              size="small"
              class="size-select"
              :disabled="!draftOn[c.id]"
              @update:model-value="(v: string) => setSize(i, v)"
            >
              <el-option
                v-for="s in sizesFor(c.id)"
                :key="s.key"
                :label="s.label"
                :value="s.key"
              />
            </el-select>
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="i === 0"
              aria-label="上移"
              @click="move(i, -1)"
            >
              ↑
            </button>
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="i === draftCards.length - 1"
              aria-label="下移"
              @click="move(i, 1)"
            >
              ↓
            </button>
          </div>
        </div>
      </div>
      <div class="custom-hint">
        取消勾选即隐藏；↑↓ 调整顺序；右侧选择卡片大小 —— 也可以直接在仪表盘上拖
      </div>
      <template #footer>
        <button class="mini-btn" type="button" @click="resetCustom">恢复默认</button>
        <button class="mini-btn" type="button" :disabled="customSaving" @click="saveCustom">
          保存
        </button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onBeforeUnmount, reactive, ref, watch } from "vue";
// 按需引入 echarts：全量引入会让本页 chunk 多出约 700KB
import * as echarts from "echarts/core";
import { LineChart } from "echarts/charts";
import { GridComponent, LegendComponent, TooltipComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import http from "../api/http";
import { useAuthStore } from "../stores/auth";
import {
  useDashboardStore,
  DASH_CARDS,
  CARD_LABELS,
  CARD_SIZES,
  ROW_H,
  GAP,
  MAX_CARD_W,
  maxH,
  sizesFor,
  defaultSize,
  type CardConfig,
  type DashCard,
} from "../stores/dashboard";

echarts.use([
  LineChart,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
]);

interface Snapshot {
  cpu: number;
  mem_used: number;
  mem_total: number;
  disk_used: number;
  disk_total: number;
  net_in_per_sec: number;
  net_out_per_sec: number;
  load1: number;
  load5: number;
  load15: number;
  cpu_cores: number;
  uptime: number;
  swap_used: number;
  swap_total: number;
  procs: number;
  disk_read_per_sec: number;
  disk_write_per_sec: number;
  disk_partitions: number;
  disk_worst_mount: string;
  disk_worst_pct: number;
  arch: string;
  distro: string;
  kernel: string;
}
interface MetricPoint {
  ts: number;
  cpu: number;
  mem_used: number;
  net_in: number;
  net_out: number;
}

const auth = useAuthStore();
const dash = useDashboardStore();

const snap = reactive<Snapshot>({
  cpu: 0,
  mem_used: 0,
  mem_total: 1,
  disk_used: 0,
  disk_total: 1,
  net_in_per_sec: 0,
  net_out_per_sec: 0,
  load1: 0,
  load5: 0,
  load15: 0,
  cpu_cores: 1,
  uptime: 0,
  swap_used: 0,
  swap_total: 0,
  procs: 0,
  disk_read_per_sec: 0,
  disk_write_per_sec: 0,
  disk_partitions: 0,
  disk_worst_mount: "",
  disk_worst_pct: 0,
  arch: "",
  distro: "",
  kernel: "",
});
const history = ref<MetricPoint[]>([]);
const chartEl = ref<HTMLElement>();
let chart: ReturnType<typeof echarts.init> | null = null;
let timer: number | undefined;

// 趋势图时间窗：后端按 2 秒采样一条，点数 = 秒数/2，全部落在后端 limit 上限内。
// label 是按钮上的短标签（四个按钮并排在卡片头部，写「10 分钟」就挤不下），
// full 是标题里的完整读数。两者分开，标题与按钮各说各需要的那一版。
const RANGES = [
  { key: "10m", label: "10分", full: "10 分钟", secs: 600, points: 120 },
  { key: "30m", label: "30分", full: "30 分钟", secs: 1800, points: 360 },
  { key: "1h", label: "1小时", full: "1 小时", secs: 3600, points: 720 },
  { key: "2h", label: "2小时", full: "2 小时", secs: 7200, points: 1440 },
] as const;
type RangeKey = (typeof RANGES)[number]["key"];
const rangeKey = ref<RangeKey>("10m");
const rangeLabel = computed(
  () => RANGES.find((r) => r.key === rangeKey.value)?.full ?? "10 分钟",
);
function setRange(k: RangeKey) {
  rangeKey.value = k;
  refreshHistory().catch(() => {});
}

function pct(a: number, b: number) {
  return b > 0 ? Math.round((a / b) * 100) : 0;
}
function fmtBytes(n: number) {
  const units = ["B", "K", "M", "G", "T"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)}${units[i]}`;
}

function fmtUptime(sec: number) {
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (d > 0) return `${d}天${h}时`;
  if (h > 0) return `${h}时${m}分`;
  return `${m}分`;
}

// ---------- 4.2 卡片渲染 ----------

interface CardView {
  id: DashCard;
  label: string;
  value: string;
  /// 进度条占比 0–100；`null` = 该指标没有上限，不画进度条
  bar: number | null;
  /// 栅格跨度，来自用户配置
  w: number;
  h: number;
  /// 趋势图卡片：渲染 ECharts 容器而不是数值
  isChart?: boolean;
}

/// 展示顺序与尺寸都来自配置；未定制时按默认顺序与各自的默认尺寸。
/// 内容始终取实时快照。
const visibleCards = computed<CardView[]>(() => {
  const cfg: CardConfig[] =
    dash.cards ?? DASH_CARDS.map((id) => ({ id, ...defaultSize(id) }));
  return cfg.map(({ id, w, h }) => {
    const base = { id, label: CARD_LABELS[id], w, h };
    switch (id) {
      case "cpu":
        return { ...base, value: `${snap.cpu.toFixed(1)}%`, bar: snap.cpu };
      case "mem":
        return {
          ...base,
          value: fmtBytes(snap.mem_used),
          bar: pct(snap.mem_used, snap.mem_total),
        };
      case "disk":
        return {
          ...base,
          value: `${pct(snap.disk_used, snap.disk_total)}%`,
          bar: pct(snap.disk_used, snap.disk_total),
        };
      case "net":
        return {
          ...base,
          value: `↓${fmtBytes(snap.net_in_per_sec)}/s ↑${fmtBytes(snap.net_out_per_sec)}/s`,
          // 速率没有上限，进度条无从表达
          bar: null,
        };
      case "load":
        return {
          ...base,
          value: `${snap.load1.toFixed(2)} / ${snap.load5.toFixed(2)} / ${snap.load15.toFixed(2)}`,
          bar: pct(snap.load1, snap.cpu_cores),
        };
      case "uptime":
        return { ...base, value: fmtUptime(snap.uptime), bar: null };
      case "swap":
        return {
          ...base,
          value:
            snap.swap_total > 0
              ? `${fmtBytes(snap.swap_used)} / ${fmtBytes(snap.swap_total)}`
              : "未启用",
          // 未启用交换区时没有「占比」可言，不该画一条空槽
          bar: snap.swap_total > 0 ? pct(snap.swap_used, snap.swap_total) : null,
        };
      case "procs":
        return { ...base, value: String(snap.procs), bar: null };
      case "diskio":
        return {
          ...base,
          value: `读 ${fmtBytes(snap.disk_read_per_sec)}/s 写 ${fmtBytes(snap.disk_write_per_sec)}/s`,
          // 同网络：速率无上限
          bar: null,
        };
      case "partitions":
        return {
          ...base,
          // 进度条走最紧张分区的占用率，而不是聚合值
          value: snap.disk_partitions
            ? `${snap.disk_partitions} 个 · ${snap.disk_worst_mount} ${Math.round(snap.disk_worst_pct)}%`
            : "无分区",
          bar: snap.disk_worst_pct,
        };
      case "cores":
        return { ...base, value: `${snap.cpu_cores} 核 · ${snap.arch}`, bar: null };
      case "sysinfo":
        return {
          ...base,
          // 内核串形如 `6.6.66-microsoft-standard-WSL2`，卡片放不下，
          // 只取破折号前的主版本号 —— 看内核主要就看这三个数字
          value: `${snap.distro || "未知"} · ${snap.kernel.split("-")[0] || "—"}`,
          bar: null,
        };
      case "chart":
        // 图表卡没有数值与进度条，模板会走另一个分支渲染 ECharts 容器
        return { ...base, value: "", bar: null, isChart: true };
    }
  });
});

// ---------- 4.2 自定义弹窗 ----------

const customOpen = ref(false);
const customSaving = ref(false);
/// 弹窗编辑态：全部卡片（含隐藏的）+ 勾选状态。
/// 列表含未勾选项，便于重新勾上时知道插回哪个位置、用什么尺寸。
const draftCards = ref<CardConfig[]>([]);
const draftOn = reactive<Record<string, boolean>>({});

function openCustom() {
  const cfg: CardConfig[] =
    dash.cards ?? DASH_CARDS.map((id) => ({ id, ...defaultSize(id) }));
  const configured = new Set(cfg.map((c) => c.id));
  // 编辑态按"全部卡片"列出：已配置的在前（保持其顺序与尺寸，故要浅拷贝），
  // 未配置的追加在后并用各自默认尺寸
  const rest: CardConfig[] = DASH_CARDS.filter((id) => !configured.has(id)).map(
    (id) => ({ id, ...defaultSize(id) }),
  );
  draftCards.value = [...cfg.map((c) => ({ ...c })), ...rest];
  for (const id of DASH_CARDS) draftOn[id] = configured.has(id);
  customOpen.value = true;
}

/// 当前尺寸对应的档位 key；不在档位里的组合按「小」回显
function sizeKey(c: CardConfig): string {
  const hit = CARD_SIZES.find((s) => s.w === c.w && s.h === c.h);
  return hit ? hit.key : CARD_SIZES[0].key;
}

function setSize(i: number, key: string) {
  const size = CARD_SIZES.find((s) => s.key === key);
  if (!size) return;
  // 就地改属性即可：draftCards 是 ref 包着的数组，元素本身是响应式的
  draftCards.value[i].w = size.w;
  draftCards.value[i].h = size.h;
}

function move(i: number, dir: -1 | 1) {
  const j = i + dir;
  if (j < 0 || j >= draftCards.value.length) return;
  const arr = [...draftCards.value];
  [arr[i], arr[j]] = [arr[j], arr[i]];
  draftCards.value = arr;
}

async function saveCustom() {
  const picked = draftCards.value.filter((c) => draftOn[c.id]);
  if (!picked.length) {
    ElMessage.warning("至少保留一张卡片");
    return;
  }
  customSaving.value = true;
  try {
    await dash.save(picked);
    ElMessage.success("已保存");
    customOpen.value = false;
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "保存失败");
  } finally {
    customSaving.value = false;
  }
}

async function resetCustom() {
  customSaving.value = true;
  try {
    await dash.save(null);
    ElMessage.success("已恢复默认");
    customOpen.value = false;
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "恢复失败");
  } finally {
    customSaving.value = false;
  }
}

// ---------- 拖拽重排与缩放（鼠标直接操作） ----------

const gridEl = ref<HTMLElement>();

/// 拖拽态：正在拖的卡片 + 相对起点的位移（只做视觉跟随，不改布局）
const drag = reactive({ id: null as DashCard | null, from: -1, dx: 0, dy: 0 });
/// 指针悬停到的卡片索引；-1 表示没落在任何卡片上
const dropIndex = ref(-1);
/// 缩放态：w/h 是起点值，curW/curH 是拖动过程中的实时预览值
const resize = reactive({
  id: null as DashCard | null,
  x: 0,
  y: 0,
  w: 1,
  h: 1,
  curW: 1,
  curH: 1,
});
let pointerStart = { x: 0, y: 0 };

function clamp(v: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, v));
}

/// 栅格列数断点：≤480 单列、≤768 两列、其余四列（宽屏与 CSS 里的默认 4 列一致）。
/// 早先是「数 gridTemplateColumns 的已用轨道」来推列数，但那恰好数不出这里的毛病 ——
/// 有卡片跨度超过列数时，栅格会为超出的跨度凭空多出隐式轨道，数出来正好是被撑大的
/// 那个数，于是「列数」看着永远是对的。改成由这里算出来、再用内联
/// grid-template-columns 下发，CSS 与脚本不会各说一套。
const NARROW_W = 480;
const MEDIUM_W = 768;
function colsFor(width: number): number {
  if (width <= NARROW_W) return 1;
  if (width <= MEDIUM_W) return 2;
  return 4;
}
const cols = ref(colsFor(window.innerWidth));
function syncCols() {
  cols.value = colsFor(window.innerWidth);
}

/// 单元格宽度（含一个间距）：把指针的像素位移换算成列数
function colWidth(): number {
  if (!gridEl.value) return 0;
  const n = cols.value;
  return (gridEl.value.clientWidth - GAP * (n - 1)) / n;
}

/// 渲染用的跨度：正在缩放的那张卡走实时预览值。
/// 必须钳到当前列数 —— 配置里的跨度是按 4 列存的（趋势图默认 4×2），窄屏只有
/// 1–2 列时 `grid-column: span 4` 会撑出隐式轨道，整个栅格宽过屏幕、左侧卡片被裁成
/// 竖条（手机上的仪表盘就是这么坏的）。
function cellW(c: CardView) {
  const w = resize.id === c.id ? resize.curW : c.w;
  return Math.min(w, cols.value);
}
function cellH(c: CardView) {
  return resize.id === c.id ? resize.curH : c.h;
}

/// 当前生效的配置（未定制时按默认顺序 + 各自默认尺寸现生成一份）
function currentConfig(): CardConfig[] {
  return dash.cards
    ? dash.cards.map((c) => ({ ...c }))
    : DASH_CARDS.map((id) => ({ id, ...defaultSize(id) }));
}

async function saveLayout(cfg: CardConfig[]) {
  try {
    await dash.save(cfg);
    // 尺寸或顺序变了，图表容器也跟着变，等 DOM 更新后重算画布
    await nextTick();
    chart?.resize();
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "保存布局失败");
  }
}

/// 卡片里的可交互控件：趋势图头部那一排时间窗按钮。
/// 指针按在这些元素上时整卡拖拽必须让路 —— 拖拽要 preventDefault 掉
/// pointerdown（否则会顺手选中文字、拖出图片ghost），而 pointerdown 的
/// preventDefault 会连带取消后续的 click：按钮从此点不动，一按就变成拖卡片。
const INTERACTIVE = "button, a, input, select, textarea, [role='button']";

function onCardPointerDown(e: PointerEvent, i: number) {
  // 触摸设备上整卡拖拽会跟页面滚动抢手势，那里只认右下角手柄
  if (e.pointerType !== "mouse") return;
  if ((e.target as HTMLElement | null)?.closest(INTERACTIVE)) return;
  e.preventDefault();
  drag.id = visibleCards.value[i].id;
  drag.from = i;
  drag.dx = 0;
  drag.dy = 0;
  dropIndex.value = i;
  pointerStart = { x: e.clientX, y: e.clientY };
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp, { once: true });
}

function onResizeStart(e: PointerEvent, i: number) {
  e.preventDefault();
  e.stopPropagation();
  const c = visibleCards.value[i];
  resize.id = c.id;
  resize.x = e.clientX;
  resize.y = e.clientY;
  resize.w = c.w;
  resize.h = c.h;
  resize.curW = c.w;
  resize.curH = c.h;
  pointerStart = { x: e.clientX, y: e.clientY };
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp, { once: true });
}

function onPointerMove(e: PointerEvent) {
  if (drag.id !== null) {
    drag.dx = e.clientX - pointerStart.x;
    drag.dy = e.clientY - pointerStart.y;
    // 被拖的卡片设了 pointer-events:none，这里的命中会穿到它下面的卡片
    const under = document.elementFromPoint(e.clientX, e.clientY);
    const cardEl = under?.closest<HTMLElement>(".card");
    const idx = cardEl?.dataset.index;
    if (idx !== undefined) dropIndex.value = Number(idx);
    return;
  }
  if (resize.id !== null) {
    const dw = Math.round((e.clientX - resize.x) / (colWidth() + GAP));
    const dh = Math.round((e.clientY - resize.y) / (ROW_H + GAP));
    // 上限与后端校验同源：宽度是栅格列数，高度按卡片类型给
    // （趋势图 6 行、其余 3 行，见 maxH）；窄屏列数更少，宽度上限跟着列数收
    resize.curW = clamp(resize.w + dw, 1, Math.min(MAX_CARD_W, cols.value));
    resize.curH = clamp(resize.h + dh, 1, maxH(resize.id));
  }
}

async function onPointerUp() {
  window.removeEventListener("pointermove", onPointerMove);

  // 拖拽落位：把卡片从原索引搬到落点索引；没挪动就什么都不做
  if (
    drag.id !== null &&
    drag.from >= 0 &&
    dropIndex.value >= 0 &&
    dropIndex.value !== drag.from
  ) {
    const from = drag.from;
    const to = dropIndex.value;
    drag.id = null;
    dropIndex.value = -1;
    const cfg = currentConfig();
    const [moved] = cfg.splice(from, 1);
    cfg.splice(to, 0, moved);
    await saveLayout(cfg);
    return;
  }

  // 缩放落位：尺寸没变就不必打扰服务端
  if (resize.id !== null) {
    const id = resize.id;
    const nextW = resize.curW;
    const nextH = resize.curH;
    const changed = nextW !== resize.w || nextH !== resize.h;
    resize.id = null;
    if (changed) {
      const cfg = currentConfig();
      const item = cfg.find((c) => c.id === id);
      if (item) {
        item.w = nextW;
        item.h = nextH;
        await saveLayout(cfg);
      }
    }
  }

  drag.id = null;
  dropIndex.value = -1;
}

// ---------- 数据刷新 ----------

async function refresh() {
  const { data } = await http.get("/system/state");
  Object.assign(snap, data);
}
async function refreshHistory() {
  const r = RANGES.find((x) => x.key === rangeKey.value) ?? RANGES[0];
  const now = Math.floor(Date.now() / 1000);
  const { data } = await http.get("/system/history", {
    params: { from: now - r.secs, to: now, limit: r.points },
  });
  history.value = data;
  renderChart();
}

function renderChart() {
  if (!chart || !chartEl.value) return;
  // 颜色从 CSS 变量运行时读取：主题定制（改主色/文本色/背景）后图表跟随，
  // 不再硬编码默认主题的灰阶值。canvas 不受 CSS 级联影响，只能这样取值。
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  const line = v("--el-text-color-primary") || "#111111";
  const sub = v("--el-text-color-secondary") || "#777777";
  const grid = v("--el-fill-color-dark") || "#ebebeb";
  const cardBg = v("--el-bg-color") || "#ffffff";
  const cpuData = history.value.map((p) => Number(p.cpu.toFixed(2)));
  const memData = history.value.map((p) =>
    snap.mem_total > 0 ? Number(((p.mem_used / snap.mem_total) * 100).toFixed(1)) : 0,
  );
  // 两条线量级差得远（CPU 常年个位数、内存几十个百分点），共用一根写死 0–100 的轴时
  // CPU 只能贴着底边走直线 —— 这就是「趋势看不出变化」的主因。各给一根轴、上限按各自
  // 数据自适应（最大值的 1.2 倍），并留一个下限，免得空数据时轴塌成一条线。
  const axisMax = (vals: number[], floor: number) =>
    vals.length ? Math.max(floor, Math.ceil(Math.max(...vals) * 1.2 * 10) / 10) : floor;

  chart.setOption({
    backgroundColor: "transparent",
    // 统一调色板为灰阶（tooltip 标记等默认色也走这里）
    color: [line, sub],
    tooltip: {
      trigger: "axis",
      backgroundColor: v("--el-bg-color-overlay") || cardBg,
      // ECharts 6 的 borderWidth 默认值是 1（见 TooltipModel.js），配上 borderColor
      // 就是一圈可见描边，与全站「不画可见描边」的约定冲突 —— 显式归零。
      borderWidth: 0,
      textStyle: { color: line },
      // 用全站统一的浮层档投影，替代 ECharts 自带的 shadowBlur / shadowColor
      // （默认 10px 与 20% 黑），否则会和 --panel-shadow-2 叠成两层影。
      extraCssText: "box-shadow: var(--panel-shadow-2);",
      shadowBlur: 0,
    },
    // ECharts 6 起 legend 的默认位置由顶部改成了贴底（LegendModel.defaultOption
    // 里 top 被注释、改设 bottom），于是图例会压在 x 轴标签上。这里显式钉回顶部，
    // 正好落在 grid.top 预留的空间里；bottom 保留默认值不影响 top 的解析。
    legend: { data: ["CPU %", "内存 %"], textStyle: { color: sub }, top: 0 },
    // 右侧留出第二根轴的刻度（原 20 放不下）
    grid: { left: 40, right: 36, top: 40, bottom: 30 },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: history.value.map((p) =>
        new Date(p.ts * 1000).toLocaleTimeString("zh-CN", { hour12: false }),
      ),
      axisLine: { lineStyle: { color: grid } },
      axisLabel: { color: sub },
    },
    // 双轴：左 CPU、右内存。轴刻度色与对应折线一致（CPU=正文色、内存=次要色），
    // 不加轴名也能看出哪条线读哪根轴；网格线只留一根，免得两套刻度叠成密网。
    yAxis: [
      {
        type: "value",
        min: 0,
        max: axisMax(cpuData, 1),
        splitLine: { lineStyle: { color: grid } },
        axisLabel: { color: line, fontSize: 10 },
      },
      {
        type: "value",
        min: 0,
        max: axisMax(memData, 10),
        splitLine: { show: false },
        axisLabel: { color: sub, fontSize: 10 },
      },
    ],
    series: [
      {
        name: "CPU %",
        type: "line",
        yAxisIndex: 0,
        smooth: true,
        showSymbol: false,
        data: cpuData,
        itemStyle: { color: line },
        lineStyle: { color: line, width: 2 },
        areaStyle: { color: line, opacity: 0.08 },
      },
      {
        name: "内存 %",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
        data: memData,
        itemStyle: { color: sub },
        lineStyle: { color: sub, width: 2, type: "dashed" },
      },
    ],
  });
}

function onResize() {
  // 跨过断点时列数会变（4 ↔ 2 ↔ 1），跨度钳制与卡片宽度都要跟着重算
  syncCols();
  chart?.resize();
}

/// 图表容器长在卡片网格里，只有该卡片可见时才存在 ——
/// 所以初始化/销毁必须跟着显隐走，不能只在 onMounted 里做一次。
function initChart() {
  if (chart) return;
  const el = document.getElementById("dash-chart");
  if (!el) return;
  chartEl.value = el;
  chart = echarts.init(el);
  renderChart();
}

function destroyChart() {
  chart?.dispose();
  chart = null;
  chartEl.value = undefined;
}

onMounted(async () => {
  await Promise.all([refresh(), refreshHistory(), dash.load()]);
  // 等卡片按配置渲染出来，图表容器才存在
  await nextTick();
  syncCols();
  initChart();
  // 卡片与趋势图同频，都是 2 秒 —— 后端采样同样是 2 秒一条，图表跟着它走即可。
  // 早先采样 5 秒 + 图表降到 15 秒拉一次，叠加落库延迟后最新点能滞后 20 秒，
  // 看上去就是「几十秒才动一下」。120 个点的历史请求开销可以忽略，不值得省。
  timer = window.setInterval(() => {
    // 401 时拦截器会跳登录，这里吞掉 rejection 避免轮询抛出未处理错误
    refresh().catch(() => {});
    refreshHistory().catch(() => {});
  }, 2000);
  window.addEventListener("resize", onResize);
});

// 卡片显隐会换掉 DOM：趋势图卡从无到有时建实例，从有到无时销毁
watch(
  () => visibleCards.value.some((c) => c.isChart),
  async (has) => {
    await nextTick();
    if (has) initChart();
    else destroyChart();
  },
);

onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
  window.removeEventListener("resize", onResize);
  // 组件卸载时指针可能仍在拖拽中，把全局监听摘干净
  window.removeEventListener("pointermove", onPointerMove);
  destroyChart();
});
</script>

<style scoped>
.dash {
  display: flex;
  flex-direction: column;
  gap: var(--sp-4);
}
.cards {
  display: grid;
  /* 实际列数由脚本按屏宽算好、用内联 grid-template-columns 下发（窄屏 1–2 列），
     这里只是没有脚本时的兜底值。旧的窄屏媒体查询已删除 —— 见 cellW 的注释：
     列数和卡片跨度必须出自同一处，否则 span 会大于列数、撑出隐式轨道。 */
  grid-template-columns: repeat(4, 1fr);
  /* 行高固定：卡片可以跨 2 行，拖拽缩放时要靠它把像素位移换算成行数
     （脚本里的 ROW_H 常量必须与这个值一致）。88 = 内距 16×2 + 标签 19
     + 数值 20 + 数值上下外距 8 + 进度条 4 ≈ 83，内距从 12 提到 16 后仍留
     5px 余量，够贴又不会把卡内内容裁掉；行高不必跟着调。 */
  grid-auto-rows: 88px;
  /* 栅格间距是拖拽缩放的换算基准：stores/dashboard.ts 的 GAP 必须与它一致
     （脚本用 GAP 把像素位移换算成格数）。改这里会连带改那个常量，故保持 --sp-3。 */
  gap: var(--sp-3);
}
.card {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: var(--sp-4) var(--sp-5);
  /* 与 .el-card 同档的一层投影：仪表盘卡片是 div 不吃 EP 变量，
     这里显式给一次，全站「卡片浮在底色上」的语言才是同一套。
     拖拽时 .card--dragging 会把透明度压到 0.7，投影跟着一起淡，
     「被拎起来」的读法反而更清楚。 */
  box-shadow: var(--panel-card-shadow);
}
/* grid 子项默认 min-width:auto，会被 nowrap 的长值（读 2.1M/s 写 480K/s）
   顶开列宽、撑破栅格。置 0 后 1fr 才能正常收缩，超出部分交给省略号。 */
.card {
  position: relative;
  min-width: 0;
  /* 纵向撑满：进度条靠 margin-top:auto 推到底，小卡与大卡的下沿因此对齐 */
  display: flex;
  flex-direction: column;
  /* 整卡可拖：抓手光标给出「这东西能拿起来」的暗示 */
  cursor: grab;
}
/* 拖拽中：抬起层级、交出命中（好让 elementFromPoint 穿到下层卡片），
   内描边 + 降透明度表达「被拎起来了」。位移来自内联 transform。 */
.card--dragging {
  z-index: 10;
  pointer-events: none;
  cursor: grabbing;
  /* 压到 0.7：被拖的卡片会正好盖住落点卡片，不透就看不见「松手放哪儿」 */
  opacity: 0.7;
  outline: 2px solid var(--el-color-primary);
  outline-offset: -2px;
}
/* 落点提示：虚线描边 + 浅底。浅底是给「被上层卡片盖住」准备的 ——
   拖拽卡 70% 不透明，底下这点色差正好能透出来。 */
.card--drop {
  outline: 2px dashed var(--el-text-color-placeholder);
  outline-offset: -2px;
  background: var(--el-fill-color-light);
}
/* 右下角缩放柄：平时隐形，悬停卡片才现形，不干扰读数 */
.card-resize {
  position: absolute;
  right: 0;
  bottom: 0;
  width: 20px;
  height: 20px;
  cursor: nwse-resize;
  /* 只在这里禁用浏览器手势。整卡不禁用 —— 否则手机上没法滚动页面 */
  touch-action: none;
  opacity: 0;
}
.card:hover .card-resize,
.card--dragging .card-resize {
  opacity: 1;
}
/* 用两条直角边框画手柄，比塞一个图标省事，也更容易对齐 */
.card-resize::after {
  content: "";
  position: absolute;
  right: 5px;
  bottom: 5px;
  width: 7px;
  height: 7px;
  border-right: 2px solid var(--el-text-color-placeholder);
  border-bottom: 2px solid var(--el-text-color-placeholder);
}
/* 触屏没有 hover，手柄得常驻，否则无从下手 */
@media (hover: none) {
  .card-resize {
    opacity: 1;
  }
}
/* 渐显也算动效，减少动效模式下直接切换 */
@media (prefers-reduced-motion: no-preference) {
  .card-resize {
    transition: opacity 160ms ease-out;
  }
}
/* 大卡（跨 2 行）：数值放大并下移。
   卡片只有标签/数值/进度条三个元素，撑到 200px 高时必然空出一大截；
   给数值加上 auto 上边距后，它与进度条的 auto 上边距平分富余空间，
   三者就自然分布成「标签顶 / 数值中 / 进度条底」。 */
.card--lg .card-value {
  font-size: 30px;
  margin-top: auto;
}
/* 入场：自下浮入 + 交错（延迟由模板内联的 animation-delay 给出）。
   只动 opacity / transform，不触发布局；12 张约 400ms 走完。 */
@media (prefers-reduced-motion: no-preference) {
  .card {
    animation: card-in 260ms cubic-bezier(0.16, 1, 0.3, 1) backwards;
  }
}
@keyframes card-in {
  from {
    opacity: 0;
    transform: translateY(8px);
  }
}
.card-label {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.card-value {
  font-size: 20px;
  font-weight: 600;
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  margin: var(--sp-1) 0;
  /* 单行 + 省略号：卡片值里既有短值（62%）也有长值（读 2.1M/s 写 480K/s、
     发行版 · 内核），允许换行会让卡片高度参差不齐、三行栅格对不齐 */
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.bar {
  height: 4px;
  /* 无论卡片是 1 行还是 2 行高，进度条都贴底 */
  margin-top: auto;
  border-radius: var(--radius);
  background: var(--el-fill-color);
  overflow: hidden;
}
/* 没有上限的指标（速率 / 计数 / 静态信息）不画槽，只留 4px 占位：
   整块去掉会让同一行卡片的内容高度参差，凭空多出对不齐的留白 */
.bar--none {
  background: transparent;
}
.bar i {
  display: block;
  height: 100%;
  background: var(--el-text-color-primary);
  border-radius: var(--radius);
}
/* 图表卡头部：标题在左、时间窗在右。
 * 两处防重叠：标题要 min-width:0 —— flex 子项默认 min-width:auto，
 * 「负载趋势（10 分钟）」这串在窄卡里会把按钮组顶到卡片外、压在一起；
 * 按钮组 flex:none 不跟着压缩（文字一压就叠字），真放不下时整组换行到第二排，
 * 图表自己会往上让。 */
.chart-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: var(--sp-2);
  flex: none;
}
.chart-head .card-label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* 图表撑满卡片剩余高度。卡片高度随用户配置变化，图表必须跟着长 ——
   写死像素会在「通栏」档位下留一大块空白。ECharts 读容器尺寸定画布，
   flex 布局下拿到的是最终高度，不会有问题。 */
.chart {
  flex: 1;
  min-height: 0;
  margin-top: var(--sp-1);
}
.range-group {
  display: flex;
  flex: none;
  gap: var(--sp-1);
}
/* 选中的时间窗：反色块（无边框设计规范） */
.mini-btn--on {
  background: var(--el-text-color-primary);
  color: var(--el-bg-color);
}
.mini-btn--on:hover {
  background: var(--el-text-color-primary);
  opacity: 0.85;
}

/* 4.2 卡片自定义弹窗 */
.mini-btn {
  border: none;
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
  border-radius: var(--radius);
  padding: var(--sp-1) var(--sp-3);
  font-size: 12px;
  cursor: pointer;
}
.mini-btn:hover {
  background: var(--el-fill-color);
}
.mini-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.mini-btn--sm {
  padding: var(--sp-1) var(--sp-2);
}
.custom-list {
  display: flex;
  flex-direction: column;
  gap: var(--sp-1);
}
.custom-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.custom-btns {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
/* 尺寸下拉：够放下「小 / 宽 / 大 / 整宽 / 通栏」两个字即可 */
.size-select {
  width: 72px;
}
.custom-hint {
  margin-top: var(--sp-3);
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
/* 弹窗底部按钮组 */
:deep(.el-dialog__footer) {
  display: flex;
  gap: var(--sp-2);
  justify-content: flex-end;
}
/* 窄屏不再单独改 grid-auto-rows / gap：行高与间距是拖拽缩放的换算基准
   （stores/dashboard.ts 的 ROW_H / GAP 必须与它们一致），只在媒体查询里改
   会让换算用错格宽。列数由脚本算（cellW 注释），这里没有需要覆盖的。 */
</style>
