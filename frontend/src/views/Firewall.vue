<template>
  <div class="firewall">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button :disabled="!fw.installed || !fw.active" @click="openAdd">新增规则</el-button>
      <span class="spacer"></span>
      <template v-if="fw.installed">
        <span class="backend">后端：{{ fw.backend }}</span>
        <el-switch
          v-model="fw.active"
          :loading="switching"
          @change="onToggle"
          active-text="已启用"
          inactive-text="已停用"
        />
      </template>
    </div>

    <!-- 未检测到 ufw / firewalld：面板不接管裸 iptables，如实提示安装 -->
    <div v-if="!fw.installed" class="empty">
      <div class="empty-title">未检测到防火墙</div>
      <div class="empty-desc">
        面板只驱动发行版自带的防火墙前端：Debian 系安装 <code>ufw</code>，Arch 系安装
        <code>firewalld</code>。安装后点「刷新」即可管理。
      </div>
    </div>

    <template v-else>
      <div v-if="!fw.active" class="notice">防火墙当前处于停用状态，规则不会生效。</div>

      <el-table
        v-loading="loading"
        :data="rules"
        size="small"
        class="ftable"
        height="var(--panel-table-height)"
      >
        <el-table-column label="动作" v-bind="col(90)">
          <template #default="{ row }">
            <span class="act" :class="row.action === 'ALLOW' ? 'act-allow' : 'act-deny'">
              {{ row.action }}
            </span>
          </template>
        </el-table-column>
        <el-table-column label="端口" v-bind="col(120)">
          <template #default="{ row }">{{ row.port || "-" }}</template>
        </el-table-column>
        <el-table-column label="协议" prop="protocol" v-bind="col(90)" v-if="!hideColP2" />
        <el-table-column label="来源" prop="from" v-bind="col(200, true)" />
        <el-table-column label="备注" v-bind="col(140, true)" v-if="!hideColP3">
          <template #default="{ row }">{{ row.comment || "-" }}</template>
        </el-table-column>
        <el-table-column label="操作" v-bind="col(90)" align="right">
          <template #default="{ row }">
            <el-button link size="small" @click="removeRule(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </template>

    <el-dialog v-model="showAdd" title="新增规则" width="480px">
      <el-form label-width="72px" size="small">
        <el-form-item label="动作">
          <el-radio-group v-model="form.op">
            <el-radio-button value="allow">放行</el-radio-button>
            <el-radio-button value="deny" :disabled="!fw.deny_supported">拒绝</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <el-form-item label="端口">
          <el-input v-model="form.port" placeholder="如 8080 或 1000-2000，可留空" />
        </el-form-item>
        <el-form-item label="协议">
          <el-radio-group v-model="form.protocol">
            <el-radio-button value="tcp">TCP</el-radio-button>
            <el-radio-button value="udp">UDP</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <el-form-item label="来源">
          <el-input v-model="form.from" placeholder="如 203.0.113.5 或 10.0.0.0/8，可留空" />
        </el-form-item>
      </el-form>
      <div class="hint">端口与来源至少填一个；都不填则按「任意来源的指定端口」处理。</div>
      <template #footer>
        <el-button @click="showAdd = false">取消</el-button>
        <el-button :loading="saving" @click="save">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";

interface FwStatus {
  backend: string;
  installed: boolean;
  active: boolean;
  deny_supported: boolean;
  detail: string;
}
interface Rule {
  action: string;
  protocol: string;
  port: string;
  from: string;
  comment: string;
  id: string;
}

const fw = reactive<FwStatus>({
  backend: "none",
  installed: false,
  active: false,
  deny_supported: false,
  detail: "",
});
const rules = ref<Rule[]>([]);
const loading = ref(false);
const switching = ref(false);
const saving = ref(false);
const showAdd = ref(false);
const form = reactive({ op: "allow" as "allow" | "deny", port: "", protocol: "tcp", from: "" });

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/firewall/status");
    Object.assign(fw, data);
    rules.value = fw.installed ? (await http.get("/firewall/rules")).data : [];
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取防火墙信息失败");
  } finally {
    loading.value = false;
  }
}

async function onToggle(next: string | number | boolean) {
  switching.value = true;
  try {
    await http.post("/firewall/toggle", { enable: Boolean(next) });
    ElMessage.success(Boolean(next) ? "防火墙已启用" : "防火墙已停用");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
    // 失败则回滚开关状态，避免界面与主机实际状态不一致
    fw.active = !Boolean(next);
  } finally {
    switching.value = false;
  }
}

function openAdd() {
  form.op = "allow";
  form.port = "";
  form.protocol = "tcp";
  form.from = "";
  showAdd.value = true;
}

async function save() {
  if (!form.port.trim() && !form.from.trim()) {
    ElMessage.warning("端口与来源至少填一个");
    return;
  }
  saving.value = true;
  try {
    await http.post("/firewall/rule", { ...form });
    showAdd.value = false;
    ElMessage.success("规则已添加");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "添加失败");
  } finally {
    saving.value = false;
  }
}

async function removeRule(row: Rule) {
  try {
    await ElMessageBox.confirm("确定删除该规则？", "删除确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete("/firewall/rule", { data: { id: row.id } });
    ElMessage.success("已删除");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "删除失败");
  }
}

onMounted(load);
</script>

<style scoped>
.backend {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.act {
  font-family: var(--panel-mono);
  font-size: 12px;
}
.act-allow {
  color: var(--el-color-success);
}
.act-deny {
  color: var(--el-color-danger);
}
.notice {
  font-size: 13px;
  color: var(--el-color-warning);
  padding: 6px 2px;
}
.empty {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 28px 20px;
  text-align: center;
}
.empty-title {
  font-size: 14px;
  margin-bottom: 8px;
}
.empty-desc {
  font-size: 13px;
  color: var(--el-text-color-secondary);
  line-height: 1.7;
}
.empty-desc code {
  font-family: var(--panel-mono);
  background: var(--el-fill-color-light);
  padding: 1px 5px;
  border-radius: 4px;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  padding-left: 72px;
}
</style>
