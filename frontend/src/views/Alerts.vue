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
        <el-table-column label="指标" v-bind="col(120, true)">
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

    <div class="block block-probes">
      <div class="probe-head">
        <div class="section-title">站点探针</div>
        <span class="hint">周期探测以下地址，连续失败达阈值才告警；运行态只存内存，重启后重新探测</span>
        <el-button size="small" @click="openProbe(null)">新增探针</el-button>
      </div>
      <el-table v-loading="probesLoading" :data="probeRows" size="small">
        <el-table-column label="状态" v-bind="col(90)">
          <template #default="{ row }">
            <span class="dot-wrap">
              <i class="dot" :class="probeState(row).cls" />
              {{ probeState(row).text }}
            </span>
          </template>
        </el-table-column>
        <el-table-column label="名称" v-bind="col(140)" prop="name" />
        <el-table-column label="地址" v-bind="col(240, true)">
          <template #default="{ row }">
            <span class="mono">{{ row.url }}</span>
          </template>
        </el-table-column>
        <el-table-column label="最近检查" v-bind="col(180)">
          <template #default="{ row }">
            {{ row.status?.last_check ? fmtTime(row.status.last_check) : "—" }}
          </template>
        </el-table-column>
        <el-table-column label="耗时" v-bind="col(90)" v-if="!hideColP3">
          <template #default="{ row }">
            {{ row.status?.last_ms ? row.status.last_ms + "ms" : "—" }}
          </template>
        </el-table-column>
        <el-table-column label="失败次数" v-bind="col(100)" v-if="!hideColP3">
          <template #default="{ row }">{{ row.status?.failures_total ?? 0 }}</template>
        </el-table-column>
        <el-table-column label="操作" v-bind="col(190)" align="right">
          <template #default="{ $index }">
            <el-button link size="small" @click="openProbe($index)">编辑</el-button>
            <el-button
              link
              size="small"
              :loading="probeTesting === $index"
              @click="testProbe($index)"
            >
              测试
            </el-button>
            <el-button link size="small" @click="removeProbe($index)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
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

    <el-dialog
      v-model="showProbe"
      :title="probeIndex === null ? '新增探针' : '编辑探针'"
      width="480px"
    >
      <el-form label-width="120px" size="small">
        <el-form-item label="名称">
          <el-input v-model="probeForm.name" placeholder="如：官网首页" />
        </el-form-item>
        <el-form-item label="地址">
          <el-input v-model="probeForm.url" placeholder="https://example.com/" />
        </el-form-item>
        <el-form-item label="启用">
          <el-switch v-model="probeForm.enabled" />
        </el-form-item>
        <el-form-item label="期望状态码">
          <el-input-number
            v-model="probeForm.expect_status"
            :min="0"
            :max="599"
            style="width: 100%"
          />
        </el-form-item>
        <el-form-item label="正文关键字">
          <el-input v-model="probeForm.keyword" placeholder="可留空；填了则要求正文包含它" />
        </el-form-item>
        <el-form-item label="连续失败次数">
          <el-input-number
            v-model="probeForm.fail_threshold"
            :min="1"
            :max="10"
            style="width: 100%"
          />
        </el-form-item>
        <el-form-item label="超时（秒）">
          <el-input-number v-model="probeForm.timeout_s" :min="1" :max="30" style="width: 100%" />
        </el-form-item>
        <el-form-item label="忽略证书错误">
          <el-switch v-model="probeForm.insecure" />
        </el-form-item>
      </el-form>
      <div class="hint">
        期望状态码填 0 表示任意 2xx 即正常；连续失败达到设定次数才判定宕机并推送，
        避免一次网络抖动就误报。被监控站点用自签证书时勾选「忽略证书错误」。
      </div>
      <template #footer>
        <el-button @click="showProbe = false">取消</el-button>
        <el-button :loading="probeFormTesting" @click="testProbeForm">测试</el-button>
        <el-button @click="saveProbe">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP3 } from "../composables/useResponsive";
import { computed, onMounted, onUnmounted, reactive, ref } from "vue";
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

/** 站点探针的配置项 */
interface ProbeTarget {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  /** 期望状态码：0 = 任意 2xx */
  expect_status: number;
  keyword: string;
  fail_threshold: number;
  timeout_s: number;
  insecure: boolean;
}

/** 站点探针的运行态（配置 + 探测结果合并给前端） */
interface ProbeStatus {
  id: string;
  last_status: number;
  last_ms: number;
  last_error: string;
  last_check: number;
  down: boolean;
  failures: number;
  failures_total: number;
  checks: number;
}

/** 表格行 = 配置 + 对应运行态（未探测过则 status 为空） */
type ProbeRow = ProbeTarget & { status: ProbeStatus | null };

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

const targets = ref<ProbeTarget[]>([]);
const statusById = ref<Record<string, ProbeStatus>>({});
const probesLoading = ref(false);
const showProbe = ref(false);
const probeIndex = ref<number | null>(null);
const probeTesting = ref<number | null>(null);
const probeFormTesting = ref(false);
const probeForm = reactive<ProbeTarget>(emptyProbe());

/** 表格数据 = 配置数组 join 运行态；顺序与 targets 一致，编辑按下标回取 */
const probeRows = computed<ProbeRow[]>(() =>
  targets.value.map((t) => ({ ...t, status: statusById.value[t.id] ?? null })),
);

