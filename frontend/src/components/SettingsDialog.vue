<template>
  <el-dialog
    :model-value="modelValue"
    title="界面设置"
    width="560px"
    top="6vh"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="form">
      <!-- 预设与主题包 -->
      <div class="row">
        <label>预设</label>
        <el-select v-model="presetId" placeholder="选择内置预设" @change="applyPreset">
          <el-option v-for="p in PRESETS" :key="p.id" :label="p.label" :value="p.id" />
        </el-select>
      </div>
      <div class="row">
        <label>主题包</label>
        <div class="btns">
          <button class="mini-btn mini-btn--sm" type="button" @click="pickImport">导入</button>
          <button class="mini-btn mini-btn--sm" type="button" @click="exportTheme">导出</button>
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

      <!-- 圆角 -->
      <div class="row">
        <label>圆角 {{ draft.radius }}px</label>
        <el-slider v-model="draft.radius" :min="0" :max="16" :step="1" />
      </div>

      <!-- 颜色 -->
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

      <!-- 背景图 -->
      <div class="group">背景图</div>
      <div class="row">
        <label>图片</label>
        <div class="btns">
          <button class="mini-btn mini-btn--sm" type="button" @click="pickImage">选择图片</button>
          <button
            v-if="draft.bg_image"
            class="mini-btn mini-btn--sm"
            type="button"
            @click="draft.bg_image = null"
          >
            清除
          </button>
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
      <div class="hint">颜色留空即沿用默认；保存后对所有设备生效</div>
    </div>

    <template #footer>
      <div class="footer">
        <button class="mini-btn" type="button" @click="resetDefault">恢复默认</button>
        <div class="spacer" />
        <button class="mini-btn" type="button" @click="emit('update:modelValue', false)">
          取消
        </button>
        <button class="mini-btn" type="button" :disabled="saving" @click="save">保存</button>
      </div>
    </template>
  </el-dialog>
</template>

<script setup lang="ts">
import { reactive, ref, watch } from "vue";
import { ElMessage } from "element-plus";
import { useThemeStore, type ThemeColors, type ThemeConfig } from "../stores/theme";
import { PRESETS } from "../themes/presets";

const props = defineProps<{ modelValue: boolean }>();
const emit = defineEmits<{ "update:modelValue": [boolean] }>();
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
const saving = ref(false);
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

// 打开弹窗时以服务端配置为初值
watch(
  () => props.modelValue,
  (open) => {
    if (open) fromConfig(theme.config);
  },
);

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

async function save() {
  saving.value = true;
  try {
    await theme.save(toConfig());
    ElMessage.success("已保存");
    emit("update:modelValue", false);
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "保存失败");
  } finally {
    saving.value = false;
  }
}

async function resetDefault() {
  saving.value = true;
  try {
    await theme.save(null);
    fromConfig(null);
    presetId.value = "";
    ElMessage.success("已恢复默认");
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "恢复失败");
  } finally {
    saving.value = false;
  }
}
</script>

<style scoped>
.form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.row {
  display: flex;
  align-items: center;
  gap: 12px;
}
.row > label {
  flex: none;
  width: 96px;
  font-size: 13px;
  color: var(--el-text-color-secondary);
}
.row > .el-slider,
.row > .el-input,
.row > .el-select {
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
  margin-top: 4px;
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
}
.hidden-file {
  display: none;
}
.footer {
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
