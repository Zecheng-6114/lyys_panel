<template>
  <div class="ai">
    <div class="cfg">
      <el-input
        v-model="cfg.base_url"
        placeholder="API 根地址，如 https://api.deepseek.com/v1"
        style="width: 260px"
      />
      <el-select
        v-model="cfg.model"
        filterable
        allow-create
        default-first-option
        :loading="loadingModels"
        placeholder="模型（自动获取）"
        style="width: 240px"
      >
        <el-option v-for="m in models" :key="m" :label="m" :value="m" />
      </el-select>
      <el-button :loading="loadingModels" @click="fetchModels">刷新模型</el-button>
      <el-input
        v-model="cfg.tts_url"
        placeholder="语音服务（可选，如 http://<服务器IP>:50001）"
        style="width: 280px"
      />
      <el-input
        v-model="cfg.api_key"
        type="password"
        show-password
        :placeholder="cfg.has_key ? 'API Key（已保存，留空不改）' : 'API Key（本地模型可留空）'"
        style="width: 220px"
      />
      <el-select
        v-model="cfg.embed_model"
        filterable
        allow-create
        clearable
        default-first-option
        :loading="loadingModels"
        placeholder="向量模型（可选，记忆召回用）"
        style="width: 230px"
      >
        <el-option v-for="m in embedCandidates" :key="m" :label="m" :value="m" />
      </el-select>
      <el-checkbox v-model="cfg.auto_remember">自动记忆</el-checkbox>
      <el-checkbox v-model="cfg.tools_enabled" title="只读查询：系统状态、进程、服务、日志、Docker、计划任务">
        工具调用
      </el-checkbox>
      <el-checkbox v-model="cfg.tts_auto" title="每轮回复完自动合成语音并播放（本地合成较慢）">
        自动朗读
      </el-checkbox>
      <el-button :loading="saving" @click="saveCfg">保存</el-button>
      <el-button @click="openMemory">记忆<span v-if="memories.length">（{{ memories.length }}）</span></el-button>
      <span
        class="emo-status"
        :class="{ off: emotionReady === false }"
        :title="emotionError || '本地情绪模型'"
      >
        情绪模型：{{ emotionReady === null ? "检测中" : emotionReady ? "就绪" : "不可用" }}
      </span>
      <span class="spacer" />
      <el-button @click="openHistory">
        历史<span v-if="sessions.length">（{{ sessions.length }}）</span>
      </el-button>
      <el-button :disabled="!messages.length" @click="newSession">新对话</el-button>
      <span v-if="savingSession" class="emo-status">保存中…</span>
    </div>

    <div ref="listEl" class="chat">
      <div v-if="!messages.length" class="empty">
        <div class="empty-title">和陆壹肆聊聊这台服务器</div>
        <div class="empty-hint">
          先在上面填好 API 地址与模型（Key 只需填一次），就可以开始了。
        </div>
      </div>
      <div v-for="(m, i) in messages" :key="i" :class="['row', m.role]">
        <div class="col">
          <div v-for="(t, ti) in m.tools" :key="ti" class="tool-line">{{ t }}</div>
          <div class="bubble">{{ m.content }}</div>
          <div v-if="m.role === 'assistant' && m.content && cfg.tts_url" class="msg-tools">
            <el-button
              link
              size="small"
              :loading="speakingIdx === i"
              @click="speak(m.content, i)"
            >
              {{ speakingIdx === i ? "合成中…" : "朗读" }}
            </el-button>
            <el-button v-if="playingIdx === i" link size="small" @click="stopSpeak">停止</el-button>
          </div>
          <div v-if="m.emotion" class="emo-tag">
            情绪判定：{{ emoLabel(m.emotion.label) }} · {{ Math.round(m.emotion.score * 100) }}%
          </div>
        </div>
      </div>
      <div v-if="sending && !hasDelta" class="row assistant">
        <div class="bubble pending">
          …<span v-if="waited >= 3" class="waited">
            已等待 {{ waited }} 秒{{ waited >= 15 ? "（本地模型可能正在加载）" : "" }}
          </span>
        </div>
      </div>
    </div>

    <div class="composer">
      <el-input
        v-model="draft"
        type="textarea"
        :rows="1"
        resize="none"
        placeholder="说点什么…（Enter 发送，Shift+Enter 换行）"
        @keydown.enter.exact.prevent="send"
      />
      <el-button type="primary" :loading="sending" @click="send">发送</el-button>
    </div>

    <el-dialog v-model="hisOpen" title="历史对话" width="620px">
      <div v-if="!sessions.length" class="mem-empty">还没有存下来的对话</div>
      <div v-for="s in sessions" :key="s.id" class="his-item">
        <div class="his-main" @click="openSession(s)">
          <div class="his-title">{{ s.title }}</div>
          <div class="his-meta">{{ fmtTime(s.updated) }} · {{ s.msg_count }} 条</div>
        </div>
        <el-button link size="small" @click="deleteSession(s)">删除</el-button>
      </div>
    </el-dialog>

    <el-dialog v-model="memOpen" title="长期记忆" width="620px">
      <div class="mem-bar">
        <el-input
          v-model="newMemory"
          placeholder="手动记一条，例如：他的面板部署在 192.168.1.253"
          @keydown.enter.exact.prevent="addMemory"
        />
        <el-button :loading="memSaving" @click="addMemory">记下</el-button>
      </div>
      <div class="mem-hint">
        每轮对话后会自己抽取值得长期记住的事；这里也可以手动增删。
      </div>
      <div v-if="!memories.length" class="mem-empty">还没有记住任何事</div>
      <div v-for="m in memories" :key="m.id" class="mem-item">
        <span class="mem-text">
          {{ m.content }}
          <span v-if="!m.has_embedding" class="mem-flag">（无向量，按关键词召回）</span>
        </span>
        <el-button link size="small" @click="deleteMemory(m)">删除</el-button>
      </div>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import http from "../api/http";
