<template>
  <div class="ai-group">
    <!-- 未配置 AI 上游时引导管理员去设置页 -->
    <el-alert v-if="!aiConfigured" class="cfg-tip" type="warning" :closable="false">
      <template #title>
        AI 功能尚未配置上游 API（地址 / 密钥 / 模型），暂时无法对话。
        <el-button v-if="auth.isAdmin()" link type="primary" @click="router.push('/settings')">
          前往系统设置
        </el-button>
      </template>
    </el-alert>

    <div class="cols">
      <!-- 左：房间列表 -->
      <aside class="col rooms">
        <div class="col-head">
          <span class="col-title">房间</span>
          <el-button size="small" text @click="openRoomDialog">新建</el-button>
        </div>
        <div class="room-list">
          <div
            v-for="r in rooms"
            :key="r.id"
            class="room-item"
            :class="{ active: r.id === currentRoomId }"
            @click="selectRoom(r.id)"
          >
            <div class="room-name">{{ r.name }}</div>
            <div class="room-meta">
              <el-tag v-if="r.visibility === 'public'" size="small" type="success" effect="plain">
                公开
              </el-tag>
              <el-tag v-else size="small" type="info" effect="plain">私有</el-tag>
            </div>
          </div>
          <div v-if="!rooms.length" class="col-empty">还没有房间</div>
        </div>
      </aside>

      <!-- 中：聊天区 -->
      <section class="col chat">
        <div class="col-head">
          <span class="col-title">{{ currentRoom?.name || "AI 群聊" }}</span>
          <span class="spacer" />
          <el-tag v-if="chainActive" size="small" type="warning" effect="dark">调度中</el-tag>
          <el-button
            v-if="auth.isAdmin() && chainActive"
            size="small"
            type="danger"
            plain
            @click="stopChain"
          >
            停止调度
          </el-button>
          <el-button size="small" text @click="openMemberDialog" :disabled="!currentRoomId">
            成员
          </el-button>
        </div>

        <div ref="msgsEl" class="msgs" @scroll="onScroll">
          <div v-if="!messages.length" class="empty">
            <div class="empty-title">{{ currentRoom?.name || "AI 群聊" }}</div>
            <div class="empty-sub">@ 某个 AI 触发回复；@ 管理员AI 进入调度链；不 @ 则 AI 沉默</div>
          </div>
          <div
            v-for="m in messages"
            :key="m.key"
            class="msg"
            :class="[m.sender_type, { mine: m.sender_type === 'user' && m.sender_id === myId }]"
          >
            <div v-if="m.sender_type === 'system'" class="sys-note">{{ m.content }}</div>
            <template v-else>
              <div class="who">{{ m.sender_name }}</div>
              <div v-if="m.reasoning" class="think">
                <div class="think-head" @click="m.thinkOpen = !m.thinkOpen">
                  <span class="think-tag">深度思考</span>
                  <span class="think-arrow">{{ m.thinkOpen ? "收起 ▲" : "展开 ▼" }}</span>
                </div>
                <div v-if="thinkVisible(m)" class="think-body">{{ m.reasoning }}</div>
              </div>
              <div class="bubble">
                <template v-for="(seg, si) in renderMentions(m.content)" :key="si">
                  <span v-if="seg.mention" class="mention">@{{ seg.text }}</span>
                  <template v-else>{{ seg.text }}</template>
                </template>
                <span v-if="m.streaming" class="cursor">▍</span>
              </div>
            </template>
          </div>
        </div>

        <div class="input-row">
          <div class="draft-wrap">
            <el-input
              ref="draftEl"
              v-model="draft"
              class="draft"
              type="textarea"
              :rows="1"
              resize="none"
              placeholder="输入消息，@ 选择 AI，Enter 发送 / Shift+Enter 换行"
              @keydown="onKey"
              @input="onDraftInput"
            />
            <!-- @ 自动补全 -->
            <ul v-if="mentionOpen && mentionOptions.length" class="mention-pop">
              <li
                v-for="(opt, i) in mentionOptions"
                :key="opt.id"
                :class="{ hl: i === mentionIndex }"
                @mousedown.prevent="pickMention(opt)"
              >
                <span class="m-name">@{{ opt.name }}</span>
                <el-tag v-if="opt.is_admin" size="small" type="warning" effect="plain">管理员</el-tag>
              </li>
            </ul>
          </div>
          <div class="btns">
            <el-button type="primary" size="small" :disabled="!draft.trim() || !wsReady" @click="send">
              发送
            </el-button>
          </div>
        </div>
      </section>

      <!-- 右：成员列表（常驻概览） -->
      <aside class="col members">
        <div class="col-head">
          <span class="col-title">AI 成员</span>
          <el-button size="small" text @click="openMemberDialog" :disabled="!currentRoomId">
            管理
          </el-button>
        </div>
        <div class="member-list">
          <div v-for="m in members" :key="m.id" class="member-item">
            <div class="m-top">
              <span class="m-name">{{ m.name }}</span>
              <el-tag v-if="m.is_admin" size="small" type="warning" effect="plain">管理员</el-tag>
            </div>
            <div v-if="m.persona" class="m-persona">{{ m.persona }}</div>
          </div>
          <div v-if="!members.length" class="col-empty">暂无 AI 成员</div>
        </div>
      </aside>
    </div>

    <!-- 新建房间 -->
    <el-dialog v-model="roomDialog" title="新建房间" width="360px">
      <el-form label-width="72px">
        <el-form-item label="名称">
          <el-input v-model="roomForm.name" maxlength="64" placeholder="房间名称" />
        </el-form-item>
        <el-form-item label="可见性">
          <el-select v-model="roomForm.visibility" :disabled="!auth.isAdmin()">
            <el-option label="私有" value="private" />
            <el-option label="公开（仅管理员）" value="public" />
          </el-select>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="roomDialog = false">取消</el-button>
        <el-button type="primary" @click="createRoom">创建</el-button>
      </template>
    </el-dialog>

    <!-- 成员管理 -->
    <el-dialog v-model="memberDialog" title="AI 成员管理" width="520px">
      <div class="member-manage">
        <div v-for="m in members" :key="m.id" class="mm-row">
          <div class="mm-info">
            <span class="mm-name">{{ m.name }}</span>
            <el-tag v-if="m.is_admin" size="small" type="warning" effect="plain">管理员</el-tag>
          </div>
          <div class="mm-ops">
            <el-button size="small" text @click="editMember(m)">编辑</el-button>
            <el-button size="small" text type="danger" @click="removeMember(m)">删除</el-button>
          </div>
        </div>
        <div v-if="!members.length" class="col-empty">还没有 AI 成员，点击下方添加</div>
        <el-divider />
        <el-form label-width="72px">
          <el-form-item label="名称">
            <el-input v-model="memberForm.name" maxlength="32" placeholder="不含空格与 @" />
          </el-form-item>
          <el-form-item label="人设">
            <el-input
              v-model="memberForm.persona"
              type="textarea"
              :rows="2"
              placeholder="角色设定（system prompt）"
            />
          </el-form-item>
          <el-form-item label="管理员">
            <el-switch v-model="memberForm.is_admin" />
          </el-form-item>
          <el-form-item label="模型">
            <el-input v-model="memberForm.model" placeholder="留空=全局配置" />
          </el-form-item>
          <el-form-item label="上游地址">
            <el-input v-model="memberForm.api_base" placeholder="留空=全局配置" />
          </el-form-item>
        </el-form>
      </div>
      <template #footer>
        <el-button v-if="editingId" @click="resetMemberForm">取消编辑</el-button>
        <el-button type="primary" @click="saveMember">
          {{ editingId ? "保存修改" : "添加成员" }}
        </el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from "vue";
