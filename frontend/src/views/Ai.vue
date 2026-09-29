<template>
  <div class="ai">
    <!-- 未配置 AI 上游时引导管理员去设置页（4.5） -->
    <el-alert v-if="!aiConfigured" class="cfg-tip" type="warning" :closable="false">
      <template #title>
        AI 功能尚未配置上游 API（地址 / 密钥 / 模型），暂时无法对话。
        <el-button
          v-if="auth.isAdmin()"
          link
          type="primary"
          @click="router.push('/settings')"
        >
          前往系统设置
        </el-button>
      </template>
    </el-alert>
    <div class="toolbar">
      <span class="hint">会话仅保存在当前页面，刷新或离开后清空</span>
      <span class="spacer" />
      <el-button size="small" :disabled="streaming" @click="clearChat">清空会话</el-button>
    </div>

    <!-- 消息区：流式渲染时自动滚动到底部 -->
    <div ref="msgsEl" class="msgs">
      <div v-if="messages.length === 0" class="empty">
        <div class="empty-title">AI 助手</div>
        <div class="empty-sub">基于 OpenAI 兼容接口，输入问题开始对话</div>
      </div>
      <div
        v-for="(m, i) in messages"
        :key="i"
        class="msg"
        :class="{ user: m.role === 'user', error: m.role === 'error' }"
      >
        <div class="bubble">{{ m.content }}<span v-if="m.streaming" class="cursor">▍</span></div>
      </div>
    </div>

    <div class="input-row">
      <el-input
        v-model="draft"
        class="draft"
        type="textarea"
        :rows="3"
        resize="none"
        placeholder="输入消息，Enter 发送 / Shift+Enter 换行"
        @keydown="onKey"
      />
      <div class="btns">
        <el-button v-if="streaming" size="small" @click="stop">停止</el-button>
        <el-button
          v-else
          type="primary"
          size="small"
          :disabled="!draft.trim()"
          @click="send"
        >
          发送
        </el-button>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { nextTick, onMounted, reactive, ref } from "vue";
import { useRouter } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";
import http from "../api/http";

const router = useRouter();
const auth = useAuthStore();
// 上游是否已配置（设置页密钥或环境变量任一即可）；加载失败按已配置处理，
// 避免一次网络抖动让聊天页出现误导性的“未配置”横幅。
const aiConfigured = ref(true);

onMounted(async () => {
  try {
    const resp = await http.get("/ai/config", { timeout: 5000 });
    aiConfigured.value = !!resp.data?.config?.configured;
  } catch {
    /* 保持默认 true */
  }
});

interface ChatMsg {
  role: "user" | "assistant" | "error";
  content: string;
  streaming?: boolean;
}

const messages = reactive<ChatMsg[]>([]);
const draft = ref("");
const streaming = ref(false);
const msgsEl = ref<HTMLElement>();
let aborter: AbortController | null = null;

function scrollBottom() {
  nextTick(() => {
    const el = msgsEl.value;
    if (el) el.scrollTop = el.scrollHeight;
  });
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    send();
  }
}

function clearChat() {
  messages.splice(0);
}

/// 中断流式响应：后端会感知到下游断开并停止转发上游流量
function stop() {
  aborter?.abort();
}

