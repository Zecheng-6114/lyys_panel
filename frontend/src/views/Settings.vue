<template>
  <div class="settings">
    <el-tabs v-model="tab">
      <!-- 系统设置：仅管理员可改的服务端配置 -->
      <el-tab-pane label="系统设置" name="system">
        <div class="card">
          <div class="card-title">AI API 配置</div>
          <div class="card-sub">
            用于 AI 助手对话的上游 OpenAI 兼容接口。留空的项回退到面板环境变量
            （AI_API_BASE / AI_API_KEY / AI_MODEL），均未配置时 AI 对话不可用。
          </div>

          <el-form label-position="top" class="form">
            <el-form-item label="上游 API 地址">
              <el-input
                v-model="form.base"
                placeholder="https://api.openai.com/v1（填到 /v1 为止）"
                clearable
              />
            </el-form-item>
            <el-form-item label="API 密钥">
              <el-input
                v-model="form.key"
                type="password"
                show-password
                autocomplete="new-password"
                :placeholder="keyPlaceholder"
              />
              <div v-if="config.keySet" class="key-hint">
                已保存密钥（{{ config.keyMasked }}）
                <el-button link type="danger" size="small" @click="clearKey">清除已存密钥</el-button>
              </div>
              <div v-else-if="config.envKeySet" class="key-hint">
                未在设置页保存密钥，当前使用环境变量 AI_API_KEY
              </div>
            </el-form-item>
            <el-form-item label="模型名">
              <el-input v-model="form.model" placeholder="gpt-4o-mini" clearable />
            </el-form-item>
            <el-form-item label="人格提示词（persona）">
              <el-input
                v-model="form.persona"
                type="textarea"
                :rows="4"
                resize="vertical"
                placeholder="留空使用内置默认人格：服务器运维助手，回答简洁直接"
              />
            </el-form-item>
            <el-form-item label="技能说明（skills）">
              <el-input
                v-model="form.skills"
                type="textarea"
                :rows="4"
                resize="vertical"
                placeholder="可选。追加在人格后的技能描述，如擅长回答的问题范围、操作规范等"
              />
            </el-form-item>
            <el-form-item>
              <el-button type="primary" :loading="saving" @click="save">保存配置</el-button>
            </el-form-item>
          </el-form>
        </div>
      </el-tab-pane>

      <!-- 界面设置：主题定制（预设/配色/圆角/背景图），保存到服务端对所有设备生效 -->
      <el-tab-pane label="界面设置" name="appearance">
        <div class="card">
          <div class="card-title">主题定制</div>
          <div class="card-sub">颜色留空即沿用默认；保存后对所有设备生效。</div>

          <div class="form">
            <div class="row">
              <label>预设</label>
              <el-select v-model="presetId" placeholder="选择内置预设" @change="applyPreset">
                <el-option v-for="p in PRESETS" :key="p.id" :label="p.label" :value="p.id" />
              </el-select>
            </div>
            <div class="row">
              <label>主题包</label>
              <div class="btns">
                <el-button size="small" @click="pickImport">导入</el-button>
                <el-button size="small" @click="exportTheme">导出</el-button>
                <input
                  ref="importInput"
                  type="file"
                  accept="application/json,.json"
                  class="hidden-file"
                  @change="onImportFile"
                />
              </div>
            </div>

            <div class="row">
              <label>主题名称</label>
              <el-input v-model="draft.name" placeholder="自定义主题" maxlength="40" />
            </div>

            <div class="row">
              <label>圆角 {{ draft.radius }}px</label>
              <el-slider v-model="draft.radius" :min="0" :max="16" :step="1" />
            </div>

            <div class="group">颜色</div>
            <div class="row">
              <label>主色</label>
              <el-color-picker v-model="draft.colors.primary" />
            </div>
            <div class="row">
              <label>页面底色</label>
              <el-color-picker v-model="draft.colors.bg_page" />
            </div>
            <div class="row">
              <label>卡片底色</label>
              <el-color-picker v-model="draft.colors.bg_card" />
            </div>
            <div class="row">
              <label>文本色</label>
              <el-color-picker v-model="draft.colors.text" />
            </div>

            <div class="group">背景图</div>
            <div class="row">
              <label>图片</label>
              <div class="btns">
                <el-button size="small" @click="pickImage">选择图片</el-button>
                <el-button v-if="draft.bg_image" size="small" @click="draft.bg_image = null">
                  清除
                </el-button>
                <input
                  ref="imageInput"
                  type="file"
                  accept="image/*"
                  class="hidden-file"
                  @change="onImageFile"
                />
              </div>
            </div>
            <div v-if="draft.bg_image" class="hint">已设置背景图（铺满内容区，随页面固定）</div>

            <div class="row">
              <label></label>
              <div class="btns">
                <el-button type="primary" :loading="themeSaving" @click="saveTheme">保存主题</el-button>
                <el-button :disabled="themeSaving" @click="resetDefault">恢复默认</el-button>
              </div>
            </div>
          </div>
        </div>
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref, watch } from "vue";
import { ElMessage } from "element-plus";
import http from "../api/http";
import { useThemeStore, type ThemeColors, type ThemeConfig } from "../stores/theme";
import { PRESETS } from "../themes/presets";