import { useRouter } from "vue-router";
import { ElMessage, ElMessageBox } from "element-plus";
import { useAuthStore } from "../stores/auth";
import http from "../api/http";

const router = useRouter();
const auth = useAuthStore();

// ---------- 类型 ----------
interface Room {
  id: number;
  name: string;
  creator_id: number;
  visibility: "private" | "public";
  created: number;
}
interface Member {
  id: number;
  room_id: number;
  name: string;
  persona: string;
  is_admin: boolean;
  model: string;
  api_base: string;
  sort: number;
}
interface Msg {
  key: string;
  id: number;
  sender_type: "user" | "ai" | "system";
  sender_id: number;
  sender_name: string;
  content: string;
  ts: number;
  streaming?: boolean;
  reasoning?: string;
  thinkOpen?: boolean;
}

// ---------- 状态 ----------
const aiConfigured = ref(true);
const rooms = reactive<Room[]>([]);
const members = reactive<Member[]>([]);
const messages = reactive<Msg[]>([]);
const currentRoomId = ref<number | null>(null);
const myId = ref<number | null>(null);
const chainActive = ref(false);

const currentRoom = computed(() => rooms.find((r) => r.id === currentRoomId.value) || null);

// ---------- WebSocket ----------
let ws: WebSocket | null = null;
const wsReady = ref(false);
let wsReconnectTimer: number | null = null;
let manuallyClosed = false;

