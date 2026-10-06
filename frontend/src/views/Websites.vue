<template>
  <div class="websites">
    <div class="toolbar">
      <el-button @click="load">刷新</el-button>
      <el-button :disabled="!st.installed" @click="openNew">新建站点</el-button>
      <el-button :disabled="!st.installed" @click="openAcme">ACME 设置</el-button>
      <span class="spacer"></span>
      <template v-if="st.installed">
        <span class="backend">nginx {{ st.version || "?" }}</span>
        <el-tag :type="st.active ? 'success' : 'info'" size="small" effect="plain">
          {{ st.active ? "运行中" : "已停止" }}
        </el-tag>
      </template>
    </div>

    <!-- 未安装 nginx：站点管理无从谈起，如实提示并给一键安装入口 -->
    <div v-if="!st.installed" class="empty">
      <div class="empty-title">未检测到 nginx</div>
      <div class="empty-desc">
        网站管理依赖 nginx。可点下方按钮用面板的包管理能力安装（Debian 系 / Arch
        系通用），安装完成后点「刷新」即可新建站点。
      </div>
      <el-button :loading="installing" @click="installNginx">安装 nginx</el-button>
    </div>

    <template v-else>
      <el-table
        v-loading="loading"
        :data="rows"
        size="small"
        height="var(--panel-table-height)"
      >
        <el-table-column label="服务器名" prop="name" v-bind="col(220, true)">
          <template #default="{ row }">
            <span class="mono">{{ row.name }}</span>
          </template>
        </el-table-column>
        <el-table-column label="类型" v-bind="col(96)">
          <template #default="{ row }">{{ row.kind === "static" ? "静态" : "反代" }}</template>
        </el-table-column>
        <el-table-column label="端口" v-bind="col(84)">
          <template #default="{ row }">
            <span class="mono">{{ row.listen }}{{ row.tls ? " / ssl" : "" }}</span>
          </template>
        </el-table-column>
        <el-table-column label="目标" v-bind="col(240, true)" v-if="!hideColP2">
          <template #default="{ row }">
            <span class="mono target">{{ row.kind === "static" ? row.root : row.upstream }}</span>
          </template>
        </el-table-column>
        <el-table-column label="HTTPS" v-bind="col(84)" v-if="!hideColP3">
          <template #default="{ row }">
            <span :class="row.tls ? 'on' : 'off'">{{ row.tls ? "启用" : "关闭" }}</span>
          </template>
        </el-table-column>
        <el-table-column label="证书" v-bind="col(110)" v-if="!hideColP3">
          <template #default="{ row }">
            <el-tag v-if="row.acme" size="small" type="success" effect="plain">Let's Encrypt</el-tag>
            <span v-else :class="row.tls ? 'on' : 'off'">{{ row.tls ? "自签/上传" : "—" }}</span>
          </template>
        </el-table-column>
        <el-table-column label="状态" v-bind="col(88)">
          <template #default="{ row }">
            <el-switch
              :model-value="row.enabled"
              :loading="toggling === row.id"
              @change="(v: string | number | boolean) => toggle(row, v)"
            />
          </template>
        </el-table-column>
        <el-table-column label="操作" v-bind="col(190)" align="right">
          <template #default="{ row }">
            <el-button
              v-if="row.acme"
              link
              size="small"
              :loading="issuing === row.id"
              @click="issueCert(row)"
            >
              签发
            </el-button>
            <el-button link size="small" @click="openEdit(row)">编辑</el-button>
            <el-button link size="small" @click="remove(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </template>

    <el-dialog v-model="showEdit" :title="form.id ? '编辑站点' : '新建站点'" width="560px">
      <el-form label-width="96px" size="small">
        <el-form-item label="服务器名">
          <el-input v-model="form.name" placeholder="example.com www.example.com" />
        </el-form-item>
        <el-form-item label="类型">
          <el-radio-group v-model="form.kind">
            <el-radio-button value="static">静态站点</el-radio-button>
            <el-radio-button value="proxy">反向代理</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <el-form-item label="监听端口">
          <el-input-number v-model="form.listen" :min="1" :max="65535" style="width: 100%" />
        </el-form-item>
        <el-form-item v-if="form.kind === 'static'" label="根目录">
          <el-input v-model="form.root" placeholder="/var/www/html" />
        </el-form-item>
        <el-form-item v-else label="上游地址">
          <el-input v-model="form.upstream" placeholder="http://127.0.0.1:3000" />
        </el-form-item>

        <el-form-item label="启用 HTTPS">
          <el-switch v-model="form.tls" :disabled="form.acme" />
        </el-form-item>
        <el-form-item label="自动证书">
          <el-switch v-model="form.acme" @change="onAcmeChange" />
          <span class="inline-hint">Let's Encrypt，需域名已解析到本机、80 端口可被外网访问</span>
        </el-form-item>
        <template v-if="form.tls && !form.acme">
          <el-form-item label="证书来源">
            <el-radio-group v-model="form.certMode">
              <el-radio-button value="self">自签证书</el-radio-button>
              <el-radio-button value="upload">上传证书</el-radio-button>
            </el-radio-group>
          </el-form-item>
          <template v-if="form.certMode === 'upload'">
            <el-form-item label="证书 PEM">
              <el-input v-model="form.certPem" type="textarea" :rows="3" placeholder="-----BEGIN CERTIFICATE-----" />
            </el-form-item>
            <el-form-item label="私钥 PEM">
              <el-input v-model="form.keyPem" type="textarea" :rows="3" placeholder="-----BEGIN PRIVATE KEY-----" />
            </el-form-item>
          </template>
        </template>

        <el-form-item label="启用站点">
          <el-switch v-model="form.enabled" />
        </el-form-item>
      </el-form>
      <div class="hint">
        配置由面板生成到 <code>/etc/nginx/conf.d/</code>，保存前会先执行
        <code>nginx -t</code> 校验，通过才重载；校验失败自动回滚，不会把 nginx 写挂。
        自签证书在首次启用 HTTPS 时生成，浏览器会提示不受信任——公网站点请改用上传证书。
        <br />
        静态根目录须为 nginx 可读路径，且不要用 <code>/tmp</code>：nginx 服务带
        <code>PrivateTmp</code>，看不到临时目录，否则只会得到 404。
      </div>
      <template #footer>
        <el-button @click="showEdit = false">取消</el-button>
        <el-button type="primary" :loading="saving" @click="save">保存</el-button>
      </template>
    </el-dialog>

    <el-dialog v-model="showAcme" title="Let's Encrypt 设置" width="480px">
      <el-form label-width="96px" size="small">
        <el-form-item label="账户邮箱">
          <el-input v-model="acmeSt.email" placeholder="ops@example.com（可留空）" />
        </el-form-item>
        <el-form-item label="测试环境">
          <el-switch v-model="acmeSt.staging" />
          <span class="inline-hint">用预发环境签发，不消耗配额，但证书不受浏览器信任</span>
        </el-form-item>
      </el-form>
      <div class="hint">
        邮箱用于接收证书到期提醒（可留空）。首次签发请在站点行点「签发」；
        之后证书满 60 天会由后台任务自动续期。此过程要求域名已解析到本机且
        80 端口可从公网访问（Let's Encrypt 的 http-01 校验走 80 端口）。
      </div>
      <template #footer>
        <el-button @click="showAcme = false">取消</el-button>
        <el-button type="primary" :loading="acmeSaving" @click="saveAcme">保存</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { onMounted, reactive, ref } from "vue";
