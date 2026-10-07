<template>
  <!-- 悬浮球：全局 AI 助手入口（点击展开对话面板） -->
  <div class="ai-ball" :style="{ left: pos.x + 'px', top: pos.y + 'px' }">
    <button
      class="ball"
      :class="{ 'is-busy': streaming }"
      type="button"
      aria-label="AI 助手"
      title="AI 助手"
      @pointerdown="onDragStart"
      @pointermove="onDragMove"
      @pointerup="onDragEnd"
      @click="onClick"
    >
      <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9" />
        <path d="M8.5 14.5c1 1.2 2.2 1.8 3.5 1.8s2.5-.6 3.5-1.8" />
        <circle cx="9" cy="10" r="0.6" fill="currentColor" />
        <circle cx="15" cy="10" r="0.6" fill="currentColor" />
      </svg>
      <span v-if="streaming" class="ball-pulse" aria-hidden="true" />
    </button>

    <!-- 对话面板 -->
    <section
      v-if="open"
      class="panel"
      :class="panelClass"
      :style="panelStyle"
      role="dialog"
      aria-label="AI 助手"
      @pointerdown.stop
    >
      <header class="head">
        <span class="status-dot" :class="{ live: streaming }" aria-hidden="true" />
        <span class="title">AI 助手</span>
        <span v-if="modelName" class="model">{{ modelName }}</span>
        <span class="spacer" />
        <button class="icon-btn" type="button" title="清空对话" aria-label="清空对话"
          :disabled="!messages.length || streaming" @click="clearHistory">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
            <path d="M4 7h16M9 7V5h6v2M6 7l1 13h10l1-13" />
          </svg>
        </button>
        <button class="icon-btn" type="button" title="收起（Esc）" aria-label="收起" @click="open = false">
          <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
            <path d="M5 12h14" />
          </svg>
        </button>
      </header>

      <div ref="msgsEl" class="msgs" @scroll.passive="onScroll">
        <!-- 空态：给几个可直接点的常用问题，省得不知道能问什么 -->
        <div v-if="!messages.length" class="welcome">
          <div class="welcome-title">这台服务器的运维助手</div>
          <div class="welcome-sub">
            查实时状态、翻日志、分析服务与容器，需要时还会联网查文档。
          </div>
          <div class="chips">
            <button v-for="q in QUICK" :key="q" class="chip" type="button" @click="send(q)">
              {{ q }}
            </button>
          </div>
          <div v-if="!configured" class="notice">
            还没配置 AI 服务：到「系统设置 → AI API 配置」填上游地址与密钥后即可使用。
          </div>
        </div>

        <div v-for="(m, mi) in messages" :key="mi" class="msg" :class="m.role">
          <!-- 用户消息 -->
          <div v-if="m.role === 'user'" class="bubble">{{ m.content }}</div>

          <!-- 助手消息：按 parts 的时间顺序交错渲染 -->
          <template v-else>
            <template v-for="(p, pi) in m.parts" :key="pi">
              <!-- 思考段（可折叠，流式中默认展开，结束后自动收起） -->
              <div v-if="p.kind === 'reasoning'" class="think">
                <button class="row-btn" type="button" @click="toggleFold(mi, pi, isLiveTail(mi, pi))">
                  <svg class="chev" :class="{ open: isOpen(mi, pi, isLiveTail(mi, pi)) }" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
                    <path d="M9 6l6 6-6 6" />
                  </svg>
                  <span class="row-label">{{ isLiveTail(mi, pi) ? "正在思考" : "思考过程" }}</span>
                  <span class="row-meta">{{ p.text.length }} 字</span>
                </button>
                <div v-show="isOpen(mi, pi, isLiveTail(mi, pi))" class="think-body md" v-html="renderMarkdown(p.text)" />
              </div>

              <!-- 工具调用卡片 -->
              <div v-else-if="p.kind === 'tool'" class="tool" :class="{ fail: p.ok === false }">
                <button class="row-btn" type="button" @click="toggleFold(mi, pi, false)">
                  <svg class="chev" :class="{ open: isOpen(mi, pi, false) }" viewBox="0 0 24 24" width="12" height="12" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
                    <path d="M9 6l6 6-6 6" />
                  </svg>
                  <span class="tstate" aria-hidden="true">
                    <span v-if="p.state === 'running'" class="spin" />
                    <svg v-else-if="p.ok === false" viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" aria-hidden="true">
                      <path d="M6 6l12 12M18 6L6 18" />
                    </svg>
                    <svg v-else viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                      <path d="M5 12.5l4.5 4.5L19 7.5" />
                    </svg>
                  </span>
                  <span class="row-label">{{ toolLabel(p.name) }}</span>
                  <span v-if="p.bytes" class="row-meta">{{ humanBytes(p.bytes) }}</span>
                </button>
                <div v-if="!isOpen(mi, pi, false) && p.preview" class="tpreview">{{ firstLine(p.preview) }}</div>
                <div v-show="isOpen(mi, pi, false)" class="tbody">
                  <div v-if="p.args && p.args !== '{}'" class="tlabel">参数</div>
                  <pre v-if="p.args && p.args !== '{}'" class="tcode">{{ prettyArgs(p.args) }}</pre>
                  <div v-if="p.preview" class="tlabel">结果</div>
                  <pre v-if="p.preview" class="tcode">{{ p.preview }}</pre>
                </div>
              </div>

              <!-- 正文（.answer 而非 .content：.content 是 theme.css 的全局工具类） -->
              <div v-else class="answer md" v-html="renderMarkdown(p.text)" />
            </template>

            <div v-if="!m.parts.length && streaming" class="answer pending">
              <span class="spin" />思考中…
            </div>
            <div v-if="m.error" class="err">{{ m.error }}</div>
            <div v-if="m.parts.length" class="msg-actions">
              <button class="mini" type="button" @click="copyMsg(m)">复制</button>
            </div>
          </template>
        </div>
      </div>

      <button v-if="!atBottom" class="to-bottom" type="button" @click="jumpToBottom">
        <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 5v14M6 13l6 6 6-6" />
        </svg>
        回到底部
      </button>

      <footer class="composer">
        <el-input
          v-model="draft"
          type="textarea"
          :autosize="{ minRows: 1, maxRows: 6 }"
          resize="none"
          placeholder="问点什么，或直接贴一段报错"
          @keydown.enter.exact.prevent="send()"
        />
        <div class="composer-row">
          <span class="hint">Enter 发送 · Shift+Enter 换行</span>
          <span class="spacer" />
          <el-button v-if="streaming" size="small" @click="stop">停止</el-button>
          <el-button
            type="primary"
            size="small"
            :disabled="streaming || !draft.trim()"
            @click="send()"
            >发送</el-button
          >
        </div>
      </footer>
    </section>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import http from "../api/http";
