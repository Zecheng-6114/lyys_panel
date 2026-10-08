<template>
  <div class="terminal-view">
    <div class="toolbar">
      <el-tag :type="statusTag" size="small" effect="plain">{{ statusText }}</el-tag>
      <span class="tip">仅管理员可用；会话随页面关闭而断开。</span>
      <span class="spacer" />
      <template v-if="activeTab">
        <el-button
          v-if="!activeTab.connected"
          type="primary"
          :loading="activeTab.connecting"
          @click="connect(activeTab)"
        >
          连接
        </el-button>
        <el-button v-else @click="disconnect(activeTab)">断开</el-button>
      </template>
    </div>

    <!-- 标签栏：标签在 .tab-scroll 里横向滚动，加号固定在外侧常驻，
         否则终端一多、加号会被挤出视口够不着 -->
    <div class="tabbar">
      <div ref="tabScrollEl" class="tab-scroll" @wheel="onTabWheel">
        <div
          v-for="t in tabs"
          :key="t.id"
          class="tab"
          :class="{ active: t.id === activeId }"
          @click="activate(t.id)"
        >
          <span class="dot" :class="{ on: t.connected }" />
          <span class="tab-name">{{ t.title }}</span>
          <button
            class="tab-close"
            type="button"
            :disabled="tabs.length <= 1"
            :title="tabs.length <= 1 ? '至少保留一个终端' : '关闭'"
            @click.stop="closeTerminal(t)"
          >
            ×
          </button>
        </div>
      </div>
      <button
        class="tab-add"
        type="button"
        title="新建终端"
        aria-label="新建终端"
        @click="addTerminal"
      >
        <!-- 用 SVG 画加号：全角「＋」的字形在系统字体里带基线上偏，居中调不准 -->
        <svg
          viewBox="0 0 24 24"
          width="14"
          height="14"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        >
          <line x1="12" y1="5" x2="12" y2="19" />
          <line x1="5" y1="12" x2="19" y2="12" />
        </svg>
      </button>
    </div>

    <!-- termbox 必须是 .terminal-view 的直接子元素：theme.css 靠 :has(> .termbox)
         把剩余高度交给它；多个终端各占一个，仅当前标签可见 -->
    <div
      v-for="t in tabs"
      v-show="t.id === activeId"
      :key="t.id"
      class="termbox"
      :ref="(el) => bindPane(t, el)"
    />
  </div>
</template>

<script setup lang="ts">
import {
  computed,
  markRaw,
  nextTick,
  onBeforeUnmount,
  onMounted,
  reactive,
  ref,
  watch,
  type ComponentPublicInstance,
} from "vue";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { wsUrl } from "../base";
import { useThemeStore } from "../stores/theme";
import "@xterm/xterm/css/xterm.css";

/// 一个终端 = 一个标签页 = 一条独立的 PTY WebSocket 连接（后端每连接一会话）。
/// xterm 与其配套对象不参与响应式（markRaw），只有 id/title/连接态需要驱动 UI。
interface TermTab {
  id: number;
  title: string;
  connected: boolean;
  connecting: boolean;
  term: Terminal;
  fit: FitAddon;
  ws: WebSocket | null;
  pane: HTMLElement | null;
  observer: ResizeObserver | null;
  /// 抹掉 xterm 内联底色的观察器（见 stripXtermBg）
  bgScrubber: MutationObserver | null;
  resizeTimer: number | undefined;
}

const tabs = reactive<TermTab[]>([]);
const activeId = ref(0);
const tabScrollEl = ref<HTMLElement | null>(null);
let nextId = 1;

const activeTab = computed(() => tabs.find((t) => t.id === activeId.value) ?? null);
const statusText = computed(() => {
  const t = activeTab.value;
  if (t?.connected) return "已连接";
  if (t?.connecting) return "连接中…";
  return "未连接";
});
const statusTag = computed<"success" | "warning" | "info">(() => {
  const t = activeTab.value;
  if (t?.connected) return "success";
  if (t?.connecting) return "warning";
  return "info";
});

