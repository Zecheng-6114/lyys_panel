<template>
  <div class="alerts">
    <div class="toolbar">
      <el-button @click="loadAll">刷新</el-button>
      <el-button @click="openEdit(null)">新增规则</el-button>
      <span class="hint">指标越过阈值触发告警，回落时自动恢复；规则变更一小时内生效</span>
    </div>

    <!-- 规则表与事件表各吃一块剩余高度（不再写死 240px / 100vh 相减），
         这样整页高度 = 内容区高度，页面本身不产生纵向滚动，滚动只发生在表格内部。 -->
    <div class="block block-rules">
      <div class="section-title">告警规则</div>
      <el-table v-loading="loading" :data="rules" size="small">
        <el-table-column label="指标" v-bind="col(120)">
          <template #default="{ row }">{{ metricLabel(row.metric) }}</template>
        </el-table-column>
        <el-table-column label="阈值" v-bind="col(100)">
          <template #default="{ row }">{{ row.threshold }}%</template>
        </el-table-column>
        <el-table-column label="恢复通知" v-bind="col(100)">
          <template #default="{ row }">{{ row.notify_resolve ? "是" : "否" }}</template>
        </el-table-column>
        <el-table-column label="操作" v-bind="col(120)" align="right">
          <template #default="{ $index }">
            <el-button link size="small" @click="openEdit($index)">编辑</el-button>
            <el-button link size="small" @click="removeRule($index)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <div class="section-title">通知渠道</div>
    <div class="webhook-row">
      <el-tag
        v-for="(c, i) in channels"
        :key="i"
        class="chan-tag"
        size="small"
        closable
        @close="removeChannel(i)"
        @click="openChannel(i)"
      >
        {{ kindLabel(c.kind) }} · {{ channelTarget(c) }}
      </el-tag>
      <span v-if="!channels.length" class="hint">未配置渠道：告警只记录事件，不推送</span>
      <el-button size="small" @click="openChannel(null)">新增渠道</el-button>
    </div>

    <div class="block block-events">
      <div class="section-title">最近告警事件</div>
      <el-table v-loading="eventsLoading" :data="events" size="small">
        <el-table-column label="时间" v-bind="col(180)">
          <template #default="{ row }">{{ fmtTime(row.ts) }}</template>
        </el-table-column>
        <el-table-column label="指标" v-bind="col(100)" prop="metric" />
        <el-table-column label="当前值" v-bind="col(100)">
          <template #default="{ row }">{{ row.value.toFixed(1) }}%</template>
        </el-table-column>
        <el-table-column label="阈值" v-bind="col(100)" v-if="!hideColP3">
          <template #default="{ row }">{{ row.threshold.toFixed(0) }}%</template>
        </el-table-column>
        <el-table-column label="状态" v-bind="col(120, true)">
          <template #default="{ row }">
            <span class="dot-wrap">
              <i class="dot" :class="row.state === 'firing' ? 'dot-on' : 'dot-off'" />
              {{ row.state === "firing" ? "触发" : "恢复" }}
            </span>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <el-dialog v-model="showEdit" :title="editIndex === null ? '新增规则' : '编辑规则'" width="400px">
      <el-form label-width="80px" size="small">
        <el-form-item label="指标">
          <el-select v-model="form.metric" style="width: 100%">
            <el-option label="CPU 使用率" value="cpu" />
            <el-option label="内存使用率" value="mem" />
            <el-option label="磁盘使用率" value="disk" />
          </el-select>
        </el-form-item>
        <el-form-item label="阈值 %">
          <el-input-number v-model="form.threshold" :min="1" :max="99" style="width: 100%" />
        </el-form-item>
        <el-form-item label="恢复通知">
          <el-switch v-model="form.notify_resolve" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button @click="saveRule">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog
      v-model="showChannel"
      :title="channelIndex === null ? '新增渠道' : '编辑渠道'"
      width="460px"
    >
      <el-form label-width="100px" size="small">
        <el-form-item label="渠道类型">
          <el-select v-model="chForm.kind" style="width: 100%">
            <el-option
              v-for="k in CHANNEL_KINDS"
              :key="k.value"
              :label="k.label"
              :value="k.value"
            />
          </el-select>
        </el-form-item>
        <template v-if="chForm.kind === 'telegram'">
          <el-form-item label="Bot Token">
            <el-input v-model="chForm.token" placeholder="123456:ABC-DEF…" />
          </el-form-item>
          <el-form-item label="会话 ID">
            <el-input v-model="chForm.chat_id" placeholder="私聊/群为数字，频道可填 @name" />
          </el-form-item>
        </template>
        <el-form-item v-else label="Webhook 地址">
          <el-input v-model="chForm.url" placeholder="https://…" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="showChannel = false">取消</el-button>
        <el-button :loading="testing" @click="testChannel">测试发送</el-button>
        <el-button @click="saveChannel">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP3 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";

interface AlertRule {
  metric: "cpu" | "mem" | "disk";
  threshold: number;
  notify_resolve: boolean;
}

interface AlertEvent {
  ts: number;
  metric: string;
  value: number;
  threshold: number;
  state: string;
}

type ChannelKind = "webhook" | "dingtalk" | "wecom" | "feishu" | "telegram";

interface AlertChannel {
  kind: ChannelKind;
  url: string;
  token: string;
  chat_id: string;
}

const CHANNEL_KINDS: { value: ChannelKind; label: string }[] = [
  { value: "webhook", label: "通用 Webhook" },
  { value: "dingtalk", label: "钉钉机器人" },
  { value: "wecom", label: "企业微信机器人" },
  { value: "feishu", label: "飞书机器人" },
  { value: "telegram", label: "Telegram" },
];

