<template>
  <div class="dash">
    <div class="cards">
      <!-- 4.2 卡片按用户配置的顺序与显隐渲染；未定制时全部按默认顺序 -->
      <div v-for="card in visibleCards" :key="card.id" class="card">
        <div class="card-label">{{ card.label }}</div>
        <div class="card-value">{{ card.value }}</div>
        <div class="bar"><i :style="{ width: card.bar + '%' }" /></div>
      </div>
    </div>

    <div class="chart-card">
      <div class="chart-head">
        <div class="chart-title">系统负载趋势（最近 {{ history.length }} 个采样点）</div>
        <!-- 4.2 卡片自定义入口（admin 专属，保存走 RequireRole<2> 接口） -->
        <button
          v-if="auth.isAdmin()"
          class="mini-btn"
          type="button"
          @click="openCustom"
        >
          自定义卡片
        </button>
      </div>
      <div ref="chartEl" class="chart" />
    </div>

    <!-- 4.2 卡片显隐/排序配置 -->
    <el-dialog v-model="customOpen" title="自定义仪表盘卡片" width="360px">
      <div class="custom-list">
        <div v-for="(c, i) in draftCards" :key="c" class="custom-row">
          <el-checkbox v-model="draftOn[c]" :label="CARD_LABELS[c]" />
          <div class="custom-btns">
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
      <div class="custom-hint">取消勾选即隐藏卡片；↑↓ 调整展示顺序</div>
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
import { computed, onMounted, onBeforeUnmount, reactive, ref } from "vue";
// 按需引入 echarts：全量引入会让本页 chunk 多出约 700KB
import * as echarts from "echarts/core";
import { LineChart } from "echarts/charts";
import { GridComponent, LegendComponent, TooltipComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import { ElMessage } from "element-plus";
import http from "../api/http";
import { useAuthStore } from "../stores/auth";
import {
  useDashboardStore,
  DASH_CARDS,
  CARD_LABELS,
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
});
const history = ref<MetricPoint[]>([]);
const chartEl = ref<HTMLElement>();
let chart: ReturnType<typeof echarts.init> | null = null;
let timer: number | undefined;

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
  bar: number;
}

/// 展示顺序 = 配置顺序（null 时用默认顺序）；内容始终取实时快照
const visibleCards = computed<CardView[]>(() => {
  const order = dash.cards ?? [...DASH_CARDS];
  return order.map((id) => {
    const base = { id, label: CARD_LABELS[id] };
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
          bar: 0,
        };
      case "load":
        return {
          ...base,
          value: `${snap.load1.toFixed(2)} / ${snap.load5.toFixed(2)} / ${snap.load15.toFixed(2)}`,
          bar: pct(snap.load1, snap.cpu_cores),
        };
      case "uptime":
        return { ...base, value: fmtUptime(snap.uptime), bar: 0 };
      case "swap":
        return {
          ...base,
          value:
            snap.swap_total > 0
              ? `${fmtBytes(snap.swap_used)} / ${fmtBytes(snap.swap_total)}`
              : "未启用",
          bar: snap.swap_total > 0 ? pct(snap.swap_used, snap.swap_total) : 0,
        };
      case "procs":
        return { ...base, value: String(snap.procs), bar: 0 };
    }
  });
});

// ---------- 4.2 自定义弹窗 ----------

const customOpen = ref(false);
const customSaving = ref(false);
/// 弹窗编辑态：勾选状态 + 顺序（含未勾选项，便于重新勾上时知道插回哪）
const draftCards = ref<DashCard[]>([]);
const draftOn = reactive<Record<string, boolean>>({});

function openCustom() {
  const order = dash.cards ?? [...DASH_CARDS];
  // 编辑态按"全部卡片"列出：勾选的在前（按配置顺序），未勾选的追加在后
  const rest = DASH_CARDS.filter((c) => !order.includes(c));
  draftCards.value = [...order, ...rest];
  for (const c of DASH_CARDS) draftOn[c] = order.includes(c);
  customOpen.value = true;
}

function move(i: number, dir: -1 | 1) {
  const j = i + dir;
  if (j < 0 || j >= draftCards.value.length) return;
  const arr = [...draftCards.value];
  [arr[i], arr[j]] = [arr[j], arr[i]];
  draftCards.value = arr;
}