function wsUrl(room: number) {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const token = localStorage.getItem("panel_token") ?? "";
  return `${proto}://${location.host}/api/ai/ws?room=${room}&token=${encodeURIComponent(token)}`;
}

function connectWs(room: number) {
  closeWs();
  manuallyClosed = false;
  ws = new WebSocket(wsUrl(room));
  ws.onopen = () => (wsReady.value = true);
  ws.onclose = () => {
    wsReady.value = false;
    // 非主动关闭且仍在该房间：延迟重连
    if (!manuallyClosed && currentRoomId.value === room) {
      wsReconnectTimer = window.setTimeout(() => connectWs(room), 2000);
    }
  };
  ws.onerror = () => {
    /* onclose 会随后触发 */
  };
  ws.onmessage = (ev) => handleServerEvent(JSON.parse(ev.data));
}

function closeWs() {
  manuallyClosed = true;
  if (wsReconnectTimer) {
    clearTimeout(wsReconnectTimer);
    wsReconnectTimer = null;
  }
  if (ws) {
    ws.onclose = null;
    ws.close();
    ws = null;
  }
  wsReady.value = false;
}

let keySeq = 0;
const newKey = () => `k${++keySeq}`;

function handleServerEvent(e: any) {
  switch (e.type) {
    case "history": {
      messages.splice(0);
      for (const m of e.messages || []) messages.push(toMsg(m));
      scrollBottom(true);
      break;
    }
    case "msg": {
      // 定稿消息：若该 AI 有流式气泡则替换内容并停止闪烁，否则新增
      const msg = e.message;
      if (msg.sender_type === "ai") {
        const live = messages.find(
          (x) => x.sender_type === "ai" && x.sender_id === msg.sender_id && x.streaming,
        );
        if (live) {
          live.content = msg.content;
          if (msg.reasoning) live.reasoning = msg.reasoning;
          live.streaming = false;
          live.id = msg.id;
          live.ts = msg.ts;
          scrollBottom();
          break;
        }
      }
      messages.push(toMsg(msg));
      scrollBottom();
      break;
    }
    case "ai_start": {
      chainActive.value = true;
      // 为该 AI 建一个流式占位气泡（若已存在同 AI 的流式气泡则复用）
      let live = messages.find(
        (x) => x.sender_type === "ai" && x.sender_id === e.member_id && x.streaming,
      );
      if (!live) {
        live = {
          key: newKey(),
          id: 0,
          sender_type: "ai",
          sender_id: e.member_id,
          sender_name: e.member_name,
          content: "",
          ts: Date.now(),
          streaming: true,
        };
        messages.push(live);
      }
      scrollBottom();
      break;
    }
    case "ai_delta": {
      const live = messages.find(
        (x) => x.sender_type === "ai" && x.sender_id === e.member_id && x.streaming,
      );
      if (live) {
        if (e.kind === "reasoning") live.reasoning = (live.reasoning || "") + e.delta;
        else live.content += e.delta;
        scrollBottom();
      }
      break;
    }
    case "chain_stopped":
    case "chain_busy": {
      if (e.type === "chain_stopped") chainActive.value = false;
      if (e.note) ElMessage.info(e.note);
      break;
    }
    default:
      break;
  }
}

function toMsg(m: any): Msg {
  return {
    key: newKey(),
    id: m.id,
    sender_type: m.sender_type,
    sender_id: m.sender_id,
    sender_name: m.sender_name,
    content: m.content,
    ts: m.ts,
    reasoning: m.reasoning || undefined,
  };
}

// 深度思考块的可见性：流式期间自动展开；定稿后由用户折叠状态决定（默认收起）
function thinkVisible(m: Msg): boolean {
  if (!m.reasoning) return false;
  return m.streaming ? true : !!m.thinkOpen;
}

// ---------- 房间 ----------
async function loadRooms() {
  const { data } = await http.get("/ai/rooms");
  rooms.splice(0, rooms.length, ...data);
}

async function ensureDefaultRoom() {
  // 首次进入自动建默认单 AI 房间
  const { data } = await http.post("/ai/rooms/default");
  return data as Room;
}