import http from "../api/http";
import { submitJob } from "../api/jobs";
import { useJobsStore } from "../stores/jobs";

interface Site {
  id: string;
  name: string;
  kind: "static" | "proxy";
  root: string;
  upstream: string;
  listen: number;
  tls: boolean;
  acme: boolean;
  enabled: boolean;
}
interface NginxStatus {
  installed: boolean;
  active: boolean;
  version: string;
}
interface AcmeSettings {
  email: string;
  staging: boolean;
}

const jobWatch = useJobsStore();
const st = reactive<NginxStatus>({ installed: false, active: false, version: "" });
const rows = ref<Site[]>([]);
const loading = ref(false);
const saving = ref(false);
const installing = ref(false);
const toggling = ref("");
const issuing = ref("");
const showEdit = ref(false);
const showAcme = ref(false);
const acmeSaving = ref(false);
const acmeSt = reactive<AcmeSettings>({ email: "", staging: false });

const form = reactive({
  id: "",
  name: "",
  kind: "proxy" as "static" | "proxy",
  root: "",
  upstream: "",
  listen: 80,
  tls: false,
  acme: false,
  enabled: true,
  certMode: "self" as "self" | "upload",
  certPem: "",
  keyPem: "",
});

async function load() {
  loading.value = true;
  try {
    const { data } = await http.get("/websites/status");
    Object.assign(st, data);
    rows.value = st.installed ? (await http.get("/websites")).data : [];
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取站点信息失败");
  } finally {
    loading.value = false;
  }
}

