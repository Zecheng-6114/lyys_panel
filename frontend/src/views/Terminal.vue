<template>
  <div class="terminal-view">
    <div class="toolbar">
      <el-tag :type="statusTag" size="small" effect="plain">{{ statusText }}</el-tag>
      <span class="tip">仅管理员可用；会话随页面关闭而断开。</span>
      <span class="spacer" />
      <template v-if="activeTab">
        <el-button
          v-if="!activeTab.connected"
          size="small"
          type="primary"
          :loading="activeTab.connecting"
          @click="connect(activeTab)"
        >
          连接
        </el-button>
        <el-button v-else size="small" @click="disconnect(activeTab)">断开</el-button>
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
  type ComponentPublicInstance,
} from "vue";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { wsUrl } from "../base";
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

function xtermTheme() {
  const fg = cssVar("--el-text-color-primary", "#1f1f1f");
  return {
    background: cssVar("--el-bg-color", "#ffffff"),
    foreground: fg,
    cursor: fg,
    selectionBackground: cssVar("--el-fill-color-darker", "#e5e5e5"),
  };
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
        theme: xtermTheme(),
      }),
    ),
    fit: markRaw(new FitAddon()),
    ws: null,
    pane: null,
    observer: null,
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

onBeforeUnmount(() => {
  for (const t of tabs) {
    disconnect(t);
    if (t.resizeTimer !== undefined) cancelAnimationFrame(t.resizeTimer);
    t.observer?.disconnect();
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
  gap: 6px;
  flex: none;
  height: 26px;
  padding: 0 2px 0 10px;
  border-radius: var(--radius);
  background: var(--el-fill-color-light);
  color: var(--el-text-color-regular);
  font-size: 12px;
  cursor: pointer;
  user-select: none;
  white-space: nowrap;
  transition: background 150ms ease, color 150ms ease;
}
.tab:hover {
  background: var(--el-fill-color);
}
.tab.active {
  background: var(--el-fill-color-darker);
  color: var(--el-text-color-primary);
}
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
  background: var(--el-fill-color-light);
  transition: background 150ms ease, color 150ms ease, transform 120ms ease;
}
.tab-add:hover {
  background: var(--el-fill-color);
  color: var(--el-text-color-primary);
}
.tab-add:active {
  transform: scale(0.92);
}
.termbox {
  /* 兜底高度：不支持 :has() 的浏览器退回固定高（与列表页同一口径） */
  height: var(--panel-table-height);
  background: var(--el-bg-color);
  border-radius: var(--radius);
  box-shadow: var(--panel-shadow-1);
  padding: 8px 10px;
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