async function send() {
  const content = draft.value.trim();
  if (!content || streaming.value) return;
  draft.value = "";

  // 服务端不保存历史：每次把已有对话原样带上。
  // 过滤两类不可发送项：错误提示条（role=error），以及上游报错遗留的
  // 空 assistant 占位气泡 —— 后端校验拒绝空内容（400），不过滤会
  // 导致该会话后续每次发送都被拒、无法恢复。
  const history = messages
    .filter(
      (m) =>
        m.role !== "error" &&
        !(m.role === "assistant" && !m.content.trim()),
    )
    .map((m) => ({ role: m.role, content: m.content }));
  messages.push({ role: "user", content });
  scrollBottom();

  const reply = reactive<ChatMsg>({ role: "assistant", content: "", streaming: true });
  messages.push(reply);
  streaming.value = true;
  aborter = new AbortController();

  try {
    const resp = await fetch("/api/ai/chat", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${localStorage.getItem("panel_token") ?? ""}`,
      },
      body: JSON.stringify({ messages: [...history, { role: "user", content }] }),
      signal: aborter.signal,
    });

    if (!resp.ok) {
      let msg = `请求失败（HTTP ${resp.status}）`;
      try {
        const j = await resp.json();
        if (j?.error) msg = j.error;
      } catch {
        /* 非 JSON 错误体，用默认文案 */
      }
      throw new Error(msg);
    }

    // 解析 SSE：事件按行分发，data: 行携带 OpenAI chunk JSON
    const reader = resp.body!.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let idx: number;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx).replace(/\r$/, "");
        buf = buf.slice(idx + 1);
        if (!line.startsWith("data:")) continue;
        const data = line.slice(5).trim();
        if (!data || data === "[DONE]") continue;
        let chunk: any;
        try {
          chunk = JSON.parse(data);
        } catch {
          continue; // 半截 JSON 不可能出现（按行切分），防御即可
        }
        if (chunk.error?.message) throw new Error(chunk.error.message);
        const delta: string | undefined = chunk.choices?.[0]?.delta?.content;
        if (typeof delta === "string") {
          reply.content += delta;
          scrollBottom();
        }
      }
    }
    if (!reply.content) reply.content = "（上游未返回内容）";
  } catch (e: any) {
    if (e?.name === "AbortError") {
      reply.content += reply.content ? "\n（已停止）" : "（已停止）";
    } else {
      const msg = e?.message ?? "请求失败";
      // 用户气泡之间插入一条错误提示（不进入下一轮 history）
      messages.push({ role: "error", content: msg });
      ElMessage.error(msg);
    }
  } finally {
    reply.streaming = false;
    streaming.value = false;
    aborter = null;
    scrollBottom();
  }
}
</script>

<style scoped>
.ai {
  display: flex;
  flex-direction: column;
  /* 撑满内容区：视口高度 - 顶栏 - 内容区上下 padding */
  height: calc(100vh - var(--topbar-h, 52px) - 40px);
}
.toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: none;
  margin-bottom: 12px;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.spacer {
  flex: 1;
}
.msgs {
  flex: 1;
  min-height: 0;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px;
  border-radius: var(--radius);
  background: var(--el-bg-color);
}
.empty {
  margin: auto;
  text-align: center;
  color: var(--el-text-color-secondary);
}
.empty-title {
  font-size: 16px;
  margin-bottom: 6px;
}
.empty-sub {
  font-size: 12px;
}
.msg {
  display: flex;
}
.msg.user {
  justify-content: flex-end;
}
.msg.error {
  justify-content: center;
}
.bubble {
  max-width: 78%;
  padding: 8px 12px;
  border-radius: var(--radius);
  font-size: 13px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  background: var(--el-fill-color-light);
  color: var(--el-text-color-primary);
}
.msg.user .bubble {
  background: var(--el-color-primary);
  color: var(--el-bg-color);
}
.msg.error .bubble {
  background: var(--el-color-danger-light-9, var(--el-fill-color));
  color: var(--el-color-danger);
  font-size: 12px;
}
/* 流式渲染中的光标闪烁 */
.cursor {
  display: inline-block;
  margin-left: 2px;
  animation: blink 1s step-start infinite;
}
@keyframes blink {
  50% {
    opacity: 0;
  }
}
.input-row {
  flex: none;
  display: flex;
  align-items: flex-end;
  gap: 8px;
  margin-top: 12px;
}
.draft {
  flex: 1;
}
.btns {
  display: flex;
  flex-direction: column;
}
@media (max-width: 768px) {
  .ai {
    height: calc(100vh - var(--topbar-h, 52px) - 24px);
  }
  .bubble {
    max-width: 88%;
  }
}
</style>
