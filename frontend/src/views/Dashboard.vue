<template>
  <div class="dash">
    <div class="toolbar">
      <div class="spacer" />
      <!-- 4.2 卡片自定义入口（admin 专属，保存走 RequireRole<2> 接口） -->
      <button v-if="auth.isAdmin()" class="mini-btn" type="button" @click="openCustom">
        自定义卡片
      </button>
    </div>
    <div ref="gridEl" class="cards" :style="{ height: `${canvasHeight}px` }">
      <!-- 自由布局：卡片按**绝对几何**摆放（x/w 是容器宽度的百分数、y/h 是像素）。
           按住卡身 = 两轴自由移动，右下角 = 自由缩放，松手即存服务端。
           没有栅格、没有自动成排 —— 留洞、重叠都归用户自己摆（要整齐就用
           弹窗里的「自动排列」）。 -->
      <div
        v-for="(card, i) in visibleCards"
        :key="card.id"
        class="card"
        :class="{
          'card--dragging': drag.id === card.id,
          'card--drop': dropIndex === i && drag.id !== null && drag.id !== card.id,
        }"
        :data-index="i"
        :style="posStyle(card, i)"
        @pointerdown="onCardPointerDown($event, i)"
      >
        <!-- 趋势图卡：头部放标题与该卡自己的时间窗，图表撑满剩余高度。
             三张卡（负载 / 网络 / 磁盘 I/O）各自一个容器、各自一个时间窗。 -->
        <template v-if="card.isChart">
          <div class="chart-head">
            <span class="card-label">{{ card.label }}（{{ rangeLabelOf(card.id) }}）</span>
            <div class="range-group">
              <button
                v-for="r in RANGES"
                :key="r.key"
                class="mini-btn mini-btn--sm"
                :class="{ 'mini-btn--on': rangeOf(card.id) === r.key }"
                type="button"
                @click="setRange(card.id, r.key)"
              >
                {{ r.label }}
              </button>
            </div>
          </div>
          <div :id="chartDomId(card.id)" class="chart" />
        </template>
        <template v-else-if="card.list">
          <!-- 排行榜卡：标题 + 若干行「名次 / 名称 / 数值」。
               每行是一条独立的读取路径，不加表格（表格在卡片里会带来自己的
               行高与边距，反而不好对齐）。 -->
          <div class="card-label">{{ card.label }}</div>
          <div class="card-body rank-body">
            <div v-if="!card.rows?.length" class="rank-empty">暂无数据</div>
            <div v-for="(row, ri) in card.rows" :key="row.name + ri" class="rank-row">
              <span class="rank-no">{{ ri + 1 }}</span>
              <span class="rank-name" :title="row.name">{{ row.name }}</span>
              <span class="rank-val">{{ row.value }}</span>
            </div>
          </div>
        </template>
        <template v-else>
          <div class="card-label">{{ card.label }}</div>
          <div class="card-body">
            <div class="card-value">{{ card.value }}</div>
            <!-- 次级说明（如系统卡里的 CPU 规格）：比数值弱一档，不抢主读数 -->
            <div v-if="card.hint" class="card-hint">{{ card.hint }}</div>
            <!-- 速率类指标用波形代替进度条（见 .bar--none 的说明） -->
            <div v-if="card.spark" class="spark-box">
              <Sparkline
                :up="card.spark.up"
                :down="card.spark.down"
                :label="card.spark.label"
                :peak="sparkPeakBytes(card.id)"
              />
              <!-- 量程标注：波形按 P90 定标（否则一次尖峰会把正常波动压成平线），
                   所以必须把「画布顶端代表多少」写出来，读数才不会被误判 -->
              <span class="spark-peak">{{ sparkPeaks[card.id] }}</span>
            </div>
            <!-- 进度条只表达「距离上限还有多少」：有上限的指标才画槽，
                 速率/计数/静态信息不画 -->
            <div v-else class="bar" :class="{ 'bar--none': card.bar === null }">
              <i v-if="card.bar !== null" :style="{ width: card.bar + '%' }" />
            </div>
          </div>
        </template>
        <!-- 右下角缩放柄。触摸设备上它也是唯一安全的拖拽起点：
             整卡拖会跟页面滚动抢手势，只有手柄设了 touch-action:none。
             拖出来的跨度下限/上限由 store 的 sizeLimits 按卡片类型给。 -->
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
            <!-- 尺寸下拉已撤：自由布局下尺寸靠**拖**（右下角手柄），
                 下拉给的"几格"在像素/百分数体系里没有意义。
                 想一键整齐就用下面的「自动排列」。 -->
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="i === 0"
              aria-label="上移"
              @click="moveRow(i, -1)"
            >
              ↑
            </button>
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="i === draftCards.length - 1"
              aria-label="下移"
              @click="moveRow(i, 1)"
            >
              ↓
            </button>
          </div>
        </div>
      </div>
      <div class="custom-hint">
        取消勾选即隐藏；↑↓ 调整顺序。卡片直接在仪表盘上拖：按住卡身移动、拖右下角改大小。
        摆乱了就按「自动排列」按类型重排一遍。
      </div>
      <template #footer>
        <button class="mini-btn" type="button" @click="autoArrange">自动排列</button>
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
import Sparkline from "../components/Sparkline.vue";
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
  DEFAULT_ORDER,
  maxH,
  sizesFor,
  defaultSize,
  sizeLabel,
  spanFor,
  autoLayout,
  sizeLimits,
  CHART_KINDS,
  chartKindOf,
  type CardConfig,
  type ChartKind,
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
  /// 迁移 0014 之前的历史行没有磁盘 I/O：后端返回 null。
  /// 前端必须把它当「无数据」而不是 0 —— 0 会被读成「当时磁盘空闲」。
  disk_read: number | null;
  disk_write: number | null;
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
/// 三种趋势分组各存各的历史与时间窗：**一张卡一个 ECharts 实例、一个时间窗**。
/// 早先是「一张卡三条横带 + 一个全局时间窗」——那让「网络看 10 分钟、磁盘看
/// 2 小时」这种读法做不到，也是 CARD_META 里 chartnet / chartdisk 一直没接渲染的
/// 原因（store 与后端白名单早就声明了这两张卡，视图层却没有对应分支）。
const histories = reactive<Record<ChartKind, MetricPoint[]>>({
  load: [],
  network: [],
  disk: [],
});
const rangeKeys = reactive<Record<ChartKind, RangeKey>>({
  load: "10m",
  network: "10m",
  disk: "10m",
});
/// 容器与实例同样按分组存。容器要等卡片渲染出来才存在，
/// 所以这两张表由 initCharts / destroyCharts 随卡片显隐维护。
const chartEls: Partial<Record<ChartKind, HTMLElement>> = {};
const charts: Partial<Record<ChartKind, ReturnType<typeof echarts.init>>> = {};
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
/// 分组 → 容器 id。每张趋势卡各占一个容器，
/// 不能像从前那样三张卡共用 #dash-chart（会互相抢同一个 DOM 节点）。
function chartDomIdOf(kind: ChartKind) {
  return `dash-chart-${kind}`;
}
/// 卡片 → 容器 id（模板绑定用）
function chartDomId(id: DashCard | string) {
  return chartDomIdOf(chartKindOf(id) ?? "load");
}
/// 该卡所属分组当前选中的时间窗（三个分组各记各的）
function rangeOf(id: DashCard | string): RangeKey {
  return rangeKeys[chartKindOf(id) ?? "load"];
}
/// 卡片标题里的完整读数（按钮上只放短标签，理由见 RANGES 的说明）
function rangeLabelOf(id: DashCard | string) {
  return RANGES.find((r) => r.key === rangeOf(id))?.full ?? "10 分钟";
}
function setRange(id: DashCard | string, k: RangeKey) {
  const kind = chartKindOf(id);
  if (!kind) return;
  rangeKeys[kind] = k;
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

/// 字节速率的轴刻度：与卡片的 fmtBytes 同一套单位，但固定带 /s 后缀，
/// 免得把「字节/秒」误读成「累计字节」。
function fmtRateAxis(n: number) {
  return `${fmtBytes(n)}/s`;
}

function fmtUptime(sec: number) {
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  if (d > 0) return `${d}天${h}时`;
  if (h > 0) return `${h}时${m}分`;
  return `${m}分`;
}

/// 交换区读数（「交换 x / y」）——原「交换区」散卡的内容，现已并入「内存」卡
/// 的明细行。未启用交换区时显示「未启用」，而不是会误导的「0 / 0」。
function swapText() {
  return snap.swap_total > 0
    ? `${fmtBytes(snap.swap_used)} / ${fmtBytes(snap.swap_total)}`
    : "未启用";
}

// ---------- 4.2 卡片渲染 ----------

interface CardView {
  id: DashCard;
  label: string;
  value: string;
  /// 数值下方的次级说明（如系统卡里的 CPU 规格）：比主读数弱一档
  hint?: string;
  /// 进度条占比 0–100；`null` = 该指标没有上限，不画进度条
  bar: number | null;
  /// 绝对几何（见 store 的 CardConfig）：x/w 是容器宽度百分数、y/h 是像素
  x?: number;
  y?: number;
  w: number;
  h: number;
  /// 趋势图卡片：渲染 ECharts 容器而不是数值
  isChart?: boolean;
  /// 排行榜卡片：渲染若干行「名次 / 名称 / 数值」而不是单个数值
  list?: boolean;
  rows?: RankRow[];
  /// 实时波形卡片：在数值下方渲染镜像面积图
  spark?: SparkData;
}

/// 排行榜（CPU / 内存 Top 5 进程、磁盘占用 Top 5 目录）共用的一行
interface RankRow {
  /// 进程名或目录路径（目录只显示末段，完整路径进 title）
  name: string;
  /// 已格式化好的数值（CPU% / 内存大小 / 目录体积）
  value: string;
}

/// 实时波形：卡片上的镜像面积图（上半=入/读，下半=出/写）
interface SparkData {
  up: (number | null)[];
  down: (number | null)[];
  label: string;
}

/// 波形保留的采样点数。主轮询 2 秒一次 → 60 点 ≈ 2 分钟。
/// 只存内存、刷新即重新累积：这是「实时」读数，历史趋势由折线图负责，
/// 两者职责不重叠，也就不需要为它落库。
const SPARK_POINTS = 60;

/// Top 5 排行榜取几行
const TOP_N = 5;

/// 未定制时的默认卡片配置：顺序取自 store 的 DEFAULT_ORDER
/// （按类型分组，同高元素成排、4 列栅格零空洞），尺寸用各卡的默认尺寸。
/// 未定制时的默认布局：按类型自动排列一份绝对几何（见 store 的 autoLayout）。
/// 规则与从前的栅格一致，所以从栅格搬过来看起来没变。
function defaultConfig(): CardConfig[] {
  return autoLayout(DEFAULT_ORDER);
}

/// 展示顺序与尺寸都来自配置；未定制时按默认顺序与各自的默认尺寸。
/// 内容始终取实时快照。
const visibleCards = computed<CardView[]>(() => {
  const cfg: CardConfig[] = dash.cards ?? defaultConfig();
  return cfg.map(({ id, x, y, w, h }): CardView | null => {
    const base = { id, label: CARD_LABELS[id], x, y, w, h };
    switch (id) {
      case "cpu":
        // 主读数 = 使用率；明细补「核数 · 架构」（原「CPU 规格」散卡的内容）
        return {
          ...base,
          value: `${snap.cpu.toFixed(1)}%`,
          hint: `${snap.cpu_cores} 核 · ${snap.arch}`,
          bar: snap.cpu,
        };
      case "mem":
        // 主读数改成百分比（先给「用了多少」）；原「内存」卡的「已用 / 总量」
        // 下沉为明细，再接原「交换区」散卡的内容（交换 x / y）
        return {
          ...base,
          value: `${pct(snap.mem_used, snap.mem_total)}%`,
          hint: `${fmtBytes(snap.mem_used)} / ${fmtBytes(snap.mem_total)} · 交换 ${swapText()}`,
          bar: pct(snap.mem_used, snap.mem_total),
        };
      case "disk":
        // 明细补「最紧张分区 + 已用 / 总量」（原「分区」散卡的内容）；
        // 分区计数放末尾 —— hint 允许省略号截断，先牺牲它，别牺牲用量数字。
        return {
          ...base,
          value: `${pct(snap.disk_used, snap.disk_total)}%`,
          hint: `${snap.disk_worst_mount} ${fmtBytes(snap.disk_used)} / ${fmtBytes(snap.disk_total)} · ${snap.disk_partitions} 个分区`,
          bar: pct(snap.disk_used, snap.disk_total),
        };
      case "net":
        return {
          ...base,
          value: `↓${fmtBytes(snap.net_in_per_sec)}/s ↑${fmtBytes(snap.net_out_per_sec)}/s`,
          // 速率没有上限，进度条无从表达；改用波形表达「最近两分钟怎么走的」
          bar: null,
          spark: {
            up: netInTrace.value,
            down: netOutTrace.value,
            label: "网络实时波形（上半入、下半出）",
          },
        };
      case "load":
        return {
          ...base,
          value: `${snap.load1.toFixed(2)} / ${snap.load5.toFixed(2)} / ${snap.load15.toFixed(2)}`,
          bar: pct(snap.load1, snap.cpu_cores),
        };
      case "uptime": {
        // 原「运行时长」散卡：已并入「系统」卡的明细行（… · x天x时 · …）。
        // 保留原读数便于回溯（原卡无上限，bar 为 null）；不再渲染。
        const value = fmtUptime(snap.uptime);
        void value;
        return null;
      }
      case "swap": {
        // 原「交换区」散卡：读数已并入「内存」卡的明细行（… · 交换 x / y）。
        // 保留原读数与占比计算便于回溯；不再渲染。
        const value = swapText();
        const bar = snap.swap_total > 0 ? pct(snap.swap_used, snap.swap_total) : null;
        void value;
        void bar;
        return null;
      }
      case "procs": {
        // 原「进程」散卡：进程数已并入「系统」卡的明细行（… · N 进程）。
        // 保留原读数便于回溯（原卡无上限，bar 为 null）；不再渲染。
        const value = String(snap.procs);
        void value;
        return null;
      }
      case "diskio":
        return {
          ...base,
          value: `读 ${fmtBytes(snap.disk_read_per_sec)}/s 写 ${fmtBytes(snap.disk_write_per_sec)}/s`,
          // 同网络：速率无上限，用波形代替进度条
          bar: null,
          spark: {
            up: diskReadTrace.value,
            down: diskWriteTrace.value,
            label: "磁盘 I/O 实时波形（上半读、下半写）",
          },
        };
      case "partitions": {
        // 原「分区」散卡：最紧张分区已并入「磁盘」卡的明细行。
        // 保留原读数与占比计算便于回溯；不再渲染。
        const value = snap.disk_partitions
          ? `${snap.disk_partitions} 个 · ${snap.disk_worst_mount} ${Math.round(snap.disk_worst_pct)}%`
          : "无分区";
        const bar = snap.disk_worst_pct;
        void value;
        void bar;
        return null;
      }
      case "cores":
        // 「CPU 规格」已并入「CPU」卡的明细行（核数 + 架构，见 cpu 分支）。
        // 合并的理由：这张是**进程生命周期内不变的机器信息**，
        // 各占一格却只有一行字，而实时指标（网络 / 磁盘趋势）更需要空间。
        // 这里返回 null 由下面过滤掉 —— 老配置里可能还存着 cores，
        // 必须「不渲染」而不是留下一个空洞。
        return null;
      case "sysinfo":
        // 主读数只留发行版；内核主版本（内核串形如
        // `6.6.66-microsoft-standard-WSL2`，卡片放不下，只取破折号前的数字）、
        // 运行时长、进程数一起下沉为明细 —— 后两项来自原「运行时长」「进程」
        // 两张散卡。
        return {
          ...base,
          value: snap.distro || "未知",
          hint: `${snap.kernel.split("-")[0] || "—"} · ${fmtUptime(snap.uptime)} · ${snap.procs} 进程`,
          bar: null,
        };
      // 三张趋势图卡：**一张卡只画一组同量纲的指标**（负载 % / 网络速率 / 磁盘速率）。
      // 早先是一张卡三条横带，靠「各带各自定标」绕开量纲问题；拆成三张之后每组独占
      // 一张卡，纵轴与时间窗都互不干扰 —— store 与后端白名单本来就是按三张声明的
      // （chart / chartnet / chartdisk），这里补齐视图层分支。
      case "chart":
      case "chartnet":
      case "chartdisk":
        // 图表卡没有数值与进度条，模板会走另一个分支渲染 ECharts 容器
        return { ...base, value: "", bar: null, isChart: true };
      case "topcpu":
        return {
          ...base,
          value: "",
          bar: null,
          list: true,
          rows: topRows(procs.value, (p) => p.cpu, (p) => `${p.cpu.toFixed(1)}%`),
        };
      case "topmem":
        return {
          ...base,
          value: "",
          bar: null,
          list: true,
          rows: topRows(procs.value, (p) => p.mem, (p) => fmtBytes(p.mem)),
        };
      case "topdisk": {
        // 目录名只显示末段（`/var` → `var`）：卡片一行放不下整条路径，
        // 完整路径留给 title 悬浮查看
        const rows: RankRow[] = (dirUsage.value?.children ?? [])
          .slice(0, TOP_N)
          .map((c) => ({
            name: c.path.split("/").filter(Boolean).pop() ?? c.path,
            value: fmtBytes(c.bytes),
          }));
        return { ...base, value: "", bar: null, list: true, rows };
      }
      default:
        // 未在本 switch 里处理的卡片不渲染（如已并入系统卡的 cores）。
        // 返回 null 而不是 undefined：下面用类型谓词过滤，TS 之后看到的是非空元素。
        return null;
    }
  })
    // 过滤掉合并/停用的卡片（cores），它们不再占据布局位置
    .filter((c): c is CardView => c !== null);
});

/// 从进程表里取 Top N。两个排行榜共用这一处排序逻辑：
/// 一次请求取回全表，本地按 CPU / 内存各排一次 —— 比让后端按两种排序
/// 各查一遍省一次全系统进程扫描（refresh_processes 是整表 /proc 扫描）。
function topRows(
  list: ProcRow[],
  by: (p: ProcRow) => number,
  fmt: (p: ProcRow) => string,
): RankRow[] {
  return [...list]
    .sort((a, b) => by(b) - by(a))
    .slice(0, TOP_N)
    .map((p) => ({ name: p.name, value: fmt(p) }));
}

// ---------- 4.2 自定义弹窗 ----------

const customOpen = ref(false);
const customSaving = ref(false);
/// 弹窗编辑态：全部卡片（含隐藏的）+ 勾选状态。
/// 列表含未勾选项，便于重新勾上时知道插回哪个位置、用什么尺寸。
const draftCards = ref<CardConfig[]>([]);
const draftOn = reactive<Record<string, boolean>>({});

function openCustom() {
  const cfg: CardConfig[] = dash.cards ?? defaultConfig();
  const configured = new Set(cfg.map((c) => c.id));
  // 编辑态按"全部卡片"列出：已配置的在前（保持其顺序与尺寸，故要浅拷贝），
  // 未配置的追加在后并用各自默认尺寸
  const rest: CardConfig[] = DEFAULT_ORDER.filter((id) => !configured.has(id)).map(
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

/// 「自动排列」：把当前勾选中的卡片按类型重排一遍并立刻保存。
/// 自由布局下不会自动成排，摆乱了靠它一键归位（数值卡 1 列、排行榜 3 行、趋势图整宽）。
async function autoArrange() {
  const ids = draftCards.value.filter((c) => draftOn[c.id]).map((c) => c.id);
  await saveLayout(autoLayout(ids));
  customOpen.value = false;
}

function moveRow(i: number, dir: -1 | 1) {
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
/// 缩放中的那张卡：起手指针位置 + 起手格数 + 实时格数。
/// curW/curH 只影响渲染（cellW/cellH 读它们），松手才写进配置。
/// 移动中的那张卡：起手指针 x0/y0 + 起手几何 fromX（%）/fromY（px）+ 实时 curX/curY
const move = reactive({
  id: null as DashCard | null,
  x0: 0,
  y0: 0,
  fromX: 0,
  fromY: 0,
  curX: 0,
  curY: 0,
});
const resize = reactive({
  id: null as DashCard | null,
  x: 0,
  y: 0,
  w: 1,
  h: 1,
  curW: 1,
  curH: 1,
});
/// 指针按下时的起点坐标：拖拽位移的换算基准
let pointerStart = { x: 0, y: 0 };
/// 指针悬停到的卡片索引；-1 表示没落在任何卡片上
const dropIndex = ref(-1);
/// 刻度换算助手（列数/格宽计算用）
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

/// 卡片的实时几何：拖动/缩放中走预览值，松手才落库。
/// x/w 的单位是**容器宽度的百分数**、y/h 是**像素**（见 CardConfig 的说明）。
function geomOf(c: CardView) {
  const sizing = resize.id === c.id;
  return {
    x: move.id === c.id ? move.curX : (c.x ?? 0),
    y: move.id === c.id ? move.curY : (c.y ?? 0),
    w: sizing ? resize.curW : (c.w ?? 100),
    h: sizing ? resize.curH : (c.h ?? 88),
  };
}

/// 卡片的行内样式：**绝对定位 + 自由几何**。
/// `position` 写在这里而不是 `.card` 那条规则里 —— 一处够用，也不碰那条规则的其它属性。
function posStyle(c: CardView, i: number) {
  const g = geomOf(c);
  return {
    position: "absolute" as const,
    left: `${g.x}%`,
    top: `${g.y}px`,
    width: `${g.w}%`,
    height: `${g.h}px`,
    // 入场交错：靠内联 delay 而非 CSS 变量，省掉一层自定义属性
    animationDelay: `${i * 35}ms`,
  };
}

/// 画布高度：最靠下那张卡的 y + h，再加一条纵向间距（12 = autoLayout 的行间距）。
/// 没有它，绝对定位的子元素不会撑开父元素，`.content` 就滚动不到下面的卡片。
const canvasHeight = computed(
  () =>
    visibleCards.value.reduce((m, c) => {
      const g = geomOf(c);
      return Math.max(m, g.y + g.h);
    }, 88) + 12,
);

/// 当前生效的配置（未定制时按默认顺序 + 各自默认尺寸现生成一份）
function currentConfig(): CardConfig[] {
  return dash.cards
    ? dash.cards.map((c) => ({ ...c }))
    : defaultConfig();
}

async function saveLayout(cfg: CardConfig[]) {
  // 🔴 乐观更新：**先**把新几何落到本地 store，再发请求。
  // 不这么做的话，松手瞬间卡片会先弹回旧位置（store 里还是旧值），等服务端往返
  // 回来才跳到位 —— 就是「放下时有短暂偏移」。这句在 `await` 之前同步执行，
  // 与调用方的 `move.id = null` 落在同一帧，中间那一帧根本不会渲染出旧位置。
  // ⚠️ 保存失败时本地已经是新值（提示会报错），刷新后回到服务端的值 —— 刻意取舍：
  // 宁可「看着生效了但没存上」并报错，也不要每次松手都弹一下。
  dash.cards = cfg.map((c) => ({ ...c }));
  try {
    await dash.save(cfg);
    // 尺寸或顺序变了，图表容器也跟着变，等 DOM 更新后重算画布
    await nextTick();
    for (const kind of CHART_KINDS) charts[kind]?.resize();
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
  const c = visibleCards.value[i];
  if (!c) return;
  const g = geomOf(c);
  move.id = c.id;
  move.x0 = e.clientX;
  move.y0 = e.clientY;
  move.fromX = g.x;
  move.fromY = g.y;
  move.curX = g.x;
  move.curY = g.y;
  pointerStart = { x: e.clientX, y: e.clientY };
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp, { once: true });
}

/// 右下角手柄的起手：记下起手格数与指针位置，拖动期间只改 curW/curH（不写服务端）。
function onResizeStart(e: PointerEvent, i: number) {
  e.preventDefault();
  e.stopPropagation();
  const c = visibleCards.value[i];
  if (!c) return;
  // 🔴 必须在 `resize.id = c.id` **之前**取几何。
  // 先设 id 的话，下面的 geomOf 会走进「正在缩放」分支、读到 resize.curW/curH
  // —— 那是上一次缩放的残留值（首次是初始值），于是按下手柄的一瞬间卡片就跳到
  // 那个尺寸上（实测表现为「鼠标一按下大小就自己变」）。
  // 这里直接读配置值，不绕 geomOf。
  const w0 = c.w ?? 100;
  const h0 = c.h ?? 88;
  resize.id = c.id;
  resize.x = e.clientX;
  resize.y = e.clientY;
  resize.w = w0;
  resize.h = h0;
  resize.curW = w0;
  resize.curH = h0;
  pointerStart = { x: e.clientX, y: e.clientY };
  window.addEventListener("pointermove", onPointerMove);
  window.addEventListener("pointerup", onPointerUp, { once: true });
}

function onPointerMove(e: PointerEvent) {
  // 自由布局下换算基准只剩一个：画布的**像素宽**（x 是它的百分数）。
  const cw = gridEl.value?.clientWidth || 1;
  // 移动：x 走百分数、y 走像素，两轴都自由（钳在画布内，不让卡片拖出视野）
  if (move.id !== null) {
    // 🔴 x 的上界是「100 − 这张卡的宽」，不是 100：x 是**左边缘**的百分比，
    // 钳到 100 的话卡片会整块落到画布右边外面（用户报过「卡片能去右侧区域外」）。
    const w = visibleCards.value.find((c) => c.id === move.id)?.w ?? 100;
    move.curX = clamp(
      move.fromX + ((e.clientX - move.x0) / cw) * 100,
      0,
      Math.max(0, 100 - w),
    );
    move.curY = Math.max(0, move.fromY + (e.clientY - move.y0));
    return;
  }
  // 缩放：同一套换算，游标位移直接变成宽（%）与高（px），再受最小尺寸约束
  if (resize.id !== null) {
    const lim = sizeLimits(resize.id);
    // 宽度同样不能把自己顶出右边界：上限取「100 − 这张卡的左边缘」
    const x0 = visibleCards.value.find((c) => c.id === resize.id)?.x ?? 0;
    resize.curW = clamp(
      resize.w + ((e.clientX - resize.x) / cw) * 100,
      lim.wMin,
      Math.min(lim.wMax, Math.max(lim.wMin, 100 - x0)),
    );
    resize.curH = clamp(resize.h + (e.clientY - resize.y), lim.hMin, lim.hMax);
    return;
  }
  if (drag.id === null) return;
  drag.dx = e.clientX - pointerStart.x;
  drag.dy = e.clientY - pointerStart.y;
  // 被拖的卡片设了 pointer-events:none，这里的命中会穿到它下面的卡片
  const under = document.elementFromPoint(e.clientX, e.clientY);
  const cardEl = under?.closest<HTMLElement>(".card");
  const idx = cardEl?.dataset.index;
  if (idx !== undefined) dropIndex.value = Number(idx);
}

async function onPointerUp() {
  window.removeEventListener("pointermove", onPointerMove);

  // 拖拽落位：把卡片从原索引搬到落点索引；没挪动就什么都不做。
  // 只改顺序，不改尺寸 —— 尺寸由卡片类型决定（见 store 的 spanFor）。
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
    // 换位后按类型重新分组：同高元素必须成排，否则栅格又留洞。
    // 这里复用 store 的排序（normalize 在每次保存后都会跑一遍）。
    await saveLayout(cfg);
    return;
  }

  // 移动落位：位置没变就不必打扰服务端
  if (move.id !== null) {
    const id = move.id;
    const nx = Math.round(move.curX);
    const ny = Math.round(move.curY);
    const changed = nx !== move.fromX || ny !== move.fromY;
    move.id = null;
    if (changed) {
      const cfg = currentConfig();
      const item = cfg.find((c) => c.id === id);
      if (item) {
        item.x = nx;
        item.y = ny;
        await saveLayout(cfg);
      }
    }
    return;
  }

  // 缩放落位：尺寸没变就不必打扰服务端
  if (resize.id !== null) {
    const id = resize.id;
    // 🔴 落库前**必须取整**：后端只收整数（as_u64），小数会被判「卡片宽度必须是整数」。
    // 渲染用的 curW/curH 保持小数（拖动才平滑），只有写进配置的这两个值要取整。
    const nextW = Math.round(resize.curW);
    const nextH = Math.round(resize.curH);
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

/// 进程表（CPU / 内存 Top 5 排行榜的数据源）。
/// 一次取回全表、本地按两种口径各排一次：`/processes` 每次调用都要整表扫描
/// /proc（内部有 1 秒的最短刷新间隔），按两种排序各查一遍纯属重复劳动。
interface ProcRow {
  pid: number;
  name: string;
  cpu: number;
  mem: number;
}
const procs = ref<ProcRow[]>([]);

/// 目录体积排名（磁盘占用 Top 5）。后端有 3 秒扫描预算 + 10 分钟缓存，
/// 所以这里跟着主轮询走不会反复扫盘（命中缓存时只是读一份内存里的结果）。
interface DirChild {
  path: string;
  bytes: number;
  pct: number;
  truncated: boolean;
}
interface DirReport {
  root: string;
  total: number;
  children: DirChild[];
  truncated: boolean;
  cached: boolean;
}
const dirUsage = ref<DirReport | null>(null);

/// 排行榜是否需要这些额外数据：卡片隐藏时不必为它发请求
/// （visibleCards 已经是「配置里勾选中的卡片」，不在其中即隐藏）
function needsProcesses() {
  return visibleCards.value.some((c) => c.id === "topcpu" || c.id === "topmem");
}
function needsDirUsage() {
  return visibleCards.value.some((c) => c.id === "topdisk");
}

/// 实时波形的环形缓冲：每轮采样把一个点推进去，超长即从头丢。
/// 用独立数组而不是从 `history` 里截尾巴：`history` 是 ECharts 用的降采样历史
/// （窗口可到 2 小时、点数 120），跟「最近两分钟的原始速率」不是一回事。
const netInTrace = ref<(number | null)[]>([]);
const netOutTrace = ref<(number | null)[]>([]);
const diskReadTrace = ref<(number | null)[]>([]);
const diskWriteTrace = ref<(number | null)[]>([]);

/// 推入一个采样点（就地修改，避免每 2 秒重建四个数组触发整页重渲染）
function pushTrace(buf: (number | null)[], v: number) {
  buf.push(v);
  if (buf.length > SPARK_POINTS) buf.splice(0, buf.length - SPARK_POINTS);
}

/// 波形量程（字节/秒）与卡片上的量程标注。
///
/// 🔴 量程取**P90 分位数**而不是最大值：速率类读数尖峰极高（实测磁盘写入平常
/// 50K–230K/s、偶尔一次 7.7M/s），按最大值定标会把整段正常波动压到贴着零线，
/// 看上去就是一条平线。取 P90 让常见区间铺满画布，尖峰被削顶 ——
/// 所以必须同时把量程标出来（`.spark-peak`），否则「顶到边」会被误读成到上限。
///
/// 在父组件算、通过 props 传给 Sparkline：父子两处各算一次的话，
/// 一旦分位数实现有出入，标注的量程就会与画出来的波形不是同一个刻度。
const sparkPeaks = computed<Record<string, string>>(() => {
  const out: Record<string, string> = {};
  for (const c of visibleCards.value) {
    if (!c.spark) continue;
    const vals: number[] = [];
    for (const v of [...c.spark.up, ...c.spark.down]) {
      if (v !== null && v > 0) vals.push(v);
    }
    out[c.id] = fmtBytes(sparkScale(vals));
  }
  return out;
});

/// 同上的数值形式（传给 Sparkline 当刻度），与标注同源
function sparkPeakBytes(id: string): number {
  const c = visibleCards.value.find((x) => x.id === id);
  if (!c?.spark) return 1;
  const vals: number[] = [];
  for (const v of [...c.spark.up, ...c.spark.down]) {
    if (v !== null && v > 0) vals.push(v);
  }
  return sparkScale(vals);
}

/// 线性插值分位数（p ∈ 0..1）；空数组给 1，避免除零
function sparkScale(values: number[], p = 0.9): number {
  if (!values.length) return 1;
  const sorted = [...values].sort((a, b) => a - b);
  const pos = (sorted.length - 1) * p;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  return Math.max(1, sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo));
}

async function refresh() {
  const { data } = await http.get("/system/state");
  Object.assign(snap, data);
  pushTrace(netInTrace.value, snap.net_in_per_sec);
  pushTrace(netOutTrace.value, snap.net_out_per_sec);
  pushTrace(diskReadTrace.value, snap.disk_read_per_sec);
  pushTrace(diskWriteTrace.value, snap.disk_write_per_sec);
}

/// 排行榜数据：各自失败互不影响（磁盘扫描可能因权限/超时失败，
/// 不该把整个仪表盘的轮询拖断）
async function refreshRanks() {
  if (needsProcesses()) {
    try {
      const { data } = await http.get("/processes");
      procs.value = data;
    } catch {
      /* 维持上一次的结果 */
    }
  }
  if (needsDirUsage()) {
    try {
      const { data } = await http.get("/system/dir-usage", {
        params: { path: "/" },
      });
      dirUsage.value = data;
    } catch {
      /* 维持上一次的结果 */
    }
  }
}

/// 拉历史并重画三张趋势图。
/// 三张卡的时间窗各记各的，所以按**去重后的时间窗**各拉一次：默认三张都是
/// 10 分钟 → 只有一个窗口 → 一次请求；谁切到别的窗口才会多出一次。
/// 用 allSettled：某个窗口拉失败不该把另外两张图一起拖黑。
async function refreshHistory() {
  const now = Math.floor(Date.now() / 1000);
  const wanted = [...new Set(CHART_KINDS.map((k) => rangeKeys[k]))];
  await Promise.allSettled(
    wanted.map(async (key) => {
      const r = RANGES.find((x) => x.key === key) ?? RANGES[0];
      const { data } = await http.get("/system/history", {
        params: { from: now - r.secs, to: now, limit: r.points },
      });
      for (const kind of CHART_KINDS) {
        if (rangeKeys[kind] === key) histories[kind] = data;
      }
    }),
  );
  renderCharts();
}

/// 重画三张趋势图（各画各的实例）
function renderCharts() {
  for (const kind of CHART_KINDS) renderChart(kind);
}

/// 画一张趋势图：一条横带、两根线（主序列实线带面积、次序列虚线）。
/// 🔴 拆成「一卡一组」之后不再需要多 grid / 多 yAxis / axisPointer.link ——
/// 那些是「三条带共用一张图」时为补偿量纲差付出的代价。同量纲的两根线
/// （CPU% 与内存% / 入与出 / 读与写）挤在一根轴上不会互相压平。
function renderChart(kind: ChartKind) {
  const chart = charts[kind];
  if (!chart || !chartEls[kind]) return;
  // 颜色从 CSS 变量运行时读取：主题定制（改主色/文本色/背景）后图表跟随，
  // 不再硬编码默认主题的灰阶值。canvas 不受 CSS 级联影响，只能这样取值。
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  const line = v("--el-text-color-primary") || "#111111";
  const sub = v("--el-text-color-secondary") || "#777777";
  const grid = v("--el-fill-color-dark") || "#ebebeb";

  const hist = histories[kind];
  const isPercent = kind === "load";
  // 每组的序列名与取数方式。百分比组靠名字末尾的 `%` 决定 tooltip 单位
  // （见下面 formatter 的说明），所以那个后缀不能省。
  const NAMES: Record<ChartKind, [string, string]> = {
    load: ["CPU %", "内存 %"],
    network: ["网络 ↓", "网络 ↑"],
    disk: ["磁盘读", "磁盘写"],
  };
  const PAIRS: Record<ChartKind, (p: MetricPoint) => [number | null, number | null]> =
    {
      load: (p) => [
        Number(p.cpu.toFixed(2)),
        snap.mem_total > 0
          ? Number(((p.mem_used / snap.mem_total) * 100).toFixed(1))
          : 0,
      ],
      // 带宽类序列（字节/秒）。磁盘两列在旧行上是 null：ECharts 用空值断线，
      // 正好表达「那段没有采集」，而不是画成贴着 0 的直线。
      network: (p) => [p.net_in ?? null, p.net_out ?? null],
      disk: (p) => [p.disk_read ?? null, p.disk_write ?? null],
    };
  const first: (number | null)[] = [];
  const second: (number | null)[] = [];
  for (const p of hist) {
    const [a, b] = PAIRS[kind](p);
    first.push(a);
    second.push(b);
  }
  const [nameA, nameB] = NAMES[kind];
  // 纵轴上限按**这两根线自己的最大值**定（1.2 倍），并留一个下限，免得空数据时
  // 轴塌成一条线。刻度写死 0–100 会让 CPU（常年个位数）贴着底边走直线 ——
  // 这正是从前「趋势看不出变化」的主因。
  //
  // 接收可空序列：磁盘两列在迁移前的历史行上是 null（断线语义），
  // 这里跳过它们即可，不能因为一个 null 就让整根轴退回下限。
  const axisMax = (vals: (number | null)[], floor: number) => {
    const nums = vals.filter((x): x is number => x !== null);
    return nums.length
      ? Math.max(floor, Math.ceil(Math.max(...nums) * 1.2 * 10) / 10)
      : floor;
  };
  const timeLabels = hist.map((p) =>
    new Date(p.ts * 1000).toLocaleTimeString("zh-CN", { hour12: false }),
  );

  chart.setOption({
    backgroundColor: "transparent",
    // 统一调色板为灰阶（tooltip 标记等默认色也走这里）
    color: [line, sub],
    tooltip: {
      trigger: "axis",
      // 挂一个类名给 theme.css 的磨砂规则用（见该处说明）
      className: "dash-tooltip",
      // 🔴 底色与投影**不在 JS 里写死**：写了就会盖掉主题的「不透明度 + 模糊」
      // （内联样式优先级高于样式表，除非处处补 !important）。
      // 这里只交代文字色，底色、模糊、投影统一由 theme.css 的 .dash-tooltip 规则给，
      // 于是主题里的不透明度滑块对浮层同样生效 —— 与消息、对话框、抽屉一致。
      textStyle: { color: line },
      // ECharts 6 的 borderWidth 默认值是 1（见 TooltipModel.js），配上 borderColor
      // 就是一圈可见描边，与全站「不画可见描边」的约定冲突 —— 显式归零。
      borderWidth: 0,
      // 单位由**序列名**决定（名字以 % 结尾就是百分比）。
      //
      // 🔴 不能用 `valueFormatter(val, idx)` 的第二个参数查名字：那是**数据点下标**
      // （ECharts 的 valueFormatter 签名是 (value, valueIndex, seriesData)），
      // 拿它当序列下标会取到别的序列名 —— 实测表现为 CPU 显示成 `0.3B`、
      // 内存显示成 `4.1B`。tooltip 的 formatter 回调里每个 param 自带 seriesName，
      // 那是权威来源，不需要任何下标推算。
      formatter: (items: unknown) => {
        const list = Array.isArray(items) ? items : [items];
        const head = String(
          (list[0] as { axisValueLabel?: string })?.axisValueLabel ?? "",
        );
        const lines = list.map((it) => {
          const p = it as { seriesName?: string; value?: unknown; marker?: string };
          const name = p.seriesName ?? "";
          const v = p.value;
          if (v === null || v === undefined || v === "-") {
            return `${p.marker ?? ""}${name}　无数据`;
          }
          const shown = name.endsWith("%")
            ? `${v}%`
            : `${fmtBytes(Number(v))}/s`;
          return `${p.marker ?? ""}${name}　${shown}`;
        });
        return [head, ...lines].filter(Boolean).join("<br/>");
      },
    },
    // ECharts 6 起 legend 的默认位置由顶部改成了贴底（LegendModel.defaultOption
    // 里 top 被注释、改设 bottom），于是图例会压在 x 轴标签上。这里显式钉回顶部，
    // 正好落在 grid.top 预留的空间里；bottom 保留默认值不影响 top 的解析。
    legend: {
      data: [nameA, nameB],
      textStyle: { color: sub },
      top: 0,
      itemWidth: 14,
      itemHeight: 8,
    },
    // 一条横带铺满卡片：图例占顶部 34px、X 轴标签占底部 22px。
    // 卡高由 spanFor 固定（趋势卡 4×2 = 188px），所以这里写固定像素即可 ——
    // 不再需要从前那套「按容器实测高度反算三条带」的换算。
    grid: [{ left: 46, right: 20, top: 34, bottom: 22 }],
    xAxis: [
      {
        type: "category" as const,
        boundaryGap: false,
        data: timeLabels,
        axisLine: { lineStyle: { color: grid } },
        axisLabel: { color: sub, fontSize: 10 },
      },
    ],
    // 一根 Y 轴：同一张卡里的两根线同量纲，单位随分组（% 或 字节/秒）。
    // 不带单位的刻度在这张图里无法解读。
    yAxis: [
      {
        type: "value" as const,
        min: 0,
        max: axisMax([...first, ...second], isPercent ? 10 : 64 * 1024),
        splitNumber: 3,
        splitLine: { lineStyle: { color: grid } },
        axisLabel: {
          color: line,
          fontSize: 10,
          formatter: (val: number) =>
            isPercent ? `${Math.round(val)}%` : fmtRateAxis(val),
        },
      },
    ],
    // 主序列（实线 + 面积）与次序列（虚线）成对出现，配色沿用全站灰阶约定：
    // 同一张图里两条线靠实/虚与主/次文本色区分，不引入彩色。
    // （sparkSeries 第三个参数是轴下标，现在每张卡只有一根轴，恒为 0。）
    series: [
      sparkSeries(nameA, first, 0, line, false),
      sparkSeries(nameB, second, 0, sub, true),
    ],
  });
}

/// 一条趋势线。抽成工厂函数是为了让上面六个 series 保持一行一条 ——
/// 六个对象的字面量写下来有七十多行，读的时候看不出它们其实只有两处差异
/// （数据、所在带），反而容易写错 yAxisIndex。
function sparkSeries(
  name: string,
  data: (number | null)[],
  band: number,
  color: string,
  dashed: boolean,
) {
  return {
    name,
    type: "line" as const,
    xAxisIndex: band,
    yAxisIndex: band,
    smooth: true,
    showSymbol: false,
    data,
    itemStyle: { color },
    lineStyle: { color, width: 2, type: dashed ? ("dashed" as const) : ("solid" as const) },
    ...(dashed ? {} : { areaStyle: { color, opacity: 0.08 } }),
  };
}
function onResize() {
  // 跨过断点时列数会变（4 ↔ 2 ↔ 1），跨度钳制与卡片宽度都要跟着重算
  syncCols();
  for (const kind of CHART_KINDS) charts[kind]?.resize();
}

/// 图表容器长在卡片网格里，只有该卡片可见时才存在 ——
/// 所以初始化/销毁必须跟着显隐走，不能只在 onMounted 里做一次。
/// 三张趋势卡各建各的实例（三个容器 id），互不影响。
function initCharts() {
  for (const kind of CHART_KINDS) {
    if (charts[kind]) continue;
    const el = document.getElementById(chartDomIdOf(kind));
    if (!el) continue;
    chartEls[kind] = el;
    charts[kind] = echarts.init(el);
  }
  renderCharts();
}

function destroyCharts() {
  for (const kind of CHART_KINDS) {
    charts[kind]?.dispose();
    delete charts[kind];
    delete chartEls[kind];
  }
}

onMounted(async () => {
  await Promise.all([refresh(), refreshHistory(), dash.load()]);
  // 等卡片按配置渲染出来，图表容器才存在
  await nextTick();
  syncCols();
  initCharts();
  refreshRanks().catch(() => {});
  // 卡片与趋势图同频，都是 2 秒 —— 后端采样同样是 2 秒一条，图表跟着它走即可。
  // 早先采样 5 秒 + 图表降到 15 秒拉一次，叠加落库延迟后最新点能滞后 20 秒，
  // 看上去就是「几十秒才动一下」。120 个点的历史请求开销可以忽略，不值得省。
  //
  // 排行榜（进程 / 目录）单独降到 5 秒：进程表要做一次全 /proc 扫描、目录扫描
  // 更重，而排名变化以秒计已经足够。趋势图仍是 2 秒。
  let rankTick = 0;
  timer = window.setInterval(() => {
    // 401 时拦截器会跳登录，这里吞掉 rejection 避免轮询抛出未处理错误
    refresh().catch(() => {});
    refreshHistory().catch(() => {});
    if (++rankTick % 3 === 0) refreshRanks().catch(() => {});
  }, 2000);
  window.addEventListener("resize", onResize);
});

// 卡片显隐会换掉 DOM：趋势图卡从无到有时建实例，从有到无时销毁。
// 键取「当前在显示的趋势卡 id」而不是 some(...) 的布尔值 ——
// 三张里去掉一张时布尔值不变，就不会重建，那一张的容器会留下空壳。
watch(
  () => visibleCards.value.filter((c) => c.isChart).map((c) => c.id).join(","),
  async (ids) => {
    await nextTick();
    if (ids) initCharts();
    else destroyCharts();
  },
);

onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
  window.removeEventListener("resize", onResize);
  // 组件卸载时指针可能仍在拖拽中，把全局监听摘干净
  window.removeEventListener("pointermove", onPointerMove);
  destroyCharts();
});
</script>

<style scoped>
.dash {
  display: flex;
  flex-direction: column;
  gap: var(--sp-4);
  /* 画布的定位基准：工具栏要绝对定位在它顶端（见下面的 .toolbar） */
  position: relative;
}
/* 🔴 工具栏**不占行**、浮在画布之上。
 * 原先它是一个正常的行，于是画布从它下面开始 —— 卡片的 y=0 也就只能落到那一行
 * 之下，**永远拖不到工具栏那条水平线**（用户报的「卡片无法移动到与自定义卡片
 * 相同高度」，「自定义卡片」按钮就在这条工具栏上）。
 * 绝对定位之后画布 = 整个内容区，卡片能拖到最顶上。
 * 代价：它可能压住右上角那张卡的顶部 —— 把卡挪开就行，这是自由布局该有的取舍。 */
.toolbar {
  position: absolute;
  top: 0;
  right: 0;
  z-index: 5;
  margin-bottom: 0;
}
.cards {
  /* 自由布局的画布：卡片都是它的绝对定位子元素，位置与尺寸全部来自配置。
     高度由脚本按「最靠下那张卡的 y + h」现算，用内联 style 下发。 */
  position: relative;
  /* 空配置时不至于塌成 0 */
  min-height: 88px;
}
.card {
  /* 半透卡片：--panel-card-bg 只在主题把卡片不透明度调到 100 以下时才存在
     （见 stores/theme.ts 的 cardBgVar），缺省回退到完全不透明的 --el-bg-color，
     与未定制主题完全一致。半透之后背景图 / 模糊光晕从卡片后面透出来，
     卡上读数仍靠投影立边界（.card--drop 那层落点底也照旧透得出来）。 */
  background: var(--panel-card-bg, var(--el-bg-color));
  /* 毛玻璃：把卡片「背后」那层背景糊掉（backdrop-filter 影响的是元素背后的
     内容，不影响卡片自己的文字与图表）。半径来自主题的 --panel-card-blur。
     🔴 这两条只在变量存在时生效 —— 不支持 backdrop-filter 的浏览器会把未注册的
     自定义属性当作无效值，从而忽略整条声明，卡片退回「仅半透」。
     不写 @supports：那样得把规则拆成两份，反而不如变量自带的降级干净。 */
  -webkit-backdrop-filter: var(--panel-card-blur);
  backdrop-filter: var(--panel-card-blur);
  border-radius: var(--radius);
  /* 内边距取自统一标尺：原来用 --sp-4/--sp-5（四边中最宽的一档），比卡片里
     其它留白都大一圈，加上字号各写各的，整块看起来就不像同一套组件。 */
  padding: var(--sp-3);
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
/* 用两条直角边框画手柄，比塞一个图标省事，也更容易对齐。
 * 尺寸从 7 提到 10、颜色从 placeholder 提到 secondary：原来的标记太小太淡，
 * 用户反馈「根本找不到/不好拖」。
 * 右下角取 --radius 圆角：全站不留直角，两条边在这里接成一段与卡片圆角同心的弧。
 * 盒子 9px = 半径 6 + 描边 2 + 1px 余量；再小就只剩弧、读不出「能抓」。 */
.card-resize::after {
  content: "";
  position: absolute;
  right: 4px;
  bottom: 4px;
  width: 9px;
  height: 9px;
  border-right: 2px solid var(--el-text-color-secondary);
  border-bottom: 2px solid var(--el-text-color-secondary);
  border-bottom-right-radius: var(--radius);
}
.card-resize:hover::after {
  border-color: var(--el-text-color-primary);
}
/* 拖动中的实时格数徽标：浮在手柄左上方，不遮挡卡片内容 */
.card-resize-badge {
  position: absolute;
  right: 18px;
  bottom: 2px;
  padding: 1px var(--sp-1);
  border-radius: var(--radius);
  font-size: var(--fs-xs);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  /* 反色实底：拖动时视线在卡片右下角，这个徽标必须是全卡最清楚的一处 */
  background: var(--el-text-color-primary);
  color: var(--el-bg-color);
  pointer-events: none;
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
/* 🔴 跨 2 行的「大卡」数值样式（放大字号 + 上移）已整条删除，连同模板里那处
   已无任何规则命中的绑定类。它是一条死规则：数值卡的高度由 store 的 spanFor
   固定为 **1 行**，永远不是大卡；而唯一会跨行的 chart / 排行榜卡，其模板里
   根本没有 .card-value。数值卡的字号与位置统一由下面的 .card-value 给出 ——
   不再有「大卡字号档」。 */
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
  font-size: var(--fs-xs);
  color: var(--el-text-color-secondary);
  /* 行高压到 1.25（11px → 约 14px）：卡内容区只有 64px（行高 88 − 内距 12×2），
     标签 + 读数 + 明细 + 量度条四段必须挤进去，行高是大头。 */
  line-height: 1.25;
  /* 标签单行：长卡片名（「磁盘占用 Top 5 目录」）不该换行把内容挤下去 */
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.card-value {
  font-size: var(--fs-xl);
  font-weight: 600;
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  /* 读数行高固定 1.2（20px → 24px），与标签 / 明细的高度预算对齐（见 .card-label）。
     上外距给标签一个呼吸（--sp-1）；下外距归零 —— 明细用 margin-top: auto
     吸走自由空间，读数与明细之间不再叠一份固定外距，四段才放得进 64px。 */
  line-height: 1.2;
  margin: var(--sp-1) 0 0;
  /* 单行 + 省略号：卡片值里既有短值（62%）也有长值（读 2.1M/s 写 480K/s、
     发行版 · 内核），允许换行会让卡片高度参差不齐、三行栅格对不齐 */
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.card-body {
  display: flex;
  flex-direction: column;
  /* 占满标签下方的剩余高度，进度条的 margin-top:auto 才有东西可推 */
  flex: 1;
  min-height: 0;
  /* 矮卡片里内容放不下时裁掉，而不是溢出到卡片外 */
  overflow: hidden;
}
/* 数值下方的次级说明（系统卡里的 CPU 规格、内存卡里的「已用 / 总量」等）。
 * 与标题同档（--fs-xs）：说明不该抢主读数，也不该比卡片名还大。
 * flex:none 让它不被压扁；行高压到 1.2 省出纵向空间。
 * margin-top: auto 把它沉到下沿 —— 自由空间全由这个上外距吸收，
 * 明细因此与量度条一起贴卡片底边，同排卡片下沿自然对齐。 */
.card-hint {
  font-size: var(--fs-xs);
  line-height: 1.2;
  color: var(--el-text-color-secondary);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: none;
  margin-top: auto;
  margin-bottom: var(--sp-1);
}
/* 明细与量度条同处底部组：条不能再抢一次自由空间（否则空白在两者之间
 * 对半分、明细浮不到底，见 .card-hint 的说明），只留一个 --sp-1 的缝。 */
.card-hint + .bar {
  margin-top: 0;
}
/* 排行榜：名次 + 名称 + 数值 三列。
 * 名称列必须 min-width:0 才能被省略号截断（flex 子项默认 min-width:auto，
 * 长进程名 / 目录名会把数值列顶出卡片）。 */
.rank-body {
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 2px;
  margin-top: var(--sp-1);
}
.rank-row {
  display: flex;
  align-items: baseline;
  gap: var(--sp-2);
  font-size: var(--fs-sm);
  line-height: 1.5;
  min-width: 0;
}
.rank-no {
  flex: none;
  width: 14px;
  color: var(--el-text-color-secondary);
  font-variant-numeric: tabular-nums;
}
.rank-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--el-text-color-primary);
}
.rank-val {
  flex: none;
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  color: var(--el-text-color-regular);
}
.rank-empty {
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
}
/* 波形占位：**吃掉「读数」剩下的高度**，不是一个固定尺寸的挂件。
 * 早先写死 height: 28px，而 net / diskio 在 spanFor 下是 **1 行卡**（可用高度
 *   = 行高 88 − 内距 12×2 = 64px），
 *   13.75(标签 11×1.25) + 4(读数上外距) + 24(读数 20×1.2) + 28 = 69.75 > 64，
 * 波形被挤出卡片 5.75px。改成 flex 抢占剩余高度后：
 *   1 行卡里它拿到 64 − 13.75 − 4 − 24 = 22.25px（≥ min-height 18），不再溢出；
 *   2 行卡里它长满多出来的空间，波形反而更可读。
 * 🔴 别再写回固定高度：卡片高度由栅格决定，波形只负责填满它。 */
.spark-box {
  position: relative;
  flex: 1 1 auto;
  margin-top: auto;
  min-height: 18px;
}
/* 量程标注：右上角小字。波形按 P90 定标、尖峰会被削顶，
 * 不标出量程的话「波形顶到边」会被误读成「到达上限了」。 */
.spark-peak {
  position: absolute;
  right: 0;
  top: -2px;
  /* 用最小档：这是量程标注，不能和它会标注的波形抢视觉 */
  font-size: var(--fs-xs);
  line-height: 1;
  color: var(--el-text-color-secondary);
  pointer-events: none;
}
.bar {
  height: 4px;
  /* 无论卡片是 1 行还是 2 行高，进度条都贴底 */
  margin-top: auto;
  border-radius: var(--radius);
  /* 进度条槽：与卡片上其它控件同档，摊开主题的不透明度（缺省 100% = 原值） */
  background: color-mix(
    in srgb,
    var(--el-fill-color) var(--panel-surface-opacity, 100%),
    transparent
  );
  -webkit-backdrop-filter: var(--panel-card-blur, blur(0px));
  backdrop-filter: var(--panel-card-blur, blur(0px));
  overflow: hidden;
}
/* 没有上限的指标（速率 / 计数 / 静态信息）不画槽，只留 4px 占位：
   整块去掉会让同一行卡片的内容高度参差，凭空多出对不齐的留白。
   🔴 这里必须把 backdrop-filter 也一并关掉：底色虽然透明，但「透明元素 + backdrop-filter」
   仍会在圆角边缘露出一圈可见痕迹（实测是一条横贯卡片的暗线，看着就像「没有进度的
   卡片却有一条进度条」）。空占位就该是纯占位 —— 不画底、也不参与合成。 */
.bar--none {
  background: transparent;
  -webkit-backdrop-filter: none;
  backdrop-filter: none;
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

/* 4.2 卡片自定义弹窗
 *
 * 🔴 这里**故意不再定义 .mini-btn / :hover / :disabled / --sm**。
 * 它们原先在本 scoped 块里又定义了一遍（12px、无投影、:disabled 0.5），
 * 与 theme.css 的全局定义（13px、有 --panel-shadow-1、0.55）冲突 ——
 * 同一个按钮类两处定义、字号与投影都不同，仪表盘上的按钮就与
 * 「系统调优」页等页面长得不一样。scoped 属性选择器权重更高，
 * 所以这一份一直在赢，等于全站的 mini-btn 在仪表盘里被悄悄改了。
 * 统一由 theme.css 提供（那里是唯一来源）。 */
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
  font-size: var(--fs-sm);
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
