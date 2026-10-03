<template>
  <!-- 悬浮球：全局 AI 助手入口（点击展开对话面板） -->
  <div class="ai-ball" :style="{ left: pos.x + 'px', top: pos.y + 'px' }">
    <button
      class="ball"
      type="button"
      aria-label="AI 助手"
      title="AI 助手"
      @pointerdown="onDragStart"
      @pointermove="onDragMove"
      @pointerup="onDragEnd"
      @click="onClick"
    >
      <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="9" />
        <path d="M8.5 14.5c1 1.2 2.2 1.8 3.5 1.8s2.5-.6 3.5-1.8" />
        <circle cx="9" cy="10" r="0.6" fill="currentColor" />
        <circle cx="15" cy="10" r="0.6" fill="currentColor" />
      </svg>
    </button>

    <!-- 对话面板 -->
    <div v-if="open" class="panel" :class="panelClass" @pointerdown.stop>
      <div class="panel-head">
        <span class="panel-title">AI 助手</span>
        <span class="spacer" />
        <el-button size="small" text :disabled="!messages.length || streaming" @click="clearHistory">
          清空
        </el-button>
        <el-button size="small" text @click="open = false">收起</el-button>
      </div>

      <div ref="msgsEl" class="msgs">
        <div v-if="!messages.length" class="empty">有什么可以帮你？</div>
        <div v-for="(m, i) in messages" :key="i" class="msg" :class="m.role">
          <details v-if="m.role === 'assistant' && m.reasoning" class="think">
            <summary>深度思考</summary>
            <pre class="think-body">{{ m.reasoning }}</pre>
          </details>
          <div v-if="m.tools?.length" class="tool-lines">
            <div v-for="(t, j) in m.tools" :key="j" class="tool-line">{{ t }}</div>
          </div>
          <div v-if="m.content" class="bubble">{{ m.content }}</div>
        </div>
        <div v-if="streaming" class="msg assistant">
          <details v-if="curReasoning" class="think" open>
            <summary>深度思考中…</summary>
            <pre class="think-body">{{ curReasoning }}</pre>
          </details>
          <div v-if="curTools.length" class="tool-lines">
            <div v-for="(t, j) in curTools" :key="j" class="tool-line">{{ t }}</div>
          </div>
          <div class="bubble">{{ curContent || "…" }}</div>
        </div>
      </div>

      <div class="input-row">
        <el-input
          v-model="draft"
          type="textarea"
          :rows="2"
          resize="none"
          placeholder="输入消息，回车发送"
          @keydown.enter.exact.prevent="send"
        />
        <div class="input-actions">
          <el-button
            type="primary"
            :disabled="streaming || !draft.trim()"
            @click="send"
            >发送</el-button
          >
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { ElMessage } from "element-plus";
import http from "../api/http";

interface Msg {
  role: "user" | "assistant";
  content: string;
  reasoning?: string;
  tools?: string[];
}

const TOOL_LABELS: Record<string, string> = {
  get_system_state: "查询系统状态",
  get_system_history: "查询历史指标",
  list_processes: "查询进程",
  list_services: "查询服务",
  list_packages: "查询软件包",
  list_upgradable_packages: "查询可更新软件包",
  get_docker_status: "查询 Docker 状态",
  list_docker_containers: "查询容器",
  list_docker_images: "查询镜像",
  get_network_interfaces: "查询网络接口",
  get_dns_config: "查询 DNS 配置",
  get_smart_health: "查询磁盘健康",
  read_journal_logs: "读取日志",
};

const open = ref(false);
const messages = ref<Msg[]>([]);
const draft = ref("");
const streaming = ref(false);
const curContent = ref("");
const curReasoning = ref("");
const curTools = ref<string[]>([]);
const msgsEl = ref<HTMLElement | null>(null);
const loaded = ref(false);

// ---------- 悬浮球位置（可拖动，记忆到 localStorage） ----------
const BALL_SIZE = 48;
/** 面板尺寸，与 .panel 的 CSS 对应；翻边判断要用 */
const PANEL_W = 360;
const PANEL_H = 460;
/** 视口尺寸做成响应式：窗口一变，球的位置与面板的展开方向都要跟着重算 */
const vw = ref(window.innerWidth);
const vh = ref(window.innerHeight);

/** 视口内允许的坐标上限（整球可见，各留 4px 边距） */
function maxX() {
  return Math.max(4, vw.value - BALL_SIZE - 4);
}
function maxY() {
  return Math.max(4, vh.value - BALL_SIZE - 4);
}
/** 把球夹回视口内。窗口从大缩小时坐标可能已落在视口外，球就"消失"了 */
function clampIntoView() {
  pos.x = Math.min(Math.max(pos.x, 4), maxX());
  pos.y = Math.min(Math.max(pos.y, 4), maxY());
}
function persist() {
  localStorage.setItem("ai_ball_pos", JSON.stringify({ x: pos.x, y: pos.y }));
}

