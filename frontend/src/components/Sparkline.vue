<template>
  <!-- 实时波形：镜像面积图。
       上半 = up 序列（网络入 / 磁盘读），下半 = down 序列（网络出 / 磁盘写），
       中线为 0。用「镜像」而不是两条同向折线：两块面积共用一条零线，
       读数时不必分辨哪条线属于哪个方向。
       SVG 而不是再开一个 ECharts 实例：卡片里只有几十个点，
       一套 canvas + 实例管理换不来任何可读性，还要多一份显隐/销毁的生命周期。 -->
  <svg
    v-if="up.length || down.length"
    class="spark"
    viewBox="0 0 100 100"
    preserveAspectRatio="none"
    role="img"
    :aria-label="label"
  >
    <!-- 网格中线：零线本身。没有它两块面积会读成「一块被切开的面积」 -->
    <line x1="0" y1="50" x2="100" y2="50" class="spark-axis" />
    <path v-if="upArea" :d="upArea" class="spark-up" />
    <path v-if="downArea" :d="downArea" class="spark-down" />
  </svg>
</template>

<script setup lang="ts">
import { computed } from "vue";

/// 一组随时间变化的速率（字节/秒），元素为 null 表示该点缺失（不连线）
type Series = (number | null)[];

const props = defineProps<{
  up: Series;
  down: Series;
  label: string;
  /// 画布顶端代表的值（字节/秒）。**由父组件算好传进来** ——
  /// 卡片上的量程标注与这里的刻度必须是同一个数，两处各算一次迟早会漂移。
  peak: number;
}>();

/// 序列 → 面积路径。序列按数组下标均匀铺满 0–100 的横向坐标：
/// 时间轴不按真实时间戳折算 —— 采样间隔固定，均匀即等距。
///
/// 纵向分两块：`up` 占 0–50（零在 50、向上画），`down` 占 50–100（零在 50、
/// 向下画），中线即零线。两块面积因此占满整个高度 —— 只占半高的话，
/// 卡片里 28px 的波形会被压成一条几乎看不出起伏的细线。
///
/// 超过量程的点会被钳到边界（削顶）：量程取的是 P90 分位数，见父组件的说明。
///
/// 🔴 必须按「连续段」分段闭合：直接从末点拉一条回到 x=0 的直线，
/// 会在数据缺口处画出一块覆盖整条横轴以下的幽灵矩形；
/// 每个连续段各自闭合，缺口才是真的断开。
function areaPath(data: Series, dir: "up" | "down"): string {
  const n = data.length;
  if (n < 2) return "";
  const max = Math.max(1, props.peak);
  const x = (i: number) => (i / (n - 1)) * 100;
  // 零线：up 贴 50（向上到 0），down 贴 50（向下到 100）
  const zero = 50;
  // 🔴 先归一化、再钳制：写成 `Math.min(1, v) / max` 时，钳制作用在**原始字节值**
  // 上 —— 任何 ≥1 字节的读数都被压成 1，再除以百万量级的分母，恒为 0，
  // 波形就成了一条零线（实测踩过：数据 52K–110K、量程 106K，画出来全平）。
  const y = (v: number) => {
    const h = Math.min(1, Math.max(0, v / max)) * 50;
    return dir === "up" ? 50 - h : 50 + h;
  };

  let path = "";
  let seg: number[] = [];
  const flush = () => {
    if (seg.length >= 2) {
      let line = "";
      for (const i of seg) {
        const v = data[i];
        if (v === null) continue;
        line += `L${x(i).toFixed(2)},${y(v).toFixed(2)}`;
      }
      if (line) {
        const first = seg[0];
        const last = seg[seg.length - 1];
        path +=
          `M${x(first).toFixed(2)},${zero}` +
          line +
          `L${x(last).toFixed(2)},${zero}L${x(first).toFixed(2)},${zero}Z`;
      }
    }
    seg = [];
  };
  for (let i = 0; i < n; i++) {
    if (data[i] === null) flush();
    else seg.push(i);
  }
  flush();
  return path;
}

const upArea = computed(() => areaPath(props.up, "up"));
const downArea = computed(() => areaPath(props.down, "down"));
</script>

<style scoped>
.spark {
  display: block;
  width: 100%;
  height: 100%;
  overflow: visible;
}
/* 面积用主/次文本色的低透明度：与全站「只用灰阶」一致，主题换色也跟着走。
   描边靠 vector-effect 保持 1px 视觉宽度，不随 viewBox 拉伸变粗。 */
.spark-up {
  fill: color-mix(in srgb, var(--el-text-color-primary) 22%, transparent);
  stroke: var(--el-text-color-primary);
  stroke-width: 1;
  vector-effect: non-scaling-stroke;
}
.spark-down {
  fill: color-mix(in srgb, var(--el-text-color-secondary) 22%, transparent);
  stroke: var(--el-text-color-secondary);
  stroke-width: 1;
  vector-effect: non-scaling-stroke;
}
.spark-axis {
  stroke: color-mix(in srgb, var(--el-text-color-secondary) 45%, transparent);
  stroke-width: 1;
  vector-effect: non-scaling-stroke;
}
</style>