async function selectRoom(id: number) {
  if (currentRoomId.value === id) return;
  currentRoomId.value = id;
  messages.splice(0);
  chainActive.value = false;
  await Promise.all([loadMembers(id), loadMessages(id)]);
  connectWs(id);
}

async function loadMessages(id: number) {
  const { data } = await http.get("/ai/messages", { params: { room: id } });
  messages.splice(0, messages.length, ...data.map(toMsg));
  scrollBottom(true);
}

const roomDialog = ref(false);
const roomForm = reactive({ name: "", visibility: "private" as "private" | "public" });
function openRoomDialog() {
  roomForm.name = "";
  roomForm.visibility = auth.isAdmin() ? "private" : "private";
  roomDialog.value = true;
}
async function createRoom() {
  const name = roomForm.name.trim();
  if (!name) return ElMessage.warning("请输入房间名称");
  try {
    const { data } = await http.post("/ai/rooms", { name, visibility: roomForm.visibility });
    roomDialog.value = false;
    await loadRooms();
    await selectRoom(data.id);
  } catch (e: any) {
    ElMessage.error(e?.response?.data?.error || "创建房间失败");
  }
}

// ---------- 成员 ----------
async function loadMembers(roomId: number) {
  const { data } = await http.get("/ai/members", { params: { room: roomId } });
  members.splice(0, members.length, ...data);
}

const memberDialog = ref(false);
const editingId = ref<number | null>(null);
const memberForm = reactive({
  name: "",
  persona: "",
  is_admin: false,
  model: "",
  api_base: "",
});
function resetMemberForm() {
  editingId.value = null;
  memberForm.name = "";
  memberForm.persona = "";
  memberForm.is_admin = false;
  memberForm.model = "";
  memberForm.api_base = "";
}
function openMemberDialog() {
  resetMemberForm();
  memberDialog.value = true;
}
function editMember(m: Member) {
  editingId.value = m.id;
  memberForm.name = m.name;
  memberForm.persona = m.persona;
  memberForm.is_admin = m.is_admin;
  memberForm.model = m.model;
  memberForm.api_base = m.api_base;
}
async function saveMember() {
  if (!currentRoomId.value) return;
  const name = memberForm.name.trim();
  if (!name) return ElMessage.warning("请输入成员名称");
  if (/[\s@]/.test(name)) return ElMessage.warning("成员名称不得包含空格或 @");
  const payload = {
    name,
    persona: memberForm.persona,
    is_admin: memberForm.is_admin,
    model: memberForm.model.trim(),
    api_base: memberForm.api_base.trim(),
  };
  try {
    if (editingId.value) {
      await http.post("/ai/members/update", { id: editingId.value, ...payload });
    } else {
      await http.post("/ai/members", { room_id: currentRoomId.value, ...payload });
    }
    await loadMembers(currentRoomId.value);
    resetMemberForm();
    ElMessage.success("已保存");
  } catch (e: any) {
    ElMessage.error(e?.response?.data?.error || "保存成员失败");
  }
}
async function removeMember(m: Member) {
  try {
    await ElMessageBox.confirm(`确定删除 AI 成员「${m.name}」？`, "删除成员", {
      type: "warning",
    });
  } catch {
    return;
  }
  try {
    await http.post("/ai/members/remove", { id: m.id });
    if (currentRoomId.value) await loadMembers(currentRoomId.value);
    ElMessage.success("已删除");
  } catch (e: any) {
    ElMessage.error(e?.response?.data?.error || "删除成员失败");
  }
}

// ---------- 输入与发送 ----------
const draft = ref("");
const draftEl = ref<any>();

function send() {
  const content = draft.value.trim();
  if (!content || !wsReady.value) return;
  ws?.send(JSON.stringify({ type: "user_msg", content }));
  draft.value = "";
  mentionOpen.value = false;
}

function stopChain() {
  ws?.send(JSON.stringify({ type: "stop" }));
}

// ---------- @ 自动补全 ----------
const mentionOpen = ref(false);
const mentionIndex = ref(0);
const mentionStart = ref(-1); // draft 中 '@' 的字符下标
const mentionOptions = computed(() => {
  if (mentionStart.value < 0) return members.slice(0, 8);
  const frag = draft.value.slice(mentionStart.value + 1).toLowerCase();
  return members.filter((m) => m.name.toLowerCase().startsWith(frag)).slice(0, 8);
});