/// 终端必须是等宽字体；--panel-mono 其实是正文用的无衬线栈，这里另起一套。
const MONO_STACK =
  '"JetBrains Mono", "Cascadia Mono", "DejaVu Sans Mono", Consolas, "Courier New", monospace';

/// 读当前主题下已应用的 CSS 变量（面板是单一主题，颜色都落在 :root 上）
function cssVar(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

/// 主题色 → `rgba(r, g, b, a)`：把 --el-bg-color（或任意 rgb/hex 值）按透明度摊开。
/// 解析不出来时返回 null，由调用方回落到不透明底色。
///
/// 🔴 必须是**逗号分隔**的老式写法：xterm 的颜色解析器用
/// `/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(,\s*([\d.]+))?\)/` 取色，
/// CSS4 的空格加斜杠写法（`rgb(28 30 48 / 0.7)`）匹配不上、直接抛
/// "Unsupported css format" 并回落成白底 —— 实测踩过：终端整块变白。
/// 同理也不能写字面量 "transparent"（同样不被识别，回落成黑）。
function fadeColor(css: string, opacity: number): string | null {
  const hex = /^#([0-9a-f]{6})$/i.exec(css.trim());
  const rgb = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/.exec(css.trim());
  let parts: [number, number, number] | null = null;
  if (hex) {
    const n = parseInt(hex[1], 16);
    parts = [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  } else if (rgb) {
    parts = [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])];
  }
  if (!parts) return null;
  return `rgba(${parts[0]}, ${parts[1]}, ${parts[2]}, ${(opacity / 100).toFixed(2)})`;
}

/// 解析主题里的不透明度：优先读 --panel-card-bg（形如 color-mix(in srgb, … N%, …)），
/// 它只在主题把不透明度调到 100 以下时才会生成。
function themeOpacity(): number {
  const m = /(\d+(?:\.\d+)?)%/.exec(cssVar("--panel-card-bg", ""));
  return m ? Number(m[1]) : 100;
}

function xtermTheme() {
  const fg = cssVar("--el-text-color-primary", "#1f1f1f");
  const card = cssVar("--el-bg-color", "#ffffff");
  // 给解析器一个能被识别的透明色：DOM 渲染器仍会往它自己那层元素上写
  // rgba(..., 0)，不写底 —— 真正的磨砂交给 .termbox（见 stripXtermBg）。
  const bg = fadeColor(card, 0) ?? "rgba(0, 0, 0, 0)";
  return {
    background: bg,
    foreground: fg,
    cursor: fg,
    selectionBackground: cssVar("--el-fill-color-darker", "#e5e5e5"),
  };
}

/// 抹掉 xterm 自己那层底色，把「透」与「糊」都交回容器 .termbox。
///
/// xterm 的 DOM 渲染器会把 theme.background 写成**内联**样式，直接挂在
/// .xterm-scrollable-element 上（实测：`style="… background-color: rgba(28, 30, 48, 0.7)"`）。
/// 内联样式优先级最高，CSS 压不住；而它每次重绘都会重新写一遍，所以要在写入的
/// 同时清掉 —— 用 MutationObserver 监听 style 变化。
///
/// ⚠️ 这里**不能**像早先设想的那样挂到 canvas 上：@xterm/xterm 6 默认走 DOM
/// 渲染器，页面里根本没有 canvas（实测 canvasCount = 0）。
/// 观察整棵子树是因为这层元素可能随重绘被替换；`background-color` 之外的
/// 内联样式（position 等）必须保留，只删这一个属性。
function stripXtermBg(el: HTMLElement) {
  const scrub = () => {
    const target = el.querySelector<HTMLElement>(".xterm-scrollable-element");
    if (target?.style.backgroundColor) target.style.backgroundColor = "";
  };
  scrub();
  const mo = new MutationObserver(scrub);
  mo.observe(el, { subtree: true, attributes: true, attributeFilter: ["style"] });
  return mo;
}

/// 把当前行列数同步给后端（后端 ioctl TIOCSWINSZ 下发到内核）
function pushResize(tab: TermTab) {
  if (tab.ws && tab.ws.readyState === WebSocket.OPEN) {
    tab.ws.send(JSON.stringify({ type: "resize", cols: tab.term.cols, rows: tab.term.rows }));
  }
}

