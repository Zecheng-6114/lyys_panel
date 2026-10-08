import { onBeforeUnmount, onMounted, unref, type Ref } from "vue";
import type { ComponentPublicInstance } from "vue";

/// 让 Element Plus 的分段单选（el-radio-group）拥有「滑动指示块」。
///
/// 滑块是组上的一个伪元素（`.el-radio-group::before`，见 theme.css）。
/// 这里只负责把「滑块该在哪、多宽」写上去，动画完全交给 CSS 的 transition。
///
/// 🔴 写的是**内联 CSS 变量**（样式表里 `::before` 的 transform/width 读它们）。
/// 伪元素拿不到内联属性，只能靠变量传值；这里的取舍是「写值前先判重」，
/// 避免观察器自身回调反复写同一个值、把进行中的过渡打断。
///
/// 🔴 用 getBoundingClientRect 求差取位置，不要用 offsetLeft：
/// `.el-radio-button__inner` 的 offsetParent 是它自己的 <label>（label 带
/// position: relative），offsetLeft 恒为 0 —— 实测滑块会永远停在最左侧。
///
/// 🔴 模板 ref 绑在 `<el-radio-group>` 上拿到的是**组件实例**，必须经 `$el`
/// 落到根元素。观察器回调早于样式重算，所以统一推迟到下一帧再量、再写。
export function useSegmentIndicator(refEl: Ref<HTMLElement | ComponentPublicInstance | null>) {
  let mo: MutationObserver | null = null;
  let ro: ResizeObserver | null = null;
  let raf = 0;

  /// 组件实例 → 根 DOM 元素；已经是元素就原样返回。
  /// 走 unknown 中转：直接对联合类型做 instanceof 收窄时，TS 会把交叉类型判成 never。
  const resolve = (): HTMLElement | null => {
    const raw = unref(refEl) as unknown;
    if (raw instanceof HTMLElement) return raw;
    const el = (raw as { $el?: unknown } | null)?.$el;
    return el instanceof HTMLElement ? el : null;
  };

  const sync = () => {
    const group = resolve();
    if (!group) return;
    const items = [...group.querySelectorAll<HTMLElement>(".el-radio-button")];
    if (!items.length) return;
    const active = items.find((it) => it.classList.contains("is-active"));
    if (!active) return;
    const inner = active.querySelector<HTMLElement>(".el-radio-button__inner");
    if (!inner) return;

    const gr = group.getBoundingClientRect();
    const ir = inner.getBoundingClientRect();
    const x = Math.round(ir.left - gr.left);
    const w = Math.round(ir.width);
    if (!w) return; // 还没布局出来，下一次观察会再触发

    // 只在真的变了的时候写：观察器回调会因 style 变更再次触发，
    // 反复写同一个值会把进行中的过渡打断（视觉上就是「动画没了」）
    if (group.style.getPropertyValue("--radio-x") === `${x}px` &&
        group.style.getPropertyValue("--radio-w") === `${w}px`) return;
    group.style.setProperty("--radio-x", `${x}px`);
    group.style.setProperty("--radio-w", `${w}px`);
  };

  /// 合并同一帧内的多次触发，并**推迟到下一帧**再量几何
  const schedule = () => {
    if (raf) return;
    raf = requestAnimationFrame(() => {
      raf = 0;
      sync();
    });
  };

  onMounted(() => {
    const group = resolve();
    if (!group) return;
    schedule();
    // 首帧之后再补一次：挂载那一刻标签宽度可能还没定，且 CSS 过渡需要一次「起点」
    requestAnimationFrame(() => requestAnimationFrame(sync));
    mo = new MutationObserver(schedule);
    mo.observe(group, { subtree: true, attributes: true, attributeFilter: ["class"] });
    ro = new ResizeObserver(schedule);
    ro.observe(group);
  });

  onBeforeUnmount(() => {
    if (raf) cancelAnimationFrame(raf);
    mo?.disconnect();
    ro?.disconnect();
  });

  return { sync: schedule };
}