const tab = ref("system");

/* ---------------- 系统设置：AI API 配置 ---------------- */

interface AiConfigResp {
  base: string;
  model: string;
  key_set: boolean;
  key_masked: string | null;
  env_key_set: boolean;
  configured: boolean;
  persona: string;
  skills: string;
}

// 生效配置（后端已做 settings → env → 默认值合并）
const config = reactive({
  base: "",
  model: "",
  keySet: false,
  keyMasked: null as string | null,
  envKeySet: false,
  configured: false,
});

// 表单：key 不回显明文，仅输入新密钥时提交
const form = reactive({ base: "", key: "", model: "", persona: "", skills: "" });
const saving = ref(false);

const keyPlaceholder = computed(() =>
  config.keySet ? "已配置，留空保持不变" : "sk-...",
);

async function load() {
  const { data } = await http.get<{ config: AiConfigResp }>("/ai/config");
  const c = data.config;
  config.base = c.base;
  config.model = c.model;
  config.keySet = c.key_set;
  config.keyMasked = c.key_masked;
  config.envKeySet = c.env_key_set;
  config.configured = c.configured;
  // 回显生效值；密钥不回显，占位符提示状态
  form.base = c.base;
  form.model = c.model;
  form.key = "";
  form.persona = c.persona;
  form.skills = c.skills;
}

async function save() {
  if (form.base.trim() && !/^https?:\/\//.test(form.base.trim())) {
    ElMessage.warning("上游 API 地址必须以 http(s):// 开头");
    return;
  }
  saving.value = true;
  try {
    await http.post("/ai/config", {
      base: form.base.trim(),
      model: form.model.trim(),
      // key 为空表示保持原密钥不变（后端 None=不改）
      ...(form.key.trim() ? { key: form.key.trim() } : {}),
      // persona/skills 传值即覆盖，空串=清除回退默认人格
      persona: form.persona,
      skills: form.skills,
    });
    ElMessage.success("AI API 配置已保存，对话即时生效");
    form.key = "";
    await load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.message || "保存失败");
  } finally {
    saving.value = false;
  }
}

async function clearKey() {
  saving.value = true;
  try {
    await http.post("/ai/config", {
      base: form.base.trim(),
      model: form.model.trim(),
      key: "",
      persona: form.persona,
      skills: form.skills,
    });
    ElMessage.success("已清除设置页密钥（如配置了环境变量将回退）");
    await load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.message || "操作失败");
  } finally {
    saving.value = false;
  }
}

/* ---------------- 界面设置：主题定制 ---------------- */

const theme = useThemeStore();

/// 背景图原始文件大小上限（base64 后约 ×1.34，仍低于后端 3MB 限制）
const MAX_IMAGE_BYTES = 2 * 1024 * 1024;

interface DraftColors {
  primary: string | null;
  bg_page: string | null;
  bg_card: string | null;
  text: string | null;
}

interface Draft {
  name: string;
  radius: number;
  colors: DraftColors;
  bg_image: string | null;
}

function emptyDraft(): Draft {
  return {
    name: "自定义主题",
    radius: 6,
    colors: { primary: null, bg_page: null, bg_card: null, text: null },
    bg_image: null,
  };
}

const draft = reactive<Draft>(emptyDraft());
const presetId = ref("");
const themeSaving = ref(false);
const importInput = ref<HTMLInputElement>();
const imageInput = ref<HTMLInputElement>();