function defaultPos() {
  return { x: vw.value - BALL_SIZE - 20, y: vh.value - BALL_SIZE - 90 };
}
const pos = reactive(
  (() => {
    try {
      const raw = localStorage.getItem("ai_ball_pos");
      if (raw) {
        const p = JSON.parse(raw);
        // 记忆里的坐标可能来自更大的窗口，挂载时先夹一次
        if (typeof p.x === "number" && typeof p.y === "number") {
          return {
            x: Math.min(Math.max(p.x, 4), maxX()),
            y: Math.min(Math.max(p.y, 4), maxY()),
          };
        }
      }
    } catch {
      /* 损坏则用默认 */
    }
    return defaultPos();
  })()
);

/** 面板朝哪边展开：贴边时翻到另一侧，否则会被视口裁掉 */
const panelClass = computed(() => ({
  "panel--open-right": pos.x < PANEL_W - BALL_SIZE,
  "panel--open-down": pos.y < PANEL_H + 56,
}));

// 拖动与点击区分：位移超过阈值算拖动，pointerup 时抑制紧随的 click
let drag: { startX: number; startY: number; ox: number; oy: number; moved: boolean } | null = null;
let suppressClick = false;
function onDragStart(e: PointerEvent) {
  (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
  drag = { startX: e.clientX, startY: e.clientY, ox: pos.x, oy: pos.y, moved: false };
}
function onDragMove(e: PointerEvent) {
  if (!drag) return;
  const dx = e.clientX - drag.startX;
  const dy = e.clientY - drag.startY;
  if (Math.abs(dx) + Math.abs(dy) > 6) drag.moved = true;
  if (drag.moved) {
    pos.x = Math.min(Math.max(drag.ox + dx, 4), maxX());
    pos.y = Math.min(Math.max(drag.oy + dy, 4), maxY());
  }
}
function onDragEnd() {
  if (drag?.moved) {
    suppressClick = true;
    persist();
  }
  drag = null;
}

/** 视口变化：先更新尺寸基准，再把球拉回可视区并覆写记忆值 */
function onViewportChange() {
  vw.value = window.innerWidth;
  vh.value = window.innerHeight;
  const x = pos.x;
  const y = pos.y;
  clampIntoView();
  if (x !== pos.x || y !== pos.y) persist();
}

onMounted(() => {
  clampIntoView();
  window.addEventListener("resize", onViewportChange);
});
onBeforeUnmount(() => {
  window.removeEventListener("resize", onViewportChange);
});

function onClick() {
  if (suppressClick) {
    suppressClick = false;
    return;
  }
  toggle();
}

function toggle() {
  open.value = !open.value;
  if (open.value && !loaded.value) loadHistory();
  scrollBottom();
}

// ---------- 历史 ----------
async function loadHistory() {
  try {
    const { data } = await http.get("/ai/history");
    messages.value = (data.messages || []).map((m: any) => ({
      role: m.role,
      content: m.content,
    }));
    loaded.value = true;
  } catch (e: any) {
    ElMessage.error(e?.response?.data?.error || "加载对话历史失败");
  }
}

async function clearHistory() {
  try {
    await http.post("/ai/history/clear");
    messages.value = [];
    ElMessage.success("已清空对话历史");
  } catch (e: any) {
    ElMessage.error(e?.response?.data?.error || "清空失败");
  }
}

function scrollBottom() {
  nextTick(() => {
    const el = msgsEl.value;
    if (el) el.scrollTop = el.scrollHeight;
  });
}

// ---------- 发送（SSE 流式） ----------
async function send() {
  const content = draft.value.trim();
  if (!content || streaming.value) return;
  draft.value = "";
  messages.value.push({ role: "user", content });
  scrollBottom();

  streaming.value = true;
  curContent.value = "";
  curReasoning.value = "";
  curTools.value = [];
  try {
    const token = localStorage.getItem("panel_token");
    const resp = await fetch("/api/ai/chat", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({ content }),
    });
    if (!resp.ok || !resp.body) {
      let msg = `请求失败（HTTP ${resp.status}）`;
      try {
        msg = (await resp.json())?.error || msg;
      } catch {
        /* 非 JSON 错误体 */
      }
      ElMessage.error(msg);
      return;
    }
    const reader = resp.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line.startsWith("data:")) continue;
        const data = line.slice(5).trim();
        if (!data || data === "[DONE]") continue;
        let chunk: any;
        try {
          chunk = JSON.parse(data);
        } catch {
          continue;
        }
        if (chunk.error?.message) {
          ElMessage.error(chunk.error.message);
          continue;
        }
        if (chunk.tool) {
          const label = TOOL_LABELS[chunk.tool.name] || chunk.tool.name;
          const mark = chunk.tool.state === "start" ? "🔧" : "✅";
          curTools.value = [...curTools.value, `${mark} ${label}`];
          scrollBottom();
          continue;
        }
        const d = chunk.choices?.[0]?.delta;
        if (d?.reasoning_content) {
          curReasoning.value += d.reasoning_content;
        }
        if (d?.content) {
          curContent.value += d.content;
          scrollBottom();
        }
      }
    }
    if (curContent.value || curReasoning.value || curTools.value.length) {
      messages.value.push({
        role: "assistant",
        content: curContent.value,
        reasoning: curReasoning.value || undefined,
        tools: curTools.value.length ? curTools.value : undefined,
      });
    }
  } catch (e: any) {
    ElMessage.error(e?.message === "Failed to fetch" ? "无法连接面板服务" : "请求失败");
  } finally {
    streaming.value = false;
    curContent.value = "";
    curReasoning.value = "";
    curTools.value = [];
    scrollBottom();
  }
}
</script>