/// v-for 的函数 ref：把每个标签的 DOM 容器挂到对应终端上
function bindPane(tab: TermTab, el: Element | ComponentPublicInstance | null) {
  tab.pane = (el as HTMLElement | null) ?? null;
}

/// 标签栏是横向滚动区，但滚动条被全局样式藏起来、滚轮又只认纵向，
/// 所以把纵向滚轮改写成横向滚动，否则超出视口的标签根本够不着。
function onTabWheel(e: WheelEvent) {
  const el = tabScrollEl.value;
  if (!el || el.scrollWidth <= el.clientWidth) return;
  if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
  el.scrollLeft += e.deltaY;
  e.preventDefault();
}

function connect(tab: TermTab) {
  if (tab.ws || tab.connecting) return;
  const token = localStorage.getItem("panel_token");
  if (!token) return;
  tab.fit.fit();
  tab.connecting = true;
  const url =
    `${wsUrl("/terminal")}?token=${encodeURIComponent(token)}` +
    `&cols=${tab.term.cols}&rows=${tab.term.rows}`;
  const ws = new WebSocket(url);
  tab.ws = ws;
  // 二进制帧 = PTY 原始输出；文本帧 = 后端的提示（如会话结束）
  ws.binaryType = "arraybuffer";
  ws.onopen = () => {
    tab.connecting = false;
    tab.connected = true;
    if (activeId.value === tab.id) tab.term.focus();
    pushResize(tab);
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") {
      tab.term.writeln(`\r\n\x1b[90m${ev.data}\x1b[0m`);
    } else {
      tab.term.write(new Uint8Array(ev.data as ArrayBuffer));
    }
  };
  ws.onclose = () => {
    tab.connecting = false;
    tab.connected = false;
    tab.ws = null;
  };
  ws.onerror = () => {
    // onclose 必跟在 onerror 之后，状态复位交给 onclose
  };
}

function disconnect(tab: TermTab) {
  tab.ws?.close();
  tab.ws = null;
  tab.connected = false;
  tab.connecting = false;
}

/// 容器尺寸变化 → 重排 + 通知后端；用 rAF 节流，避免拖拽窗口时刷爆 ioctl。
/// 后台标签（display:none，尺寸为 0）不参与重排，否则会把行列算成 0。
function scheduleRefit(tab: TermTab) {
  if (tab.resizeTimer !== undefined) cancelAnimationFrame(tab.resizeTimer);
  tab.resizeTimer = requestAnimationFrame(() => {
    tab.resizeTimer = undefined;
    if (activeId.value !== tab.id || !tab.pane) return;
    tab.fit.fit();
    pushResize(tab);
  });
}

/// 绑定按键输出：xterm 的输入编码成二进制帧发给后端
function setupInput(tab: TermTab) {
  tab.term.onData((data) => {
    if (tab.ws && tab.ws.readyState === WebSocket.OPEN) tab.ws.send(new TextEncoder().encode(data));
  });
  tab.term.onBinary((data) => {
    // 鼠标上报等二进制协议：xterm 给的是「每字符一个字节」的字符串
    if (!tab.ws || tab.ws.readyState !== WebSocket.OPEN) return;
    const bytes = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i++) bytes[i] = data.charCodeAt(i) & 0xff;
    tab.ws.send(bytes);
  });
}