import router from "../router";

interface Emotion {
  label: string;
  score: number;
  valence: number;
}
interface Message {
  role: "user" | "assistant";
  content: string;
  emotion?: Emotion;
  /// 这一轮助手查过什么（工具调用的过程记录）
  tools?: string[];
}

function emoLabel(label: string) {
  return label === "positive" ? "正面" : label === "negative" ? "负面" : "中性";
}

const cfg = reactive({
  base_url: "",
  model: "",
  api_key: "",
  has_key: false,
  /// 算记忆向量用的模型，留空则记忆退化为关键词召回
  embed_model: "",
  /// 每轮对话后自动抽取值得记的事
  auto_remember: true,
  /// 允许助手调用只读工具
  tools_enabled: true,
  /// 语音合成服务地址，留空则关闭朗读
  tts_url: "",
  /// 每轮自动朗读
  tts_auto: false,
});
/// 下拉框里的候选模型（从接口地址拉取）
const models = ref<string[]>([]);
const loadingModels = ref(false);

/// 向量模型的常见命名。接口不区分模型用途（`/v1/models` 把聊天模型和
/// embedding 模型混在一个列表里），所以只能按名字挑 —— 挑不出来就退回
/// 完整列表，反正允许手输。
const EMBED_NAME = /embed|bge|gte|\be5\b|nomic|minilm|jina|arctic|paraphrase|stella|voyage/i;
const embedCandidates = computed(() => {
  const hit = models.value.filter((m) => EMBED_NAME.test(m));
  return hit.length ? hit : models.value;
});
/// 列表对应的地址，用于判断地址改过后要不要重拉
const modelsForUrl = ref("");
/// 本地情绪模型是否可用（null 表示还没检测）
const emotionReady = ref<boolean | null>(null);
const emotionError = ref("");

interface SessionMeta {
  id: number;
  created: number;
  updated: number;
  title: string;
  msg_count: number;
}
interface Memory {
  id: number;
  ts: number;
  content: string;
  has_embedding: boolean;
}
const sessions = ref<SessionMeta[]>([]);
const hisOpen = ref(false);
const sessionId = ref<number | null>(null);
const savingSession = ref(false);
const memories = ref<Memory[]>([]);
const memOpen = ref(false);
const memSaving = ref(false);
const newMemory = ref("");
const messages = ref<Message[]>([]);
const draft = ref("");
const sending = ref(false);
const saving = ref(false);
/// 是否已经收到第一个增量：收到之前显示「已等待 N 秒」
const hasDelta = ref(false);
const waited = ref(0);
const listEl = ref<HTMLElement>();
let waitTimer: number | undefined;