/// 服务端配置/预设 → 编辑态。keepRadius=true 时保留当前圆角
/// （预设只定义配色，不该把用户调好的圆角重置回默认值）
function fromConfig(cfg: ThemeConfig | null, keepRadius = false) {
  const d = emptyDraft();
  if (keepRadius) d.radius = draft.radius;
  if (cfg) {
    if (cfg.name) d.name = cfg.name;
    if (typeof cfg.radius === "number") d.radius = cfg.radius;
    if (cfg.colors) Object.assign(d.colors, pickColors(cfg.colors));
    if (cfg.bg_image) d.bg_image = cfg.bg_image;
  }
  Object.assign(draft, d);
}

function pickColors(c: ThemeColors): DraftColors {
  return {
    primary: c.primary ?? null,
    bg_page: c.bg_page ?? null,
    bg_card: c.bg_card ?? null,
    text: c.text ?? null,
  };
}

/// 编辑态 → 配置（过滤空颜色，全空则不带 colors）
function toConfig(): ThemeConfig {
  const cfg: ThemeConfig = { version: 1, name: draft.name.trim() || "自定义主题" };
  cfg.radius = draft.radius;
  const colors = compact(draft.colors);
  if (Object.keys(colors).length) cfg.colors = colors;
  if (draft.bg_image) cfg.bg_image = draft.bg_image;
  return cfg;
}

function compact(c: DraftColors): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(c)) if (v) out[k] = v;
  return out;
}

// 主题配置由 AppLayout 异步拉取，到位后同步进编辑态
watch(() => theme.config, (cfg) => fromConfig(cfg), { immediate: true });

function applyPreset(id: string) {
  const p = PRESETS.find((x) => x.id === id);
  // 预设显式给了 radius（如高对比=0）则用预设值，否则保留用户现值
  if (p) fromConfig(p.config, p.config.radius === undefined);
}

function pickImport() {
  importInput.value?.click();
}

async function onImportFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  try {
    const text = await file.text();
    const obj = JSON.parse(text) as ThemeConfig;
    if (typeof obj !== "object" || obj === null) throw new Error("bad");
    fromConfig(obj);
    presetId.value = "";
    ElMessage.success("已导入主题包，点击保存生效");
  } catch {
    ElMessage.error("主题包不是有效的 JSON 文件");
  }
}

function exportTheme() {
  const cfg = toConfig();
  const blob = new Blob([JSON.stringify(cfg, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `${(cfg.name || "theme").replace(/[\\/:*?"<>|]/g, "_")}.json`;
  a.click();
  URL.revokeObjectURL(url);
}

function pickImage() {
  imageInput.value?.click();
}

async function onImageFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  if (file.size > MAX_IMAGE_BYTES) {
    ElMessage.error("背景图不能超过 2MB");
    return;
  }
  draft.bg_image = await toDataURL(file);
}

function toDataURL(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

async function saveTheme() {
  themeSaving.value = true;
  try {
    await theme.save(toConfig());
    ElMessage.success("主题已保存，对所有设备生效");
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "保存失败");
  } finally {
    themeSaving.value = false;
  }
}

async function resetDefault() {
  themeSaving.value = true;
  try {
    await theme.save(null);
    fromConfig(null);
    presetId.value = "";
    ElMessage.success("已恢复默认");
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "恢复失败");
  } finally {
    themeSaving.value = false;
  }
}

onMounted(() => {
  load().catch(() => ElMessage.error("配置加载失败"));
});
</script>

<style scoped>
.card {
  background: var(--el-bg-color);
  border-radius: var(--radius, 8px);
  padding: 20px 24px;
}
.card-title {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 4px;
}
.card-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-bottom: 16px;
  line-height: 1.6;
}
.form {
  margin-top: 8px;
}
/* 卡片占满内容区，但输入控件保持可读宽度 */
.settings :deep(.el-form-item__content) {
  max-width: 480px;
}
.key-hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 4px;
  display: flex;
  align-items: center;
  gap: 8px;
}
/* 界面设置表单行 */
.settings .form .row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
  max-width: 480px;
}
.settings .form .row > label {
  flex: none;
  width: 96px;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.settings .form .row > .el-slider,
.settings .form .row > .el-input,
.settings .form .row > .el-select {
  flex: 1;
}
.btns {
  display: flex;
  gap: 8px;
}
.group {
  font-size: 13px;
  font-weight: 500;
  color: var(--el-text-color-primary);
  margin: 4px 0 12px;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-bottom: 12px;
}
.hidden-file {
  display: none;
}
</style>