async function addTerminal() {
  const id = nextId++;
  const tab = reactive<TermTab>({
    id,
    title: `终端 ${id}`,
    connected: false,
    connecting: false,
    term: markRaw(
      new Terminal({
        fontFamily: MONO_STACK,
        fontSize: 13,
        cursorBlink: true,
        scrollback: 5000,
        // 底色由 xtermTheme 画成带 alpha 的（见那里的说明）；关掉不透明合成，
        // 让画布的半透底与容器 .termbox 的 backdrop-filter 叠在一起
        allowTransparency: true,
        theme: xtermTheme(),
      }),
    ),
    fit: markRaw(new FitAddon()),
    ws: null,
    pane: null,
    observer: null,
    bgScrubber: null,
    resizeTimer: undefined,
  });
  tab.term.loadAddon(tab.fit);
  setupInput(tab);
  tabs.push(tab);
  activeId.value = id;
  // 等 DOM 更新出容器再 open，否则拿不到尺寸、fit 会算出 0 行列
  await nextTick();
  if (!tab.pane) return;
  tab.term.open(tab.pane);
  // xterm 会给它自己那层元素写内联底色（不透明），把磨砂整块盖住 ——
  // open 之后立刻开始持续清除（详见 stripXtermBg）
  tab.bgScrubber = markRaw(stripXtermBg(tab.pane));
  tab.observer = markRaw(new ResizeObserver(() => scheduleRefit(tab)));
  tab.observer.observe(tab.pane);
  tab.fit.fit();
  connect(tab);
  // 新标签排在列表末尾，标签多时已在视口外：滚过去，让刚建的终端露出来
  const scrollEl = tabScrollEl.value;
  if (scrollEl) scrollEl.scrollTo({ left: scrollEl.scrollWidth, behavior: "smooth" });
}

function activate(id: number) {
  if (id === activeId.value) return;
  activeId.value = id;
  const tab = tabs.find((t) => t.id === id);
  if (!tab) return;
  // 隐藏期间容器尺寸为 0，重新显示后必须重排并聚焦
  nextTick(() => {
    scheduleRefit(tab);
    tab.term.focus();
  });
}

function closeTerminal(tab: TermTab) {
  if (tabs.length <= 1) return;
  const idx = tabs.indexOf(tab);
  const wasActive = activeId.value === tab.id;
  disconnect(tab);
  if (tab.resizeTimer !== undefined) cancelAnimationFrame(tab.resizeTimer);
  tab.observer?.disconnect();
  tab.observer = null;
  tab.bgScrubber?.disconnect();
  tab.bgScrubber = null;
  tab.term.dispose();
  if (idx >= 0) tabs.splice(idx, 1);
  if (wasActive) {
    const next = tabs[Math.min(idx, tabs.length - 1)];
    if (next) activate(next.id);
  }
}

onMounted(() => {
  addTerminal();
});

/// 主题里的「不透明度」变了就把已开终端的底色跟着重算：xterm 的 theme 只在
/// 构造时吃一次，不主动更新的话，设置页把不透明度从 100% 拖到 70% 之后，
/// 终端仍是那块实底，只有新开的标签才跟着变。
const theme = useThemeStore();
watch(
  () => theme.config,
  () => {
    for (const t of tabs) t.term.options.theme = xtermTheme();
  },
  { deep: true },
);

onBeforeUnmount(() => {
  for (const t of tabs) {
    disconnect(t);
    if (t.resizeTimer !== undefined) cancelAnimationFrame(t.resizeTimer);
    t.observer?.disconnect();
    t.bgScrubber?.disconnect();
    t.term.dispose();
  }
  tabs.splice(0);
});
</script>