// 等待期间显示已等待秒数：本地模型可能要先把整个模型读进显存才开始回话，
// 屏幕上一直只有一个「…」会让人以为卡死了
function startWaiting() {
  waited.value = 0;
  waitTimer = window.setInterval(() => {
    waited.value += 1;
  }, 1000);
}
function stopWaiting() {
  if (waitTimer) {
    clearInterval(waitTimer);
    waitTimer = undefined;
  }
}

async function loadCfg() {
  const { data } = await http.get("/ai/config");
  cfg.base_url = data.base_url ?? "";
  cfg.model = data.model ?? "";
  cfg.has_key = !!data.has_key;
  cfg.embed_model = data.embed_model ?? "";
  cfg.auto_remember = data.auto_remember !== false;
  cfg.tools_enabled = data.tools_enabled !== false;
  cfg.tts_url = data.tts_url ?? "";
  cfg.tts_auto = data.tts_auto === true;
  cfg.api_key = "";
}

/// 按当前地址拉一次模型列表。地址改过也能先试拉（不必先保存）。
async function fetchModels() {
  if (!cfg.base_url.trim()) return;
  loadingModels.value = true;
  try {
    const { data } = await http.get("/ai/models", {
      params: { base_url: cfg.base_url.trim() },
    });
    models.value = data;
    modelsForUrl.value = cfg.base_url.trim();
    // 还没选模型时自动选第一个，省掉一次手点
    if (!cfg.model && models.value.length) cfg.model = models.value[0];
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "获取模型列表失败");
  } finally {
    loadingModels.value = false;
  }
}

async function saveCfg() {
  saving.value = true;
  try {
    await http.post("/ai/config", {
      base_url: cfg.base_url,
      model: cfg.model,
      api_key: cfg.api_key,
      embed_model: cfg.embed_model,
      auto_remember: cfg.auto_remember,
      tools_enabled: cfg.tools_enabled,
      tts_url: cfg.tts_url,
      tts_auto: cfg.tts_auto,
    });
    // 保存成功后不再回显 Key，只标记为已保存
    cfg.api_key = "";
    cfg.has_key = true;
    ElMessage.success("已保存");
    // 地址换过了就按新地址重新拉一次模型列表
    if (cfg.base_url.trim() !== modelsForUrl.value) await fetchModels();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  } finally {
    saving.value = false;
  }
}

/// 正在合成的消息下标（null 表示没有）
const speakingIdx = ref<number | null>(null);
/// 正在播放的消息下标
const playingIdx = ref<number | null>(null);
let audioEl: HTMLAudioElement | undefined;
let audioUrl: string | undefined;

/// 把一条回复合成语音并播放。合成在服务端做（本地 TTS 短句也要十几秒），
/// 所以这里要等；播放完自动释放 blob，避免把内存攒住。
async function speak(text: string, idx: number) {
  stopSpeak();
  speakingIdx.value = idx;
  try {
    const token = localStorage.getItem("panel_token") ?? "";
    const res = await fetch("/api/ai/speech", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify({ text }),
    });
    if (!res.ok) {
      let msg = `语音合成失败（HTTP ${res.status}）`;
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        // 非 JSON 响应就沿用默认文案
      }
      throw new Error(msg);
    }
    const blob = await res.blob();
    audioUrl = URL.createObjectURL(blob);
    audioEl = new Audio(audioUrl);
    playingIdx.value = idx;
    audioEl.onended = stopSpeak;
    await audioEl.play();
  } catch (e: any) {
    ElMessage.error(e?.message ?? "语音播放失败");
    stopSpeak();
  } finally {
    speakingIdx.value = null;
  }
}

function stopSpeak() {
  if (audioEl) {
    audioEl.pause();
    audioEl = undefined;
  }
  if (audioUrl) {
    URL.revokeObjectURL(audioUrl);
    audioUrl = undefined;
  }
  playingIdx.value = null;
}