import { humanBytes, renderMarkdown } from "../utils/markdown";

/** 工具显示名：键必须与后端 ai_tools::registry() 的工具名一一对应 */
const TOOL_LABELS: Record<string, string> = {
  get_system_state: "查系统状态",
  get_system_history: "查历史指标",
  list_processes: "查进程",
  list_services: "查服务",
  read_service_unit: "读服务配置",
  list_packages: "查已装软件",
  list_upgradable_packages: "查可更新软件",
  search_packages: "搜软件包",
  get_docker_status: "查 Docker 状态",
  list_docker_containers: "查容器",
  list_docker_images: "查镜像",
  list_compose_projects: "查 Compose 项目",
  get_network_interfaces: "查网络接口",
  get_network_routes: "查路由表",
  get_dns_config: "查 DNS 配置",
  get_smart_health: "查磁盘健康",
  read_journal_logs: "读系统日志",
  list_log_files: "列日志文件",
  read_log_file: "读日志文件",
  list_cron_jobs: "查计划任务",
  list_backups: "查备份",
  list_instances: "查实例",
  get_alert_config: "查告警配置",
  web_search: "联网搜索",
  fetch_web_page: "抓取网页",
};

/** 空态的常用问题：覆盖最常见的四类诉求 */
const QUICK = [
  "现在系统有什么异常吗？",
  "哪些进程占资源最多？",
  "有可升级的软件包吗？",
  "磁盘健康怎么样？",
];