/** 用面板现有的包管理作业安装 nginx，装完点刷新即可 */
async function installNginx() {
  installing.value = true;
  try {
    const id = await submitJob("pkg_install", { names: ["nginx"] });
    jobWatch.watch(id);
    ElMessage.success("已加入任务队列，安装完成后点「刷新」");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "提交安装失败");
  } finally {
    installing.value = false;
  }
}

function reset() {
  form.id = "";
  form.name = "";
  form.kind = "proxy";
  form.root = "";
  form.upstream = "";
  form.listen = 80;
  form.tls = false;
  form.acme = false;
  form.enabled = true;
  form.certMode = "self";
  form.certPem = "";
  form.keyPem = "";
}

function openNew() {
  reset();
  showEdit.value = true;
}

function openEdit(row: Site) {
  reset();
  form.id = row.id;
  form.name = row.name;
  form.kind = row.kind;
  form.root = row.root;
  form.upstream = row.upstream;
  form.listen = row.listen;
  form.tls = row.tls;
  form.acme = row.acme;
  form.enabled = row.enabled;
  showEdit.value = true;
}

/** 勾选自动证书即隐含启用 HTTPS（后端亦强制，此处同步 UI 以免状态割裂） */
function onAcmeChange(v: string | number | boolean) {
  if (v) form.tls = true;
}

/** 打开 ACME 全局设置弹窗，读取当前账户邮箱 / 测试环境 */
async function openAcme() {
  try {
    const { data } = await http.get("/websites/acme");
    Object.assign(acmeSt, data);
    showAcme.value = true;
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取 ACME 设置失败");
  }
}

async function saveAcme() {
  acmeSaving.value = true;
  try {
    await http.post("/websites/acme", { email: acmeSt.email, staging: acmeSt.staging });
    showAcme.value = false;
    ElMessage.success("已保存 ACME 设置");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  } finally {
    acmeSaving.value = false;
  }
}

/** 立即为站点申请 / 续期证书；耗时较长（需等 CA 校验与签发），按钮期间转圈 */
async function issueCert(row: Site) {
  issuing.value = row.id;
  try {
    await http.post("/websites/acme/issue", { id: row.id });
    ElMessage.success("证书已签发并重载 nginx");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "签发失败");
  } finally {
    issuing.value = "";
  }
}

async function save() {
  if (!form.name.trim()) {
    ElMessage.warning("请填写服务器名");
    return;
  }
  if (form.kind === "static" && !form.root.trim()) {
    ElMessage.warning("请填写静态站点根目录");
    return;
  }
  if (form.kind === "proxy" && !form.upstream.trim()) {
    ElMessage.warning("请填写上游地址");
    return;
  }
  saving.value = true;
  try {
    await http.post("/websites", {
      id: form.id || null,
      name: form.name,
      kind: form.kind,
      root: form.root,
      upstream: form.upstream,
      listen: form.listen,
      tls: form.tls,
      acme: form.acme,
      enabled: form.enabled,
      // 仅「上传证书」时带上 PEM；自签则留空，由后端生成或沿用已存证书
      cert_pem: form.tls && form.certMode === "upload" ? form.certPem : "",
      key_pem: form.tls && form.certMode === "upload" ? form.keyPem : "",
    });
    showEdit.value = false;
    ElMessage.success("已保存并重载 nginx");
    load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "保存失败");
  } finally {
    saving.value = false;
  }
}

async function toggle(row: Site, next: string | number | boolean) {
  toggling.value = row.id;
  try {
    await http.post("/websites", { ...row, enabled: Boolean(next) });
    ElMessage.success(Boolean(next) ? "站点已启用" : "站点已停用");
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "操作失败");
  } finally {
    toggling.value = "";
    load();
  }
}

async function remove(row: Site) {
  try {
    await ElMessageBox.confirm(`确定删除站点 ${row.name}？其 nginx 配置与证书一并移除。`, "删除确认", {
      type: "warning",
      confirmButtonText: "删除",
      cancelButtonText: "取消",
    });
  } catch {
    return;
  }
  try {
    await http.delete("/websites", { data: { id: row.id } });
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
.mono {
  font-family: var(--panel-mono);
  font-size: 12px;
}
.target {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
}
.on {
  color: var(--el-color-success);
}
.off {
  color: var(--el-text-color-secondary);
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
  margin-bottom: 16px;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  line-height: 1.7;
}
.hint code {
  font-family: var(--panel-mono);
  background: var(--el-fill-color-light);
  padding: 1px 5px;
  border-radius: 4px;
}
.inline-hint {
  margin-left: 10px;
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
</style>