/// 新对话：清空当前内容，下一次发送时后端会新建一条会话
function newSession() {
  messages.value = [];
  sessionId.value = null;
}

/// 存档用的标题：取用户说的第一句话
function sessionTitle() {
  const first = messages.value.find((m) => m.role === "user");
  const t = first?.content.trim() ?? "";
  return t ? t.slice(0, 40) : "新对话";
}

/// 每轮对话结束后整份覆盖保存 —— 消息量小，不做增量更不容易出错
async function saveSession() {
  if (!messages.value.length) return;
  savingSession.value = true;
  try {
    const { data } = await http.post("/ai/session", {
      id: sessionId.value,
      title: sessionTitle(),
      messages: messages.value,
    });
    sessionId.value = data.id;
  } catch {
    // 存档失败不该打断对话，静默跳过
  } finally {
    savingSession.value = false;
  }
}

async function loadSessions() {
  try {
    const { data } = await http.get("/ai/session");
    sessions.value = data;
  } catch {
    // 拉不到就保持原样
  }
}

async function openHistory() {
  hisOpen.value = true;
  await loadSessions();
}

async function openSession(s: SessionMeta) {
  try {
    const { data } = await http.get("/ai/session/get", { params: { id: s.id } });
    messages.value = data.messages ?? [];
    sessionId.value = s.id;
    hisOpen.value = false;
    await scrollToBottom();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "载入会话失败");
  }
}

async function deleteSession(s: SessionMeta) {
  try {
    await http.post("/ai/session/delete", { id: s.id });
    // 删的正是当前这条，就清空
    if (sessionId.value === s.id) newSession();
    await loadSessions();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

function fmtTime(ts: number) {
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

async function scrollToBottom() {
  await nextTick();
  if (listEl.value) listEl.value.scrollTop = listEl.value.scrollHeight;
}

async function send() {
  const text = draft.value.trim();
  if (!text || sending.value) return;
  messages.value.push({ role: "user", content: text });
  draft.value = "";
  sending.value = true;
  hasDelta.value = false;
  startWaiting();
  await scrollToBottom();

  // 先占一个空的助手气泡，收到多少写多少
  messages.value.push({ role: "assistant", content: "", tools: [] });
  const idx = messages.value.length - 1;
  let pending = 0;

  try {
    const token = localStorage.getItem("panel_token") ?? "";
    // 后端不存会话，每次把完整历史带上（末尾那个空气泡不算）
    const res = await fetch("/api/ai/chat/stream", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify({ messages: messages.value.slice(0, -1) }),
    });
    if (res.status === 401) {
      localStorage.removeItem("panel_token");
      router.push("/login");
      throw new Error("登录已过期，请重新登录");
    }
    if (!res.ok) {
      let msg = `请求失败（HTTP ${res.status}）`;
      try {
        msg = (await res.json()).error ?? msg;
      } catch {
        // 响应不是 JSON 就沿用默认文案
      }
      throw new Error(msg);
    }
    if (!res.body) throw new Error("响应没有可读内容");

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      // SSE 以空行分隔事件；最后一段可能不完整，留到下一轮
      const chunks = buf.split("\n\n");
      buf = chunks.pop() ?? "";
      for (const chunk of chunks) {
        for (const line of chunk.split("\n")) {
          if (!line.startsWith("data:")) continue;
          const payload = line.slice(5).trim();
          if (!payload) continue;
          const evt = JSON.parse(payload);
          if (evt.error) throw new Error(evt.error);
          if (evt.done && evt.emotion) {
            messages.value[idx].emotion = evt.emotion;
          }
          if (evt.tool) {
            // 工具调用的过程记下来，显示在气泡上方
            (messages.value[idx].tools ??= []).push(evt.tool);
          }
          if (evt.delta) {
            messages.value[idx].content += evt.delta;
            hasDelta.value = true;
            // 每 8 个增量滚一次，避免每个字都触发布局
            if (++pending % 8 === 0) await scrollToBottom();
          }
        }
      }
    }
  } catch (e: any) {
    ElMessage.error(e?.message ?? "请求失败");
    // 一个字都没收到就不留空气泡
    if (!messages.value[idx].content) messages.value.pop();
  } finally {
    sending.value = false;
    stopWaiting();
    await scrollToBottom();
    // 回复结束再存档：失败的那条用户消息也留着，方便改完重发
    await saveSession();
    // 开了自动朗读就把这段读出来（合成慢，放在最后，不挡前面的收尾）
    const reply = messages.value[idx]?.content;
    if (cfg.tts_auto && cfg.tts_url && reply && !messages.value[idx]?.tools?.length) {
      speak(reply, idx);
    }
  }
}

async function loadMemories() {
  try {
    const { data } = await http.get("/ai/memory");
    memories.value = data;
  } catch {
    // 加载失败就保持原样，不打扰用户
  }
}

async function openMemory() {
  memOpen.value = true;
  await loadMemories();
}

async function addMemory() {
  const content = newMemory.value.trim();
  if (!content) return;
  memSaving.value = true;
  try {
    await http.post("/ai/memory", { content });
    newMemory.value = "";
    await loadMemories();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "写入失败");
  } finally {
    memSaving.value = false;
  }
}

