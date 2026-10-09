<template>
  <div class="tuning">
    <div class="toolbar">
      <span class="hint">
        内核 {{ status?.kernel || "—" }} · 调优直接作用于本机，改动前会记录原值并给出回滚命令
      </span>
      <div class="spacer" />
      <button class="mini-btn" type="button" :disabled="loading" @click="load">
        {{ loading ? "读取中…" : "刷新" }}
      </button>
    </div>

    <!-- 能力提示：不能写的时候必须说清原因，否则用户会以为是参数填错了 -->
    <div v-if="status && !status.capability.can_write" class="notice notice--warn">
      <strong>当前环境无法写入内核参数。</strong>
      <span>{{ status.capability.reason }}</span>
    </div>
    <div v-else-if="status?.capability.container" class="notice">
      <strong>检测到容器环境。</strong>
      <span>{{ status.capability.reason }}</span>
    </div>

    <!-- 内核参数 -->
    <section v-for="g in status?.params ?? []" :key="g.group" class="panel">
      <h3 class="panel-title">{{ g.group }}</h3>
      <div class="rows">
        <div v-for="p in g.items" :key="p.key" class="row">
          <div class="row-head">
            <span class="row-title">{{ p.title }}</span>
            <code class="row-key">{{ p.key }}</code>
            <span class="row-cur">当前 <b>{{ p.value }}</b></span>
          </div>
          <p class="row-effect">{{ p.effect }}</p>
          <div class="row-edit">
            <el-select
              v-if="p.choices.length"
              v-model="draft[p.key]"
              size="small"
              class="edit-input"
            >
              <el-option v-for="c in p.choices" :key="c" :label="c" :value="c" />
            </el-select>
            <el-input
              v-else
              v-model="draft[p.key]"
              size="small"
              class="edit-input"
              :placeholder="`${p.range?.[0]}–${p.range?.[1]}`"
            />
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="busyKey === p.key || draft[p.key] === p.value"
              @click="applyParam(p)"
            >
              {{ busyKey === p.key ? "提交中…" : "应用" }}
            </button>
            <span v-if="draft[p.key] === p.value" class="row-tip">与当前值相同</span>
          </div>
        </div>
      </div>
    </section>

    <!-- 交换区 -->
    <section v-if="status" class="panel">
      <h3 class="panel-title">交换区（Swap）</h3>
      <div class="rows">
        <div class="row">
          <div class="row-head">
            <span class="row-title">当前交换区</span>
            <span class="row-cur">
              合计 <b>{{ fmtKb(status.swap.total_kb) }}</b>
              <template v-if="status.swap.total_kb > 0">
                ，已用 {{ fmtKb(status.swap.used_kb) }}（{{
                  Math.round((status.swap.used_kb / status.swap.total_kb) * 100)
                }}%）
              </template>
            </span>
          </div>
          <ul v-if="status.swap.entries.length" class="swap-list">
            <li v-for="e in status.swap.entries" :key="e.name">
              <code>{{ e.name }}</code>
              <span class="dim">{{ e.kind === "file" ? "交换文件" : "交换分区" }}</span>
              <span>{{ fmtKb(e.size_kb) }}</span>
              <span class="dim">优先级 {{ e.priority }}</span>
            </li>
          </ul>
          <p v-else class="row-effect">当前没有启用任何交换区。</p>
          <p class="row-effect">
            压缩内存：zram {{ status.swap.zram ? "已启用" : "未启用" }} ·
            zswap {{ status.swap.zswap ? "已启用" : "未启用" }}
          </p>
        </div>

        <div class="row">
          <div class="row-head">
            <span class="row-title">创建交换文件</span>
            <span class="row-cur">
              文件路径 <code>{{ swapPath }}</code>，大小 {{ swapSizeMb }} MB
            </span>
          </div>
          <p class="row-effect">
            在根目录下创建一个交换文件并写入 <code>/etc/fstab</code>（原文件会先备份）。
            已有的交换分区不会被停用 —— 两者可以共存，优先级由各自决定。
            上限 {{ SWAP_MAX_MB }} MB。
          </p>
          <div class="row-edit">
            <el-input v-model="swapPath" size="small" class="edit-input edit-input--path" />
            <el-input-number
              v-model="swapSizeMb"
              :min="64"
              :max="SWAP_MAX_MB"
              :step="512"
              size="small"
            />
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="busyAction !== '' || status.swap.has_file"
              @click="applySwap('enable')"
            >
              {{ busyAction === "swap-enable" ? "提交中…" : "创建并启用" }}
            </button>
            <span v-if="status.swap.has_file" class="row-tip">已有交换文件</span>
          </div>
        </div>
      </div>
    </section>

    <!-- 时间同步 -->
    <section v-if="status" class="panel">
      <h3 class="panel-title">时间同步（NTP）</h3>
      <div class="rows">
        <div class="row">
          <div class="row-head">
            <span class="row-title">当前状态</span>
            <span class="row-cur">
              守护进程 <b>{{ status.ntp.daemon }}</b>
              <template v-if="status.ntp.enabled !== null">
                · NTP {{ status.ntp.enabled ? "已启用" : "未启用" }}
              </template>
              <template v-if="status.ntp.synchronized !== null">
                · {{ status.ntp.synchronized ? "已同步" : "未同步" }}
              </template>
              <template v-if="status.ntp.timezone"> · 时区 {{ status.ntp.timezone }}</template>
            </span>
          </div>
          <p class="row-effect">
            配置文件：<code>{{ status.ntp.config_path || "（未找到）" }}</code>
          </p>
          <ul v-if="status.ntp.servers.length" class="swap-list">
            <li v-for="s in status.ntp.servers" :key="s"><code>{{ s }}</code></li>
          </ul>
        </div>

        <div class="row">
          <div class="row-head">
            <span class="row-title">时间服务器</span>
            <span class="row-cur">逗号分隔，可填多个</span>
          </div>
          <p class="row-effect">
            重写配置文件里的 server / pool 行（其它指令保留），随后重启时间同步服务。
            原配置会备份为 <code>*.lyys-panel.bak</code>。
            系统未安装时间同步守护进程时此操作会被拒绝。
          </p>
          <div class="row-edit">
            <el-input
              v-model="ntpServers"
              size="small"
              class="edit-input edit-input--path"
              placeholder="ntp.aliyun.com,time.cloudflare.com"
            />
            <button
              class="mini-btn mini-btn--sm"
              type="button"
              :disabled="busyAction !== '' || !status.ntp.config_path"
              @click="applyNtp"
            >
              {{ busyAction === "ntp" ? "提交中…" : "应用" }}
            </button>
          </div>
        </div>
      </div>
    </section>

    <p v-if="!status && !loading" class="empty">读取失败，请刷新重试。</p>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import http from "../api/http";