/** 一轮回答里的有序片段（与后端 TracePart 的 kind 对齐） */
interface ToolPart {
  kind: "tool";
  /** 流式期间由后端下发，用于把 done 事件配回同一条；历史数据没有该字段 */
  id?: string;
  name: string;
  args?: string;
  state?: "running" | "done";
  ok?: boolean;
  bytes?: number;
  preview?: string;
}
type Part =
  | { kind: "reasoning"; text: string }
  | ToolPart
  | { kind: "content"; text: string };

interface Msg {
  role: "user" | "assistant";
  content: string;
  parts: Part[];
  error?: string;
}

const open = ref(false);
const messages = ref<Msg[]>([]);
const draft = ref("");
const streaming = ref(false);
const msgsEl = ref<HTMLElement | null>(null);
const historyLoaded = ref(false);
const configured = ref(true);
const modelName = ref("");
const atBottom = ref(true);

// ---------- 悬浮球位置（可拖动，记忆到 localStorage） ----------
const BALL_SIZE = 48;
/** 面板尺寸，与 .panel 的 CSS 对应；展开方向与高度都用它算 */
const PANEL_W = 400;
const PANEL_H = 620;
/** 面板与球之间的净空隙（CSS 里两侧偏移都是 8px） */
const PANEL_GAP = 8;
/** 视口尺寸做成响应式：窗口一变，球的位置与面板的展开方向都要跟着重算 */
const vw = ref(window.innerWidth);
const vh = ref(window.innerHeight);

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
  })(),
);

/** 面板朝哪边展开、能有多高：都按**球的实际位置**算可用空间。
 *  早先只看「球是否偏上（pos.y < PANEL_H + 56）」就决定往下开，面板一加高就露馅：
 *  球在下方时往下开，面板会大半个跑到视口外面去。 */
function roomBelow() {
  // 往下开时面板顶边在 球底 + 8px 处（CSS 里 .panel--open-down 的 top:56px）
  return vh.value - (pos.y + BALL_SIZE + PANEL_GAP) - PANEL_GAP;
}
function roomAbove() {
  // 往上开时面板底边在 球顶 − 8px 处（CSS 里 .panel 的 bottom:56px）
  return pos.y - PANEL_GAP * 2;
}
const openDown = computed(() => vw.value > 640 && roomBelow() >= roomAbove());
const panelClass = computed(() => ({
  "panel--open-right": pos.x < PANEL_W - BALL_SIZE,
  "panel--open-down": openDown.value,
}));
/** 高度按选定方向的可用空间收缩，保证整块面板始终在视口内。
 *  窄屏由 CSS 接管（贴底整幅卡片），这里不出手，否则内联样式会盖掉媒体查询。 */
const panelStyle = computed(() => {
  if (vw.value <= 640) return {};
  const room = Math.max(roomAbove(), roomBelow());
  return { height: `${Math.max(300, Math.min(PANEL_H, room))}px` };
});

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
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape" && open.value) open.value = false;
}

