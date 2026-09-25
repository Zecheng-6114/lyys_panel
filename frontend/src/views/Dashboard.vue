<template>
  <div class="dash">
    <div class="cards">
      <div class="card">
        <div class="card-label">CPU</div>
        <div class="card-value">{{ snap.cpu.toFixed(1) }}%</div>
        <div class="bar"><i :style="{ width: snap.cpu + '%' }" /></div>
      </div>
      <div class="card">
        <div class="card-label">内存</div>
        <div class="card-value">{{ fmtBytes(snap.mem_used) }}</div>
        <div class="bar">
          <i :style="{ width: pct(snap.mem_used, snap.mem_total) + '%' }" />
        </div>
      </div>
      <div class="card">
        <div class="card-label">磁盘</div>
        <div class="card-value">{{ pct(snap.disk_used, snap.disk_total) }}%</div>
        <div class="bar">
          <i :style="{ width: pct(snap.disk_used, snap.disk_total) + '%' }" />
        </div>
      </div>
      <div class="card">
        <div class="card-label">网络</div>
        <div class="card-value">
          ↓{{ fmtBytes(snap.net_in_per_sec) }}/s ↑{{ fmtBytes(snap.net_out_per_sec) }}/s
        </div>
        <div class="bar"><i :style="{ width: '0%' }" /></div>
      </div>
    </div>

    <div class="chart-card">
      <div class="chart-title">系统负载趋势（最近 {{ history.length }} 个采样点）</div>
      <div ref="chartEl" class="chart" />
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, onBeforeUnmount, reactive, ref } from "vue";
// 按需引入 echarts：全量引入会让本页 chunk 多出约 700KB
import * as echarts from "echarts/core";
import { LineChart } from "echarts/charts";
import { GridComponent, LegendComponent, TooltipComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import http from "../api/http";

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
}
interface MetricPoint {
  ts: number;
  cpu: number;
  mem_used: number;
  net_in: number;
  net_out: number;
}

const snap = reactive<Snapshot>({
  cpu: 0,
  mem_used: 0,
  mem_total: 1,
  disk_used: 0,
  disk_total: 1,
  net_in_per_sec: 0,
  net_out_per_sec: 0,
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
  await Promise.all([refresh(), refreshHistory()]);
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
.chart-title {
  font-size: 14px;
  color: var(--el-text-color-secondary);
  margin-bottom: 8px;
}
.chart {
  height: 300px;
}
@media (max-width: 768px) {
  .cards {
    grid-template-columns: repeat(2, 1fr);
  }
}
</style>