/// 一个可调内核参数（与后端 tuning::ParamSpec 对应）
interface ParamSpec {
  key: string;
  group: string;
  title: string;
  value: string;
  choices: string[];
  range: [number, number] | null;
  effect: string;
  live: boolean;
}
interface ParamGroup {
  group: string;
  items: ParamSpec[];
}
interface SwapEntry {
  name: string;
  kind: string;
  size_kb: number;
  used_kb: number;
  priority: string;
}
interface Status {
  params: ParamGroup[];
  swap: {
    entries: SwapEntry[];
    total_kb: number;
    used_kb: number;
    has_file: boolean;
    zram: boolean;
    zswap: boolean;
  };
  ntp: {
    daemon: string;
    enabled: boolean | null;
    synchronized: boolean | null;
    timezone: string;
    servers: string[];
    config_path: string | null;
  };
  capability: {
    root: boolean;
    container: boolean;
    proc_sys_writable: boolean;
    systemd: boolean;
    can_write: boolean;
    reason: string;
  };
  swappiness: string | null;
  mem_total_kb: number;
  kernel: string;
}

/// 与后端 tuning.rs 的 SWAP_MAX_MB 同源
const SWAP_MAX_MB = 8192;

const status = ref<Status | null>(null);
const loading = ref(false);
/// 每个参数的编辑态：key → 期望值。初值取当前值，避免空输入框
const draft = ref<Record<string, string>>({});
const busyKey = ref("");
const busyAction = ref("");
const swapPath = ref("/swapfile");
const swapSizeMb = ref(2048);
const ntpServers = ref("");