onMounted(() => {
  clampIntoView();
  window.addEventListener("resize", onViewportChange);
  window.addEventListener("keydown", onKeydown);
});
onBeforeUnmount(() => {
  window.removeEventListener("resize", onViewportChange);
  window.removeEventListener("keydown", onKeydown);
  abortCtl?.abort();
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
  if (open.value) {
    if (!historyLoaded.value) loadHistory();
    if (!configLoaded) loadConfig();
    refreshAtBottom();
  }
}

// ---------- 折叠状态 ----------
// 键是「消息序号:片段序号」。片段只会追加、不会重排，所以序号稳定。
const openMap = ref<Record<string, boolean>>({});
/** 是否是流式中的最后一段（思考段据此默认展开、并显示"正在思考"） */
function isLiveTail(mi: number, pi: number): boolean {
  return (
    streaming.value &&
    mi === messages.value.length - 1 &&
    pi === (messages.value[mi]?.parts.length ?? 0) - 1
  );
}
function isOpen(mi: number, pi: number, auto: boolean): boolean {
  const v = openMap.value[`${mi}:${pi}`];
  return v === undefined ? auto : v;
}
function toggleFold(mi: number, pi: number, auto: boolean) {
  openMap.value[`${mi}:${pi}`] = !isOpen(mi, pi, auto);
}

// ---------- 历史与配置 ----------
let configLoaded = false;
async function loadConfig() {
  try {
    const { data } = await http.get("/ai/config");
    const c = data?.config ?? {};
    configured.value = !!c.configured;
    modelName.value = c.model || "";
    configLoaded = true;
  } catch {
    // 配置读不到不影响对话本身，静默即可（真未配置时发消息会给出明确报错）
  }
}

async function loadHistory() {
  try {
    const { data } = await http.get("/ai/history");
    messages.value = (data.messages || []).map(
      (m: { role: "user" | "assistant"; content: string; parts?: Part[] }): Msg => {
        // 老数据（0013 迁移之前）没有 parts：用 content 合成一段，否则历史会是空白
        const parts =
          Array.isArray(m.parts) && m.parts.length
            ? m.parts
            : m.content
              ? [{ kind: "content", text: m.content } as Part]
              : [];
        return { role: m.role, content: m.content, parts };
      },
    );
    historyLoaded.value = true;
    jumpToBottom();
  } catch {
    // 历史拉取失败不该挡住新对话，直接以空列表开始
    historyLoaded.value = true;
  }
}

async function clearHistory() {
  try {
    await http.post("/ai/history/clear");
    messages.value = [];
    openMap.value = {};
  } catch {
    /* 清空失败时保持现状，用户可重试 */
  }
}

// ---------- 滚动 ----------
let scrollPending = false;
let forceScroll = false;
function refreshAtBottom() {
  const el = msgsEl.value;
  if (el) atBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
}
function onScroll() {
  refreshAtBottom();
}
/** 贴底时才跟随新内容；用户往上翻了就别抢他的滚动位置 */
function followScroll() {
  if (atBottom.value) forceScroll = true;
  if (scrollPending) return;
  scrollPending = true;
  requestAnimationFrame(() => {
    scrollPending = false;
    const el = msgsEl.value;
    if (!el) return;
    if (forceScroll) {
      forceScroll = false;
      el.scrollTop = el.scrollHeight;
      atBottom.value = true;
    }
  });
}
function jumpToBottom() {
  forceScroll = true;
  if (!scrollPending) {
    scrollPending = true;
    requestAnimationFrame(() => {
      scrollPending = false;
      forceScroll = false;
      const el = msgsEl.value;
      if (el) {
        el.scrollTop = el.scrollHeight;
        atBottom.value = true;
      }
    });
  }
}

// ---------- 展示辅助 ----------
function toolLabel(name: string): string {
  return TOOL_LABELS[name] || name;
}
/** 参数是模型给的 JSON 字符串，格式化失败就原样显示 */
function prettyArgs(args: string): string {
  try {
    return JSON.stringify(JSON.parse(args), null, 2);
  } catch {
    return args;
  }
}
function firstLine(text: string): string {
  const line = text.split("\n").find((l) => l.trim()) || "";
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

async function copyMsg(m: Msg) {
  const text = m.parts
    .map((p) => (p.kind === "content" ? p.text : ""))
    .join("\n\n")
    .trim() || m.content;
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
    } else {
      // 非安全上下文（http 访问的局域网面板）没有 clipboard API，退回旧接口
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
  } catch {
    /* 复制失败静默：不是关键路径 */
  }
}

// ---------- 发送（SSE 流式） ----------
let abortCtl: AbortController | null = null;

function stop() {
  abortCtl?.abort();
}

async function send(text?: string) {
  const content = (text ?? draft.value).trim();
  if (!content || streaming.value) return;
  draft.value = "";
  messages.value.push({ role: "user", content, parts: [] });
  const assistant: Msg = { role: "assistant", content: "", parts: [] };
  messages.value.push(assistant);
  const target = messages.value[messages.value.length - 1];
  jumpToBottom();

  streaming.value = true;
  abortCtl = new AbortController();
  try {
    const token = localStorage.getItem("panel_token");
    const resp = await fetch("/api/ai/chat", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify({ content }),
      signal: abortCtl.signal,
    });
    if (!resp.ok || !resp.body) {
      let msg = `请求失败（HTTP ${resp.status}）`;
      try {
        msg = (await resp.json())?.error || msg;
      } catch {
        /* 非 JSON 错误体 */
      }
      target.error = msg;
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
          target.error = chunk.error.message;
          continue;
        }
        if (chunk.tool) {
          applyTool(target, chunk.tool);
          followScroll();
          continue;
        }
        const d = chunk.choices?.[0]?.delta;
        if (d?.reasoning_content) {
          appendText(target, "reasoning", d.reasoning_content);
          followScroll();
        }
        if (d?.content) {
          appendText(target, "content", d.content);
          followScroll();
        }
      }
    }
  } catch (e: any) {
    // 用户主动停止不算错误
    if (e?.name !== "AbortError") {
      target.error = e?.message === "Failed to fetch" ? "无法连接面板服务" : "请求失败";
    }
  } finally {
    streaming.value = false;
    abortCtl = null;
    // 整轮什么都没产出的空壳消息直接丢掉，别留一个空气泡
    if (!target.parts.length && !target.error) {
      const i = messages.value.indexOf(target);
      if (i >= 0) messages.value.splice(i, 1);
    } else {
      target.content = target.parts
        .map((p) => (p.kind === "content" ? p.text : ""))
        .join("\n\n")
        .trim();
    }
    followScroll();
  }
}