async function deleteMemory(m: Memory) {
  try {
    await http.post("/ai/memory/delete", { id: m.id });
    await loadMemories();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

async function loadEmotionStatus() {
  try {
    const { data } = await http.get("/ai/emotion/status");
    emotionReady.value = !!data.usable;
    emotionError.value = data.error ?? "";
  } catch {
    emotionReady.value = false;
  }
}

onMounted(async () => {
  await loadCfg();
  // 进页面就把模型列表拉好，不用手点
  await fetchModels();
  await loadEmotionStatus();
  // 自动载入最近一次会话，刷新页面不再丢对话
  await loadSessions();
  if (sessions.value.length) await openSession(sessions.value[0]);
});
onBeforeUnmount(() => {
  stopWaiting();
  stopSpeak();
});
</script>

<style scoped>
.ai {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: calc(100vh - 108px);
}
.cfg {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.spacer {
  flex: 1;
}
.chat {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
  border-radius: var(--radius);
  background: var(--el-bg-color);
}
.empty {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  text-align: center;
}
.empty-title {
  font-size: 15px;
  font-weight: 600;
}
.empty-hint {
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.row {
  display: flex;
  margin-bottom: 12px;
}
.row.user {
  justify-content: flex-end;
}
.bubble {
  max-width: 70%;
  padding: 10px 14px;
  border-radius: var(--radius);
  font-size: 14px;
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
  background: var(--el-fill-color-light);
}
.row.user .bubble {
  background: var(--el-fill-color);
}
.bubble.pending {
  color: var(--el-text-color-secondary);
}
.waited {
  margin-left: 6px;
  font-size: 12px;
}
.col {
  max-width: 70%;
  display: flex;
  flex-direction: column;
}
.col .bubble {
  max-width: 100%;
}
.tool-line {
  margin-bottom: 4px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.msg-tools {
  margin-top: 4px;
}
.emo-tag {
  margin-top: 4px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.emo-status {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.emo-status.off {
  opacity: 0.6;
}
.his-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  margin-bottom: 6px;
  border-radius: var(--radius);
  background: var(--el-fill-color-light);
}
.his-main {
  flex: 1;
  min-width: 0;
  cursor: pointer;
}
.his-title {
  font-size: 13px;
  line-height: 1.6;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.his-meta {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.mem-bar {
  display: flex;
  gap: 8px;
}
.mem-hint {
  margin: 8px 0 12px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.mem-empty {
  padding: 16px 0;
  text-align: center;
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
.mem-item {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 10px;
  margin-bottom: 6px;
  border-radius: var(--radius);
  background: var(--el-fill-color-light);
}
.mem-text {
  font-size: 13px;
  line-height: 1.6;
}
.mem-flag {
  color: var(--el-text-color-secondary);
  font-size: 12px;
}
.composer {
  display: flex;
  gap: 8px;
  /* 让按钮跟着输入框的高度走：既不写死高度（写死会变成正方形），
     也不会出现两者高度不齐 */
  align-items: stretch;
}
</style>
