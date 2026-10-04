/**
 * 断点状态：列折叠的判据。阈值沿用原 responsive.css 的约定 ——
 *   col-p2（中优先级）≤768 隐藏，col-p3（低优先级）≤992 隐藏。
 *
 * 用法：给低优先级的 el-table-column 加 `v-if="!hideColP2"`（或 hideColP3）。
 * 约定不变：「名称 / 状态 / 操作」这类主列不打标，任何宽度都保留；
 * col-p2 = 平板可用、手机隐藏；col-p3 = 平板即隐藏。
 *
 * 🔴 为什么用 JS 而不是 CSS 隐藏：
 * Element Plus 的表格是 `table-layout: fixed` + `<colgroup><col width="…">`，
 * 列宽由 `<col>` 的 **width 属性**决定。用 `display: none` 隐藏 td/th 之后
 * 列的宽度一点不释放 —— 实测 390 视口下进程页表格仍是 960px（容器只有
 * 352px），被隐藏的「可执行路径」列 240px 照旧占位，横拖过去是空白。
 * 试过 `visibility: collapse`（无反应）、`table-layout: auto`（无效）、
 * `auto` + `colgroup{display:none}`（表头表体列宽错位）—— CSS 层无解。
 * 只有让列**不渲染**（v-if），`<col>` 才会随之消失、宽度才真正回收。
 *
 * 用 matchMedia 而不是 resize + innerWidth：只在跨越断点那一刻触发一次，
 * 拖动窗口时不会反复重建整张表的列。
 */
import { ref } from "vue";

const phone = window.matchMedia("(max-width: 768px)");
const tablet = window.matchMedia("(max-width: 992px)");
/**
 * 窄屏压缩区间：接到 1300px 才放行。
 * 各表「最小宽度之和」最大的是会话页（7 列 = 1070px），容器宽度 ≈ 视口 - 216，
 * 所以视口窄于 1286 时原样渲染必然横向溢出；1300 留了余量。
 */
const narrowRange = window.matchMedia("(max-width: 1300px)");

export const hideColP2 = ref(phone.matches);
export const hideColP3 = ref(tablet.matches);
/** 压缩区间也是响应式的：col() 在模板里被调用，读 ref 才能挂上渲染依赖 */
const narrow = ref(narrowRange.matches);

phone.addEventListener("change", (e) => {
  hideColP2.value = e.matches;
});
tablet.addEventListener("change", (e) => {
  hideColP3.value = e.matches;
});
narrowRange.addEventListener("change", (e) => {
  narrow.value = e.matches;
});

/**
 * 窄屏列宽：把声明侧写死的列宽按断点降级。
 *
 * 背景同上 —— 列宽最终落到 `<col width="…">`，CSS 覆盖不掉，只能从声明侧给。
 * 但只回收「被隐藏的列」还不够：390 视口下进程页剩下的
 * PID 90 + 名称 140 + CPU 90 + 内存 100 + 操作 100 = 520px 仍然撑破 352px 的容器，
 * 「操作」整列被推到屏幕外，不横拖就点不到。所以剩下的列也得按比例缩。
 *
 * 做法：宽屏原样返回（固定列给 width、自适应列给 min-width），窄屏**一律**改给
 * min-width 并乘系数 —— 所有列都变成 Element Plus 的弹性列，声明值的和落进
 * 容器，多出来的空间由框架按比例分回各列。
 * ⚠️ 「落进容器」是系数估出来的，不是算出来的：320 视口下包管理页
 * 包名 120 + 描述 180 + 复选框 36 = 336，比 304 的容器仍宽 32px，靠表格自身的
 * 横向滚动兜底。360 及以上实测溢出全为 0。
 *
 * @param base 宽屏下的声明列宽
 * @param flex 宽屏下本来就是 min-width（长文本自适应列）时传 true
 */
const NARROW_RATIO = 0.62; // 769~1300：col-p3 的列已被摘掉，其余统一缩一档
/**
 * 手机档分两套系数：Element Plus 把剩余空间**按 min-width 占比**分给弹性列
 * （实测 390 视口进程页 PID 46/名称 70/CPU 46/内存 50/操作 50，容器 352 →
 * 多出的 90px 正好按 46:70:46:50:50 摊开）。所以给长文本列留更大的 min-width
 * 声明值，它就能多分到宽度 —— 不这么做，名称列只剩 94px，
 * "systemd-udevd" 会被迫折成两行。
 */
const PHONE_RATIO = 0.46; // 定宽列（数字 / 时间 / 按钮）
const PHONE_TEXT_RATIO = 0.6; // 长文本列（名称 / 路径 / 描述…）
/** 压缩下限：再窄就只剩省略号了 */
const NARROW_FLOOR = 44;

export function col(base: number, flex = false) {
  // 宽屏原样返回，桌面布局与改动前逐像素一致
  if (!narrow.value) return flex ? { minWidth: base } : { width: base };
  const ratio = !hideColP2.value
    ? NARROW_RATIO // 平板
    : flex
      ? PHONE_TEXT_RATIO
      : PHONE_RATIO;
  return { minWidth: Math.max(NARROW_FLOOR, Math.round(base * ratio)) };
}