/** 生成探针 id：32 位十六进制随机串，满足后端「非空、≤64、仅字母数字/-/_」的校验。
 *
 * 🔴 不能用 crypto.randomUUID：它只在**安全上下文**（HTTPS 或 localhost）下存在，
 * 面板默认走明文 HTTP，此时 window.isSecureContext 为 false、`crypto.randomUUID`
 * 直接是 undefined。而它在 setup 顶层经 emptyProbe() 被调到，一抛错整个组件
 * 渲染就中断 —— 表现是「告警通知」页点进去内容区一片空白，连根节点都没挂上
 * （router-view 只剩一个注释占位），看起来像页面挂了，其实别的页面都正常。
 * getRandomValues 不受安全上下文限制，用它自己拼一串即可。 */
function newProbeId(): string {
  const buf = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(buf, (b) => b.toString(16).padStart(2, "0")).join("");
}

function emptyProbe(): ProbeTarget {
  return {
    id: newProbeId(),
    name: "",
    url: "",
    enabled: true,
    expect_status: 0,
    keyword: "",
    fail_threshold: 2,
    timeout_s: 10,
    insecure: false,
  };
}

/** 状态点：从未探测 = 未知；宕机 = 实心，其余空心（沿用黑白灰，与事件表同款） */
function probeState(row: ProbeRow) {
  if (!row.status?.last_check) return { text: "未知", cls: "dot-off" };
  return row.status.down ? { text: "宕机", cls: "dot-on" } : { text: "正常", cls: "dot-off" };
}

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
  loadProbes();
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

async function loadProbes() {
  probesLoading.value = true;
  try {
    const { data } = await http.get("/alerts/probes");
    targets.value = data.targets ?? [];
    await refreshProbeStatus();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取站点探针失败");
  } finally {
    probesLoading.value = false;
  }
}

/** 只刷运行态（轮询用）：不摆 loading，避免每隔几十秒闪一次 */
async function refreshProbeStatus() {
  try {
    const { data } = await http.get("/alerts/probes/status");
    const map: Record<string, ProbeStatus> = {};
    for (const s of data as ProbeStatus[]) map[s.id] = s;
    statusById.value = map;
  } catch {
    // 轮询失败静默：下一轮会补上，不打扰用户
  }
}

function openProbe(index: number | null) {
  probeIndex.value = index;
  if (index === null) {
    Object.assign(probeForm, emptyProbe());
  } else {
    Object.assign(probeForm, targets.value[index]);
  }
  showProbe.value = true;
}

async function saveProbe() {
  const next = [...targets.value];
  const entry: ProbeTarget = { ...probeForm };
  if (probeIndex.value === null) {
    next.push(entry);
  } else {
    next[probeIndex.value] = entry;
  }
  try {
    await http.post("/alerts/probes", { targets: next });
    showProbe.value = false;
    ElMessage.success("已保存，探针立即重探");
    loadProbes();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  }
}

async function removeProbe(index: number) {
  const next = targets.value.filter((_, i) => i !== index);
  try {
    await http.post("/alerts/probes", { targets: next });
    ElMessage.success("已删除");
    loadProbes();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

/** 立即探测一次（不落库、不影响已存状态），用于确认地址是否真的可达 */
async function runProbeTest(target: ProbeTarget) {
  try {
    const { data } = await http.post("/alerts/probes/test", { target });
    if (data.ok) {
      ElMessage.success(`可达：HTTP ${data.status}，${data.ms}ms`);
    } else {
      ElMessage.error(`探测失败：${data.error || "未知原因"}`);
    }
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "探测失败");
  }
}

async function testProbe(index: number) {
  probeTesting.value = index;
  await runProbeTest(targets.value[index]);
  probeTesting.value = null;
}

async function testProbeForm() {
  probeFormTesting.value = true;
  await runProbeTest({ ...probeForm });
  probeFormTesting.value = false;
}

// 探针由后台每 60s 推进一轮，前端 30s 拉一次状态即可
let probeTimer: number | undefined;
onMounted(() => {
  loadAll();
  probeTimer = window.setInterval(refreshProbeStatus, 30000);
});
onUnmounted(() => {
  if (probeTimer !== undefined) window.clearInterval(probeTimer);
});
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
  gap: var(--sp-4);
  min-height: 0;
}
.block-rules {
  flex: 1 1 22%;
  min-height: 88px;
}
.block-probes {
  flex: 1 1 33%;
  min-height: 110px;
}
.block-events {
  flex: 1 1 45%;
  min-height: 132px;
}
/* 区块标题行：标题 + 说明 + 右侧按钮（探针表与规则表样式一致） */
.probe-head {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  flex: none;
  flex-wrap: wrap;
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
  margin-left: var(--sp-1);
}
.section-title {
  font-size: 14px;
  font-weight: 600;
  /* 原来 20px 上下外距，在 flex 列里会和 gap 叠成双倍间距，故上外距归 0；
     下外距按区块标题统一口径取标尺的 --sp-2。 */
  margin: 0 0 var(--sp-2);
  flex: none;
}
.webhook-row {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  flex: none;
  flex-wrap: wrap;
}
.chan-tag {
  cursor: pointer;
}
.dot-wrap {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-2);
}
</style>