<style scoped>
.terminal-view {
  display: flex;
  flex-direction: column;
  gap: 0;
}
.tip {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
.tabbar {
  display: flex;
  align-items: center;
  gap: var(--sp-1);
  margin-bottom: var(--sp-2);
}
/* 标签滚动区：flex-basis 取内容宽，标签少时紧贴加号、多了才收缩成滚动区 */
.tab-scroll {
  display: flex;
  align-items: center;
  gap: var(--sp-1);
  flex: 0 1 auto;
  min-width: 0;
  overflow-x: auto;
  overflow-y: hidden;
  /* 藏掉横向滚动条：它是占位的经典滚动条，溢出时会吃掉 6px 高度，
     把 26px 高的标签区撑成 32px，再被 .tabbar 的 center 居中 —— 标签就比
     加号高出几像素（所谓「溢出后错位」）。滚动条本身因主题色透明早已看不见，
     藏掉不损失任何提示，滚动仍可靠滚轮（见 onTabWheel）与新建时自动滚动完成。 */
  scrollbar-width: none;
}
.tab-scroll::-webkit-scrollbar {
  display: none;
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-2);
  flex: none;
  height: 26px;
  padding: 0 var(--sp-1) 0 var(--sp-3);
  border-radius: var(--radius);
  /* 与按钮 / 标签同档：摊开主题的不透明度（缺省 100% = 原值），
     再吃同一份磨砂 —— 否则半透页面上它是仅剩的几块实色小标签。 */
  background: color-mix(
    in srgb,
    var(--el-fill-color-light) var(--panel-surface-opacity, 100%),
    transparent
  );
  -webkit-backdrop-filter: var(--panel-card-blur, blur(0px));
  backdrop-filter: var(--panel-card-blur, blur(0px));
  color: var(--el-text-color-regular);
  font-size: 12px;
  cursor: pointer;
  user-select: none;
  white-space: nowrap;
  transition: background 150ms ease, color 150ms ease;
}
.tab:hover {
  background: color-mix(
    in srgb,
    var(--el-fill-color) var(--panel-surface-opacity, 100%),
    transparent
  );
}
.tab.active {
  background: color-mix(
    in srgb,
    var(--el-fill-color-darker) var(--panel-surface-opacity, 100%),
    transparent
  );
  color: var(--el-text-color-primary);
}
/* 本地保留，不改用全局 .dot 工具类：全局是「实心=开 / 空心环=关」，
   这里以 placeholder 灰 → success 色表示连接态、且尺寸更小（6px，随标签栏 26px 高），
   语义与尺寸都不同；换成全局会改变外观，超出间距统一范围。 */
.dot {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--el-text-color-placeholder);
  transition: background 150ms ease;
}
.dot.on {
  background: var(--el-color-success);
}
.tab-close,
.tab-add {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: none;
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
  padding: 0;
}
.tab-close {
  width: 18px;
  height: 18px;
  border-radius: var(--radius);
  font-size: 14px;
  line-height: 1;
  color: var(--el-text-color-secondary);
}
.tab-close:hover:not(:disabled) {
  background: var(--el-fill-color-dark);
  color: var(--el-text-color-primary);
}
.tab-close:disabled {
  opacity: 0.35;
  cursor: default;
}
.tab-add {
  flex: none;
  width: 26px;
  height: 26px;
  border-radius: var(--radius);
  color: var(--el-text-color-secondary);
  /* 与 .tab 同一档半透 + 磨砂 */
  background: color-mix(
    in srgb,
    var(--el-fill-color-light) var(--panel-surface-opacity, 100%),
    transparent
  );
  -webkit-backdrop-filter: var(--panel-card-blur, blur(0px));
  backdrop-filter: var(--panel-card-blur, blur(0px));
  transition: background 150ms ease, color 150ms ease, transform 120ms ease;
}
.tab-add:hover {
  background: color-mix(
    in srgb,
    var(--el-fill-color) var(--panel-surface-opacity, 100%),
    transparent
  );
  color: var(--el-text-color-primary);
}
.tab-add:active {
  transform: scale(0.92);
}
.termbox {
  /* 兜底高度：不支持 :has() 的浏览器退回固定高（与列表页同一口径） */
  height: var(--panel-table-height);
  background: var(--panel-card-bg, var(--el-bg-color));
  border-radius: var(--radius);
  box-shadow: var(--panel-shadow-1);
  padding: var(--sp-2) var(--sp-3);
  overflow: hidden;
  min-height: 0;
}
/* 动效：只动 opacity/transform，不触发布局；标签在挂载时播一遍入场，
   termbox 靠 display 从 none 变回 block 重播淡入，切换终端时有交代。
   与全站一致：尊重系统「减少动态效果」，走同一套缓动。 */
@media (prefers-reduced-motion: no-preference) {
  .tab {
    animation: tab-in 180ms cubic-bezier(0.16, 1, 0.3, 1) backwards;
  }
  .termbox {
    animation: term-in 160ms ease;
  }
}
@keyframes tab-in {
  from {
    opacity: 0;
    transform: scale(0.9);
  }
}
@keyframes term-in {
  from {
    opacity: 0;
  }
}
</style>