/** 追加增量文本：与末段同类就续写，否则新起一段（顺序即渲染顺序） */
function appendText(m: Msg, kind: "reasoning" | "content", text: string) {
  const last = m.parts[m.parts.length - 1];
  // 分开写两个判断，让 TS 按 kind 收窄到具体分支（联合比较收窄不可靠）
  if (last?.kind === "reasoning" && kind === "reasoning") {
    last.text += text;
    return;
  }
  if (last?.kind === "content" && kind === "content") {
    last.text += text;
    return;
  }
  if (kind === "reasoning") m.parts.push({ kind: "reasoning", text });
  else m.parts.push({ kind: "content", text });
}

/** 工具事件：start 建卡，done 按 id 回填结果 */
function applyTool(m: Msg, t: any) {
  if (t.state === "start") {
    m.parts.push({
      kind: "tool",
      id: t.id,
      name: t.name,
      args: t.args,
      state: "running",
    });
    return;
  }
  // done：优先按 id 配对，历史/异常情况退回最后一张同名且未完成的卡
  let part = m.parts.find((p) => p.kind === "tool" && p.id && p.id === t.id) as
    | ToolPart
    | undefined;
  if (!part) {
    part = m.parts.find(
      (p) => p.kind === "tool" && p.name === t.name && p.state === "running",
    ) as ToolPart | undefined;
  }
  if (!part) {
    part = { kind: "tool", name: t.name } as ToolPart;
    m.parts.push(part);
  }
  part.state = "done";
  part.ok = t.ok;
  part.bytes = t.bytes;
  part.preview = t.preview;
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
  position: relative;
  width: 48px;
  height: 48px;
  border-radius: 50%;
  border: none;
  background: var(--el-color-primary);
  color: var(--el-bg-color);
  /* 它浮在所有内容之上（可拖拽、随时可点），用浮层档 —— 不加影时这颗实心
   * 圆球贴在页面上，看不出是「浮着的一层」，与 .panel 的浮层语言也不连贯。 */
  box-shadow: var(--panel-shadow-2);
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
/* 生成中：球外一圈脉冲，不用打开面板也知道还在跑 */
.ball-pulse {
  position: absolute;
  inset: -3px;
  border-radius: 50%;
  border: 2px solid var(--el-color-primary);
  opacity: 0.5;
}

/* 面板：不画描边（全站约定），层次靠底色深浅 + 投影 + 留白。
 * 它是浮层 —— 浮在页面任意位置、下面没有遮罩托底 —— 所以用浮层档，
 * 比面板内部的贴面块（工具卡 / 思考块 / 代码块）重一档；
 * 两者用同一档时，面板边界与里面的小卡片分不出高低。 */
.panel {
  position: absolute;
  right: 0;
  bottom: 56px;
  width: 400px;
  max-width: calc(100vw - 24px);
  height: 620px;
  max-height: calc(100vh - 120px);
  display: flex;
  flex-direction: column;
  background: var(--el-bg-color-overlay);
  border-radius: var(--radius);
  box-shadow: var(--panel-shadow-2);
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

.head {
  flex: none;
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  padding: var(--sp-2) var(--sp-3);
}
/* 类名纪律：本组件的样式是 scoped（会补 [data-v-*] 属性，特异性高一档），
   但 theme.css 里有一批**全局工具类**（.content / .dot / .spacer / .mini-btn …），
   它们不受 scoped 约束、只按类名匹配。所以这里凡与全局工具类重名的，一律改名：
   - .content → .answer：曾用 .content 做正文容器，撞上 theme.css:381 的
     `.content { background-image: var(--panel-bg-image) }`，主题一开背景图
     壁纸就漏进聊天正文，正文变成"深底浅字压一张桃色画"，几乎读不了。
   - .dot → .status-dot：全局 .dot 带 margin-right:6px，会在面板标题前多顶 6px。
   scoped 只补属性、不去重，重名时全局规则里**本组件没声明的属性**会照样生效。 */
.status-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--el-text-color-placeholder);
  flex: none;
}
.status-dot.live {
  background: var(--el-color-primary);
}
.title {
  font-size: 13px;
  font-weight: 600;
}
.model {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  max-width: 132px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.spacer {
  flex: 1;
}
.icon-btn {
  border: none;
  background: transparent;
  color: var(--el-text-color-secondary);
  width: 26px;
  height: 26px;
  border-radius: var(--radius);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
}
.icon-btn:hover:not(:disabled) {
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
}
.icon-btn:disabled {
  opacity: 0.35;
  cursor: default;
}

.msgs {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: var(--sp-3);
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
  background: var(--el-bg-color-page);
}

/* ---------- 空态 ---------- */
.welcome {
  margin: auto;
  padding: var(--sp-4) var(--sp-2);
  text-align: center;
}
.welcome-title {
  font-size: 14px;
  font-weight: 600;
  margin-bottom: var(--sp-2);
}
.welcome-sub {
  font-size: 12px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}
.chips {
  display: flex;
  flex-wrap: wrap;
  gap: var(--sp-2);
  justify-content: center;
  margin-top: var(--sp-4);
}
.chip {
  border: none;
  background: var(--el-bg-color);
  color: var(--el-text-color-primary);
  font: inherit;
  font-size: 12px;
  padding: var(--sp-1) var(--sp-3);
  border-radius: var(--radius);
  box-shadow: var(--panel-card-shadow);
  cursor: pointer;
}
.chip:hover {
  background: rgba(var(--el-color-primary-rgb), 0.12);
}
/* 直接坐在消息区（--el-bg-color-page）上的浮起面：底色取 --el-bg-color 再配标准投影。
   若用 --el-fill-color-light，浅色主题下它与页底只差 3 个色阶（#f5f7fa vs #f2f3f5），
   "块"几乎不成形 —— .tool / .chip 早就是用 --el-bg-color + 投影，两种主题都清楚。 */
.notice {
  margin-top: var(--sp-4);
  font-size: 12px;
  line-height: 1.6;
  color: var(--el-text-color-regular);
  background: var(--el-bg-color);
  box-shadow: var(--panel-card-shadow);
  border-radius: var(--radius);
  padding: var(--sp-2) var(--sp-3);
  text-align: left;
}

/* ---------- 消息 ---------- */
.msg {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
  min-width: 0;
}
.msg.user {
  align-items: flex-end;
}
.bubble {
  max-width: 86%;
  padding: var(--sp-2) var(--sp-3);
  border-radius: var(--radius);
  font-size: 13px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  background: var(--el-color-primary);
  color: var(--el-bg-color);
}
.answer {
  font-size: 13px;
  line-height: 1.7;
  word-break: break-word;
}
.answer.pending {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  color: var(--el-text-color-regular);
}
.msg-actions {
  opacity: 0;
  transition: opacity 140ms ease-out;
}
.msg:hover .msg-actions {
  opacity: 1;
}
.mini {
  border: none;
  background: transparent;
  color: var(--el-text-color-secondary);
  font: inherit;
  font-size: 11px;
  cursor: pointer;
  padding: 2px var(--sp-1);
  border-radius: var(--radius);
}
.mini:hover {
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
}
.err {
  font-size: 12px;
  line-height: 1.6;
  color: var(--el-text-color-primary);
  background: var(--el-bg-color);
  box-shadow: var(--panel-card-shadow);
  border-radius: var(--radius);
  padding: var(--sp-2) var(--sp-3);
}

/* ---------- 思考段 ---------- */
.think {
  min-width: 0;
}
.row-btn {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  width: 100%;
  border: none;
  background: transparent;
  padding: var(--sp-1) 0;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  text-align: left;
}
.row-btn:hover .row-label {
  color: var(--el-text-color-primary);
}
.chev {
  flex: none;
  transition: transform 160ms ease-out;
}
.chev.open {
  transform: rotate(90deg);
}
.row-label {
  flex: none;
  font-weight: 500;
}
.row-meta {
  flex: none;
  color: var(--el-text-color-placeholder);
  font-size: 11px;
}
/* 承载实义内容的次要文本用 regular，不用 secondary：
   EP 默认 secondary 在 fill-color-light 上只有 2.87:1（浅色）/ 4.13:1（深色），
   12px 正文级文字达不到 AA 4.5。regular 为 5.69 / 7.36，层次仍弱于正文。 */
.think-body {
  margin-top: var(--sp-1);
  padding: var(--sp-2) var(--sp-3);
  border-radius: var(--radius);
  background: var(--el-bg-color);
  box-shadow: var(--panel-card-shadow);
  color: var(--el-text-color-regular);
  font-size: 12px;
  line-height: 1.7;
  max-height: 220px;
  overflow-y: auto;
}

/* ---------- 工具卡 ---------- */
.tool {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  box-shadow: var(--panel-card-shadow);
  padding: var(--sp-1) var(--sp-3) var(--sp-2);
}
.tool.fail .row-label {
  color: var(--el-text-color-primary);
  text-decoration: line-through;
  text-decoration-thickness: 1px;
}
.tstate {
  flex: none;
  display: inline-flex;
  align-items: center;
  color: var(--el-text-color-secondary);
}
/* 结果首行预览是实义内容（如"搜索结果（通道 bing，共 6 条）"），用 secondary 而非 placeholder */
.tpreview {
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  margin-top: -2px;
}
.tbody {
  padding-top: var(--sp-1);
}
.tlabel {
  font-size: 11px;
  color: var(--el-text-color-placeholder);
  margin: var(--sp-2) 0 var(--sp-1);
}
.tcode {
  margin: 0;
  padding: var(--sp-2);
  background: var(--el-fill-color-light);
  border-radius: var(--radius);
  font-size: 11px;
  line-height: 1.6;
  color: var(--el-text-color-regular);
  max-height: 180px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-word;
}
.spin {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: 1.6px solid var(--el-text-color-placeholder);
  border-top-color: transparent;
  display: inline-block;
}

/* ---------- 正文 Markdown ---------- */
.md :deep(> *:first-child) {
  margin-top: 0;
}
.md :deep(> *:last-child) {
  margin-bottom: 0;
}
.md :deep(p) {
  margin: 0 0 var(--sp-2);
}
.md :deep(h1),
.md :deep(h2),
.md :deep(h3),
.md :deep(h4),
.md :deep(h5),
.md :deep(h6) {
  margin: var(--sp-3) 0 var(--sp-2);
  font-size: 13px;
  font-weight: 600;
}
.md :deep(ul),
.md :deep(ol) {
  margin: 0 0 var(--sp-2);
  padding-left: var(--sp-5);
}
.md :deep(li) {
  margin: var(--sp-1) 0;
}
/* 行内 code 取 --el-fill-color（比 fill-color-light 深一档）：
   它同时会落在消息区（页底色）和思考块（--el-bg-color）两种底上，
   只有深一档才能在两处都成形，fill-color-light 落在页底色上等于没底。 */
.md :deep(code) {
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  background: var(--el-fill-color);
  border-radius: 3px;
  padding: 1px var(--sp-1);
}
.md :deep(.md-pre) {
  margin: 0 0 var(--sp-2);
  padding: var(--sp-2) var(--sp-3);
  background: var(--el-bg-color);
  box-shadow: var(--panel-card-shadow);
  border-radius: var(--radius);
  overflow-x: auto;
  position: relative;
}
.md :deep(.md-pre code) {
  background: none;
  padding: 0;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre;
}
.md :deep(.md-pre[data-lang])::before {
  content: attr(data-lang);
  position: absolute;
  top: 2px;
  right: var(--sp-2);
  font-size: 10px;
  color: var(--el-text-color-placeholder);
}
.md :deep(blockquote) {
  margin: 0 0 var(--sp-2);
  padding: var(--sp-1) var(--sp-3);
  background: var(--el-bg-color);
  box-shadow: var(--panel-card-shadow);
  border-radius: var(--radius);
  color: var(--el-text-color-regular);
}
.md :deep(.md-hr) {
  border: none;
  height: 1px;
  background: var(--el-fill-color);
  margin: var(--sp-3) 0;
}
.md :deep(a) {
  color: var(--el-text-color-primary);
  text-decoration: underline;
  text-underline-offset: 2px;
}

/* ---------- 回到底部 ---------- */
.to-bottom {
  position: absolute;
  left: 50%;
  transform: translateX(-50%);
  bottom: 108px;
  display: inline-flex;
  align-items: center;
  gap: var(--sp-1);
  border: none;
  background: var(--el-bg-color-overlay);
  color: var(--el-text-color-regular);
  font: inherit;
  font-size: 11px;
  padding: var(--sp-1) var(--sp-3);
  border-radius: var(--radius);
  /* 浮在消息区之上的按钮，用浮层档（同一个面板里，芯片等贴面块才用 1 档） */
  box-shadow: var(--panel-shadow-2);
  cursor: pointer;
}

/* ---------- 输入区 ---------- */
.composer {
  flex: none;
  padding: var(--sp-2) var(--sp-3) var(--sp-3);
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
}
.composer-row {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
.hint {
  font-size: 11px;
  color: var(--el-text-color-placeholder);
}

/* 动效：入场 + 悬停反馈；transform/opacity 不触发布局 */
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
  .ball-pulse {
    animation: pulse 1.6s ease-out infinite;
  }
  .spin {
    animation: spin 720ms linear infinite;
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
@keyframes pulse {
  0% {
    transform: scale(0.96);
    opacity: 0.55;
  }
  70% {
    transform: scale(1.18);
    opacity: 0;
  }
  100% {
    opacity: 0;
  }
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

/* 窄屏：面板改成贴在视口底部的整幅卡片，别再跟着球跑 */
@media (max-width: 640px) {
  .panel,
  .panel--open-right,
  .panel--open-down {
    position: fixed;
    left: 8px;
    right: 8px;
    top: auto;
    bottom: 8px;
    width: auto;
    max-width: none;
    height: min(76vh, 620px);
  }
  .to-bottom {
    bottom: 116px;
  }
}
</style>