const rules = ref<AlertRule[]>([]);
const events = ref<AlertEvent[]>([]);
const channels = ref<AlertChannel[]>([]);
const loading = ref(false);
const eventsLoading = ref(false);
const showEdit = ref(false);
const editIndex = ref<number | null>(null);
const form = reactive<AlertRule>({ metric: "cpu", threshold: 90, notify_resolve: false });
const showChannel = ref(false);
const channelIndex = ref<number | null>(null);
const chForm = reactive<AlertChannel>({ kind: "webhook", url: "", token: "", chat_id: "" });
const testing = ref(false);

function metricLabel(m: string) {
  return m === "cpu" ? "CPU" : m === "mem" ? "内存" : "磁盘";
}

function kindLabel(kind: ChannelKind) {
  return CHANNEL_KINDS.find((k) => k.value === kind)?.label ?? kind;
}

function channelTarget(c: AlertChannel) {
  if (c.kind === "telegram") return c.chat_id || "未填会话";
  const url = c.url || "未填地址";
  return url.length > 48 ? url.slice(0, 48) + "…" : url;
}

function fmtTime(ts: number) {
  return new Date(ts * 1000).toLocaleString();
}

async function loadRules() {
  loading.value = true;
  try {
    const { data } = await http.get("/alerts/rules");
    rules.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取规则失败");
  } finally {
    loading.value = false;
  }
}

async function loadChannels() {
  try {
    const { data } = await http.get("/alerts/channels");
    channels.value = data.channels ?? [];
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取通知渠道失败");
  }
}

async function loadEvents() {
  eventsLoading.value = true;
  try {
    const { data } = await http.get("/alerts/events", { params: { limit: 100 } });
    events.value = data;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取告警事件失败");
  } finally {
    eventsLoading.value = false;
  }
}

function loadAll() {
  loadRules();
  loadChannels();
  loadEvents();
}

function openEdit(index: number | null) {
  editIndex.value = index;
  if (index === null) {
    Object.assign(form, { metric: "cpu", threshold: 90, notify_resolve: false });
  } else {
    Object.assign(form, rules.value[index]);
  }
  showEdit.value = true;
}

async function saveRule() {
  const next = [...rules.value];
  if (editIndex.value === null) {
    next.push({ ...form });
  } else {
    next[editIndex.value] = { ...form };
  }
  try {
    await http.post("/alerts/rules", next);
    showEdit.value = false;
    ElMessage.success("已保存");
    loadRules();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function removeRule(index: number) {
  const next = rules.value.filter((_, i) => i !== index);
  try {
    await http.post("/alerts/rules", next);
    ElMessage.success("已删除");
    loadRules();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

function openChannel(index: number | null) {
  channelIndex.value = index;
  if (index === null) {
    Object.assign(chForm, { kind: "webhook", url: "", token: "", chat_id: "" });
  } else {
    Object.assign(chForm, channels.value[index]);
  }
  showChannel.value = true;
}

async function saveChannel() {
  const next = [...channels.value];
  const entry: AlertChannel = { ...chForm };
  if (channelIndex.value === null) {
    next.push(entry);
  } else {
    next[channelIndex.value] = entry;
  }
  try {
    await http.post("/alerts/channels", { channels: next });
    showChannel.value = false;
    ElMessage.success("已保存");
    loadChannels();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function testChannel() {
  testing.value = true;
  try {
    await http.post("/alerts/channels/test", { channel: { ...chForm } });
    ElMessage.success("测试消息已发送，请到对应渠道确认");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "发送失败");
  } finally {
    testing.value = false;
  }
}

async function removeChannel(index: number) {
  const next = channels.value.filter((_, i) => i !== index);
  try {
    await http.post("/alerts/channels", { channels: next });
    ElMessage.success("已删除");
    loadChannels();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

onMounted(loadAll);
</script>

<style scoped>
/* 整根撑满内容区，纵向不再靠「页面滚动」消化溢出 —— 溢出交给表格自己的 body 区。
   高度基准来自 .content（它已经是视口 - 顶栏的确定高度），这里只做 100% 继承。 */
.alerts {
  height: 100%;
  display: flex;
  flex-direction: column;
}
/* 两块表格区各自吃掉一部分剩余高度。
   min-height 是矮屏兜底：视口过矮时保住表格可读性，宁可让 .content 出现内部滚动，
   也不把两行缩成一条线。 */
.block {
  display: flex;
  flex-direction: column;
  gap: var(--sp-2);
  min-height: 0;
}
.block-rules {
  flex: 1 1 30%;
  min-height: 96px;
}
.block-events {
  flex: 1 1 70%;
  min-height: 144px;
}
/* EP 表格默认是「内容多高就多高」（.el-table{height:fit-content}），
   这里把它拉成 flex 子项填满 .block，表头固定、body 区滚动。 */
.block :deep(.el-table) {
  flex: 1;
  min-height: 0;
  height: auto;
  display: flex;
  flex-direction: column;
}
.block :deep(.el-table__body-wrapper) {
  flex: 1;
  min-height: 0;
  /* EP 默认 overflow:hidden（靠 height prop 的场景由它自己处理滚动）；
     这里 height 交给 flex，滚动就得由这条规则兜住。 */
  overflow: auto;
}

.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-left: 4px;
}
.section-title {
  font-size: 14px;
  font-weight: 500;
  /* 原来 20px 上下外距，在 flex 列里会和 gap 叠成双倍间距 */
  margin: 0;
  flex: none;
}
.webhook-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex: none;
  flex-wrap: wrap;
}
.chan-tag {
  cursor: pointer;
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
</style>