async function saveCustom() {
  const picked = draftCards.value.filter((c) => draftOn[c]);
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

// ---------- 数据刷新 ----------

async function refresh() {
  const { data } = await http.get("/system/state");
  Object.assign(snap, data);
}
async function refreshHistory() {
  const { data } = await http.get("/system/history", { params: { limit: 120 } });
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
  chart.setOption({
    backgroundColor: "transparent",
    // 统一调色板为灰阶（tooltip 标记等默认色也走这里）
    color: [line, sub],
    tooltip: {
      trigger: "axis",
      backgroundColor: v("--el-bg-color-overlay") || cardBg,
      borderColor: grid,
      textStyle: { color: line },
    },
    legend: { data: ["CPU %", "内存 %"], textStyle: { color: sub } },
    grid: { left: 40, right: 20, top: 40, bottom: 30 },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: history.value.map((p) =>
        new Date(p.ts * 1000).toLocaleTimeString("zh-CN", { hour12: false }),
      ),
      axisLine: { lineStyle: { color: grid } },
      axisLabel: { color: sub },
    },
    yAxis: {
      type: "value",
      max: 100,
      splitLine: { lineStyle: { color: grid } },
      axisLabel: { color: sub },
    },
    series: [
      {
        name: "CPU %",
        type: "line",
        smooth: true,
        showSymbol: false,
        data: history.value.map((p) => Number(p.cpu.toFixed(1))),
        itemStyle: { color: line },
        lineStyle: { color: line, width: 2 },
        areaStyle: { color: line, opacity: 0.08 },
      },
      {
        name: "内存 %",
        type: "line",
        smooth: true,
        showSymbol: false,
        data: history.value.map((p) =>
          snap.mem_total > 0 ? Number(((p.mem_used / snap.mem_total) * 100).toFixed(1)) : 0,
        ),
        itemStyle: { color: sub },
        lineStyle: { color: sub, width: 2, type: "dashed" },
      },
    ],
  });
}

function onResize() {
  chart?.resize();
}

onMounted(async () => {
  if (chartEl.value) chart = echarts.init(chartEl.value);
  await Promise.all([refresh(), refreshHistory(), dash.load()]);
  // 卡片与趋势图同频，都是 5 秒 —— 采样本身也是 5 秒一条，图表跟着它走即可。
  // 早先给图表降频到每 3 个 tick（15 秒）拉一次，叠加落库延迟后最新点能滞后 20 秒，
  // 看上去就是「几十秒才动一下」。120 个点的历史请求开销可以忽略，不值得省。
  timer = window.setInterval(() => {
    // 401 时拦截器会跳登录，这里吞掉 rejection 避免轮询抛出未处理错误
    refresh().catch(() => {});
    refreshHistory().catch(() => {});
  }, 5000);
  window.addEventListener("resize", onResize);
});

onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
  window.removeEventListener("resize", onResize);
  chart?.dispose();
});
</script>

<style scoped>
.dash {
  display: flex;
  flex-direction: column;
  gap: 16px;
  /* 与列表页表格同口径：撑满内容区，趋势图吃掉卡片行以下的剩余高度 */
  height: calc(100vh - 160px);
}
.cards {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 16px;
}
.card,
.chart-card {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 16px;
}
.chart-card {
  flex: 1;
  min-height: 240px;
  display: flex;
  flex-direction: column;
}
.card-label {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.card-value {
  font-size: 20px;
  font-weight: 600;
  font-family: var(--panel-mono);
  margin: 6px 0;
}
.bar {
  height: 4px;
  border-radius: var(--radius);
  background: var(--el-fill-color);
  overflow: hidden;
}
.bar i {
  display: block;
  height: 100%;
  background: var(--el-text-color-primary);
  border-radius: var(--radius);
}
.chart-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.chart-title {
  font-size: 14px;
  color: var(--el-text-color-secondary);
  margin-bottom: 8px;
}
.chart {
  flex: 1;
  min-height: 0;
}

/* 4.2 卡片自定义弹窗 */
.mini-btn {
  border: none;
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
  border-radius: var(--radius);
  padding: 6px 12px;
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
  padding: 2px 8px;
}
.custom-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.custom-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.custom-btns {
  display: flex;
  gap: 6px;
}
.custom-hint {
  margin-top: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
/* 弹窗底部按钮组 */
:deep(.el-dialog__footer) {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
}
@media (max-width: 768px) {
  .cards {
    grid-template-columns: repeat(2, 1fr);
  }
}
</style>