<style scoped>
.ai-ball {
  position: fixed;
  z-index: 3000;
  width: 48px;
  height: 48px;
}
.ball {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  border: none;
  background: var(--el-color-primary);
  color: var(--el-bg-color);
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: grab;
  touch-action: none;
  user-select: none;
}
.ball:active {
  cursor: grabbing;
}
/* 键盘可达：主色球上用文本色画外环，避免与球面同色看不见 */
.ball:focus-visible {
  outline: 2px solid var(--el-text-color-primary);
  outline-offset: 3px;
}
/* 入场 + 悬停/按压反馈。
   入场只播一次（组件每次加载挂载一次），transform/opacity 不触发布局。 */
@media (prefers-reduced-motion: no-preference) {
  .ai-ball {
    animation: ball-in 340ms cubic-bezier(0.16, 1, 0.3, 1) backwards;
  }
  .ball {
    transition:
      transform 160ms ease-out,
      filter 160ms ease-out;
  }
  .ball:hover {
    transform: scale(1.06);
    filter: brightness(1.06);
  }
  .ball:active {
    transform: scale(0.94);
  }
  .panel {
    animation: panel-in 180ms cubic-bezier(0.16, 1, 0.3, 1) backwards;
  }
}
@keyframes ball-in {
  from {
    opacity: 0;
    transform: scale(0.6) translateY(10px);
  }
}
@keyframes panel-in {
  from {
    opacity: 0;
    transform: translateY(6px) scale(0.98);
  }
}
.panel {
  position: absolute;
  right: 0;
  bottom: 56px;
  width: 360px;
  max-width: calc(100vw - 24px);
  height: 460px;
  max-height: calc(100vh - 120px);
  display: flex;
  flex-direction: column;
  background: var(--el-bg-color-overlay);
  border: 1px solid var(--el-border-color-light);
  border-radius: var(--radius);
  overflow: hidden;
}
/* 贴左边缘时改从球的右侧展开，贴顶时改朝下开 —— 否则面板会被视口裁掉 */
.panel--open-right {
  right: auto;
  left: 0;
}
.panel--open-down {
  bottom: auto;
  top: 56px;
}
.panel-head {
  flex: none;
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 8px 10px;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.panel-title {
  font-size: 14px;
  font-weight: 600;
}
.spacer {
  flex: 1;
}
.msgs {
  flex: 1;
  overflow-y: auto;
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  background: var(--el-bg-color-page);
}
.empty {
  margin: auto;
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
.msg.user {
  align-self: flex-end;
}
.msg.assistant {
  align-self: flex-start;
  max-width: 92%;
}
.msg.user .bubble {
  background: var(--el-color-primary);
  color: var(--el-bg-color);
}
.bubble {
  padding: 7px 10px;
  border-radius: var(--radius);
  font-size: 13px;
  line-height: 1.55;
  white-space: pre-wrap;
  word-break: break-word;
  background: var(--el-bg-color);
  border: 1px solid var(--el-border-color-lighter);
}
.think {
  margin-bottom: 4px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.think summary {
  cursor: pointer;
  user-select: none;
}
.think-body {
  margin: 4px 0 0;
  padding: 6px 8px;
  white-space: pre-wrap;
  word-break: break-word;
  background: var(--el-fill-color-light);
  border-radius: var(--radius);
  font-family: inherit;
  max-height: 160px;
  overflow-y: auto;
}
.tool-lines {
  margin-bottom: 4px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.tool-line {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.input-row {
  flex: none;
  padding: 8px;
  border-top: 1px solid var(--el-border-color-lighter);
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.input-actions {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 8px;
}
</style>