function onDraftInput() {
  const el = draftEl.value?.$el?.querySelector("textarea") as HTMLTextAreaElement | undefined;
  const caret = el ? el.selectionStart : draft.value.length;
  // 光标前最近的 '@'，且 '@' 到光标之间无空白 → 处于补全态
  const before = draft.value.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at >= 0 && !/\s/.test(before.slice(at + 1))) {
    mentionStart.value = at;
    mentionOpen.value = members.length > 0;
    mentionIndex.value = 0;
  } else {
    mentionOpen.value = false;
    mentionStart.value = -1;
  }
}

function pickMention(opt: Member) {
  const el = draftEl.value?.$el?.querySelector("textarea") as HTMLTextAreaElement | undefined;
  const caret = el ? el.selectionStart : draft.value.length;
  const before = draft.value.slice(0, mentionStart.value);
  const after = draft.value.slice(caret);
  draft.value = `${before}@${opt.name} ${after}`;
  mentionOpen.value = false;
  mentionStart.value = -1;
  nextTick(() => {
    if (el) {
      const pos = before.length + opt.name.length + 2;
      el.focus();
      el.setSelectionRange(pos, pos);
    }
  });
}

function onKey(e: KeyboardEvent) {
  if (mentionOpen.value && mentionOptions.value.length) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      mentionIndex.value = (mentionIndex.value + 1) % mentionOptions.value.length;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      mentionIndex.value =
        (mentionIndex.value - 1 + mentionOptions.value.length) % mentionOptions.value.length;
      return;
    }
    if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      pickMention(mentionOptions.value[mentionIndex.value]);
      return;
    }
    if (e.key === "Escape") {
      mentionOpen.value = false;
      return;
    }
  }
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    send();
  }
}

// ---------- @ 高亮 ----------
// 把消息文本按房间内成员名切段：@名字（与后端 parse_mentions 同口径：
// '@' 后连续非空白非 '@' 字符，全名不中时逐次剥离末尾标点）标为 mention 段。
const TRAILING_PUNCT = new Set([
  ",", ".", ";", ":", "!", "?", '"', "'", ")", "]", "}",
  "，", "。", "、", "；", "：", "！", "？", "）", "】", "」", "』",
]);
function renderMentions(text: string): { text: string; mention: boolean }[] {
  if (!text) return [];
  if (!members.length) return [{ text, mention: false }];
  const segs: { text: string; mention: boolean }[] = [];
  const chars = [...text];
  let plain = "";
  let i = 0;
  while (i < chars.length) {
    if (chars[i] === "@") {
      let j = i + 1;
      while (j < chars.length && !/\s/.test(chars[j]) && chars[j] !== "@") j++;
      let cand = chars.slice(i + 1, j).join("");
      let hit: string | null = null;
      while (cand) {
        if (members.some((m) => m.name === cand)) {
          hit = cand;
          break;
        }
        const last = cand[cand.length - 1];
        if (!TRAILING_PUNCT.has(last)) break;
        cand = cand.slice(0, -1);
      }
      if (hit) {
        if (plain) {
          segs.push({ text: plain, mention: false });
          plain = "";
        }
        segs.push({ text: hit, mention: true });
        i = i + 1 + hit.length;
        continue;
      }
    }
    plain += chars[i];
    i++;
  }
  if (plain) segs.push({ text: plain, mention: false });
  return segs;
}

// ---------- 滚动跟随 ----------
const msgsEl = ref<HTMLElement>();
const stickBottom = ref(true);
const NEAR_BOTTOM = 24;
function onScroll() {
  const el = msgsEl.value;
  if (!el) return;
  stickBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < NEAR_BOTTOM;
}
function scrollBottom(force = false) {
  if (!stickBottom.value && !force) return;
  nextTick(() => {
    const el = msgsEl.value;
    if (el) el.scrollTop = el.scrollHeight;
  });
}

// ---------- 生命周期 ----------
onMounted(async () => {
  try {
    const { data } = await http.get("/me");
    myId.value = data.id ?? null;
  } catch {
    /* 角色已由 store 提供，id 取不到不影响渲染 */
  }
  try {
    const resp = await http.get("/ai/config", { timeout: 5000 });
    aiConfigured.value = !!resp.data?.config?.configured;
  } catch {
    /* 保持默认 true */
  }
  await loadRooms();
  if (!rooms.length) {
    const r = await ensureDefaultRoom();
    await loadRooms();
    currentRoomId.value = r.id;
    await Promise.all([loadMembers(r.id), loadMessages(r.id)]);
    connectWs(r.id);
  } else {
    await selectRoom(rooms[0].id);
  }
});