function fmtKb(kb: number) {
  if (kb <= 0) return "0";
  const units = ["KB", "MB", "GB", "TB"];
  let v = kb;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : 1)}${units[i]}`;
}

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/system/tuning");
    status.value = data;
    for (const g of data.params as ParamGroup[]) {
      for (const p of g.items) {
        // 只在用户没改过的时候对齐当前值：否则每次刷新都会把用户填的值冲掉
        if (draft.value[p.key] === undefined) draft.value[p.key] = p.value;
      }
    }
    if (data.ntp?.servers?.length && !ntpServers.value) {
      // 从配置行里抽出服务器地址做初值：`server ntp.aliyun.com iburst` → `ntp.aliyun.com`
      ntpServers.value = (data.ntp.servers as string[])
        .map((s) => s.split(/\s+/)[1] ?? s)
        .filter(Boolean)
        .join(", ");
    }
  } catch {
    status.value = null;
  } finally {
    loading.value = false;
  }
}

/// 统一的提交：成功即拉起作业，提示用户去「任务」页看输出
async function submit(payload: Record<string, unknown>, label: string) {
  const { data } = await http.post("/system/tuning", payload);
  ElMessage.success(`${label} 已提交（作业 ${String(data.id).slice(0, 8)}），可在「任务」页查看输出`);
  return data;
}

async function applyParam(p: ParamSpec) {
  const value = (draft.value[p.key] ?? "").trim();
  busyKey.value = p.key;
  try {
    await ElMessageBox.confirm(
      `${p.title}（${p.key}）将从 ${p.value} 改为 ${value}。\n\n${p.effect}`,
      "确认修改内核参数",
      { confirmButtonText: "应用", cancelButtonText: "取消", type: "warning" },
    );
  } catch {
    busyKey.value = "";
    return; // 用户取消
  }
  try {
    await submit({ action: "sysctl", key: p.key, value }, p.title);
    await load();
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "提交失败");
  } finally {
    busyKey.value = "";
  }
}

async function applySwap(op: "enable" | "disable" | "remove") {
  busyAction.value = `swap-${op}`;
  try {
    if (op === "enable") {
      try {
        await ElMessageBox.confirm(
          `将创建 ${swapSizeMb.value} MB 的交换文件 ${swapPath.value} 并启用，同时写入 /etc/fstab。`,
          "确认创建交换文件",
          { confirmButtonText: "创建", cancelButtonText: "取消", type: "warning" },
        );
      } catch {
        return;
      }
    }
    await submit(
      { action: "swapfile", op, path: swapPath.value, size_mb: swapSizeMb.value },
      "交换区操作",
    );
    await load();
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "提交失败");
  } finally {
    busyAction.value = "";
  }
}

async function applyNtp() {
  busyAction.value = "ntp";
  try {
    try {
      await ElMessageBox.confirm(
        `将把时间服务器重写为：${ntpServers.value}\n并重启时间同步服务。`,
        "确认修改时间同步",
        { confirmButtonText: "应用", cancelButtonText: "取消", type: "warning" },
      );
    } catch {
      return;
    }
    await submit({ action: "ntp", servers: ntpServers.value }, "NTP 配置");
    await load();
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "提交失败");
  } finally {
    busyAction.value = "";
  }
}

onMounted(load);
</script>

<style scoped>
.tuning {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
}
.toolbar {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
.hint {
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
}
.spacer {
  flex: 1;
}
.notice {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: var(--sp-2) var(--sp-3);
  border-radius: var(--radius);
  font-size: var(--fs-sm);
  background: color-mix(
    in srgb,
    var(--el-fill-color-light) var(--panel-surface-opacity, 100%),
    transparent
  );
  -webkit-backdrop-filter: var(--panel-card-blur, blur(0px));
  backdrop-filter: var(--panel-card-blur, blur(0px));
}
.notice--warn {
  /* 用警示色描边而不是整块上色：整块上色在深色主题下会盖住正文对比度 */
  box-shadow: inset 0 0 0 1px var(--el-color-warning);
}
.panel {
  padding: var(--sp-3);
  border-radius: var(--radius);
  background: var(--panel-card-bg, var(--el-bg-color));
  -webkit-backdrop-filter: var(--panel-card-blur, blur(0px));
  backdrop-filter: var(--panel-card-blur, blur(0px));
  box-shadow: var(--panel-shadow-1);
}
.panel-title {
  margin: 0 0 var(--sp-2);
  font-size: var(--fs-base);
  font-weight: 600;
  color: var(--el-text-color-primary);
}
.rows {
  display: flex;
  flex-direction: column;
  gap: var(--sp-3);
}
.row {
  display: flex;
  flex-direction: column;
  gap: var(--sp-1);
}
.row-head {
  display: flex;
  align-items: baseline;
  flex-wrap: wrap;
  gap: var(--sp-2);
}
.row-title {
  font-size: var(--fs-base);
  color: var(--el-text-color-primary);
}
.row-key,
.row-effect code {
  font-family: var(--panel-mono);
  font-size: var(--fs-xs);
  color: var(--el-text-color-secondary);
}
.row-cur {
  font-size: var(--fs-sm);
  color: var(--el-text-color-regular);
  font-variant-numeric: tabular-nums;
}
.row-effect {
  margin: 0;
  font-size: var(--fs-sm);
  line-height: 1.6;
  color: var(--el-text-color-secondary);
}
.row-edit {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  flex-wrap: wrap;
}
.edit-input {
  width: 180px;
}
.edit-input--path {
  width: 240px;
}
.row-tip {
  font-size: var(--fs-xs);
  color: var(--el-text-color-secondary);
}
.swap-list {
  margin: 0;
  padding-left: 0;
  list-style: none;
  display: flex;
  flex-direction: column;
  gap: 2px;
  font-size: var(--fs-sm);
  font-variant-numeric: tabular-nums;
}
.swap-list li {
  display: flex;
  gap: var(--sp-3);
}
.dim {
  color: var(--el-text-color-secondary);
}
.empty {
  padding: var(--sp-4);
  text-align: center;
  font-size: var(--fs-sm);
  color: var(--el-text-color-secondary);
}
</style>