onBeforeUnmount(() => closeWs());
</script>

<style scoped>
.ai-group {
  display: flex;
  flex-direction: column;
  height: calc(100vh - var(--topbar-h, 52px) - 40px);
}
.cfg-tip {
  flex: none;
  margin-bottom: 12px;
}
.cols {
  flex: 1;
  min-height: 0;
  display: flex;
  gap: 12px;
}
.col {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-radius: var(--radius);
  background: var(--el-bg-color);
  border: 1px solid var(--el-border-color-lighter);
}
.col-head {
  flex: none;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.col-title {
  font-size: 13px;
  font-weight: 600;
}
.spacer {
  flex: 1;
}
.rooms {
  width: 200px;
  flex: none;
}
.room-list,
.member-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 6px;
}
.room-item {
  padding: 8px 10px;
  border-radius: var(--radius);
  cursor: pointer;
}
.room-item:hover {
  background: var(--el-fill-color-light);
}
.room-item.active {
  background: var(--el-color-primary-light-9);
}
.room-name {
  font-size: 13px;
  margin-bottom: 4px;
}
.col-empty {
  padding: 16px;
  text-align: center;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.chat {
  flex: 1;
  min-width: 0;
}
.msgs {
  flex: 1;
  min-height: 0;
  overflow: auto;
  display: flex;
  flex-direction: column;
  gap: 12px;
  padding: 16px;
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
  flex-direction: column;
  align-items: flex-start;
}
.msg.mine {
  align-items: flex-end;
}
.who {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  margin-bottom: 2px;
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
/* @ 高亮：主题色加粗；用户自己的蓝色气泡内改用白色 */
.bubble .mention {
  color: var(--el-color-primary);
  font-weight: 600;
}
.msg.user .bubble .mention {
  color: #fff;
  text-decoration: underline;
  text-underline-offset: 2px;
}
/* 深度思考折叠块 */
.think {
  max-width: 78%;
  margin-bottom: 4px;
  border: 1px solid var(--el-border-color-lighter);
  border-radius: var(--radius);
  background: var(--el-fill-color-lighter);
  overflow: hidden;
}
.think-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 4px 10px;
  font-size: 12px;
  cursor: pointer;
  user-select: none;
  color: var(--el-text-color-secondary);
}
.think-head:hover {
  background: var(--el-fill-color-light);
}
.think-tag {
  font-weight: 600;
  color: var(--el-color-info);
}
.think-arrow {
  font-size: 11px;
}
.think-body {
  padding: 8px 10px;
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--el-text-color-secondary);
  border-top: 1px dashed var(--el-border-color-lighter);
  max-height: 260px;
  overflow: auto;
}
.sys-note {
  align-self: center;
  font-size: 12px;
  color: var(--el-text-color-secondary);
  background: var(--el-fill-color-lighter);
  padding: 2px 10px;
  border-radius: 10px;
}
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
  padding: 12px;
}
.draft-wrap {
  flex: 1;
  position: relative;
}
.draft :deep(.el-textarea__inner) {
  padding: 6px 10px;
  line-height: 1.5;
}
.mention-pop {
  position: absolute;
  bottom: calc(100% + 4px);
  left: 0;
  right: 0;
  max-height: 200px;
  overflow: auto;
  margin: 0;
  padding: 4px;
  list-style: none;
  background: var(--el-bg-color-overlay);
  border: 1px solid var(--el-border-color);
  border-radius: var(--radius);
  box-shadow: var(--el-box-shadow-light);
  z-index: 10;
}
.mention-pop li {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  font-size: 13px;
  border-radius: var(--radius);
  cursor: pointer;
}
.mention-pop li.hl,
.mention-pop li:hover {
  background: var(--el-color-primary-light-9);
}
.m-name {
  font-weight: 600;
}
.btns {
  display: flex;
}
.btns :deep(.el-button) {
  height: 33px;
}
.members {
  width: 220px;
  flex: none;
}
.member-item {
  padding: 8px 10px;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.member-item:last-child {
  border-bottom: none;
}
.m-top {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 2px;
}
.member-item .m-name {
  font-size: 13px;
  font-weight: 600;
}
.m-persona {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  line-height: 1.4;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.member-manage {
  max-height: 50vh;
  overflow: auto;
}
.mm-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 0;
  border-bottom: 1px solid var(--el-border-color-lighter);
}
.mm-info {
  display: flex;
  align-items: center;
  gap: 6px;
}
.mm-name {
  font-size: 13px;
}
</style>
