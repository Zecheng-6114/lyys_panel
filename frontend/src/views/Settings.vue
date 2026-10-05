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
            <el-form-item label="联网搜索地址（可选）">
              <el-input
                v-model="form.searchBase"
                placeholder="留空使用内置通道；也可填自建 SearxNG 地址"
                clearable
              />
              <div class="key-hint">
                助手回答软件文档、报错含义一类问题时用它检索。留空走内置的 Bing /
                DuckDuckGo 通道；填 SearxNG 需实例已开启 JSON 输出（format=json）。
              </div>
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
            <div class="row">
              <label>自动配色</label>
              <div class="btns">
                <el-button
                  size="small"
                  :disabled="!draft.bg_image"
                  :loading="monetizing"
                  @click="monetize"
                >
                  从背景图取色
                </el-button>
              </div>
            </div>
            <div class="hint">
              莫奈取色：从背景图里挑出最代表这张图的颜色，再按明暗展开成上面四色。
              先设置背景图才可用；深浅档跟随当前卡片底色的明暗。
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
            <div v-else class="hint">
              常见格式均可，大图会自动压缩到最长边 2560 再保存
            </div>

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
import { isDarkTheme, useThemeStore, type ThemeColors, type ThemeConfig } from "../stores/theme";
import { PRESETS } from "../themes/presets";
import { monetFromImage } from "../themes/monet";

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
  search_base: string;
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
const form = reactive({
  base: "",
  key: "",
  model: "",
  persona: "",
  skills: "",
  searchBase: "",
});
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
  form.searchBase = c.search_base || "";
}

async function save() {
  if (form.base.trim() && !/^https?:\/\//.test(form.base.trim())) {
    ElMessage.warning("上游 API 地址必须以 http(s):// 开头");
    return;
  }
  if (form.searchBase.trim() && !/^https?:\/\//.test(form.searchBase.trim())) {
    ElMessage.warning("联网搜索地址必须以 http(s):// 开头");
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
      // 空串 = 清除自定义搜索地址，回退内置通道
      search_base: form.searchBase.trim(),
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
      search_base: form.searchBase.trim(),
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

/// 背景图原始文件上限。只防误传超大文件（如 RAW/PSD 改名成 .jpg），
/// 正常的手机原图（3~8MB）在这里一律放行 —— 真正决定能否入库的不是它。
const MAX_IMAGE_BYTES = 20 * 1024 * 1024;
/// 入库预算：data URL 的字符数上限。后端 /theme 请求体上限 3MB，主题 JSON
/// 自身只占几十字节，这里按 2.6MB 卡住，留出包装余量。base64 约为原图的
/// 1.37 倍，即约合 1.9MB 的图片体积。
const MAX_DATA_URL_BYTES = 2_600_000;
/// 压缩目标最长边（像素）。背景图是 cover 铺满内容区，再高的分辨率在屏幕上
/// 也看不出差别，超出的部分纯属白占体积。
const MAX_EDGE = 2560;

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
    ElMessage.error(`背景图不能超过 ${MAX_IMAGE_BYTES / 1024 / 1024}MB`);
    return;
  }
  try {
    draft.bg_image = await toBgDataUrl(file);
  } catch (err) {
    ElMessage.error(err instanceof Error ? err.message : "背景图读取失败");
  }
}

function toDataURL(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}

/// 文件 → 可入库的 data URL。
///
/// 背景图以 data URL 内嵌在主题 JSON 里存进 settings 表，所以体积受后端
/// 请求体上限约束；直接卡原图大小的话，手机随手拍一张就超，得用户自己去
/// 缩图。这里改成浏览器端压：原图本来就在预算内就原样用（不重编码、零画质
/// 损失），超了才用 canvas 重编码，逐轮缩边降质直到落进预算。
async function toBgDataUrl(file: File): Promise<string> {
  if (!file.type.startsWith("image/")) throw new Error("只能选择图片文件");
  // 原图转成 base64 后本来就装得下 → 原样内联，不重编码（零画质损失）。
  // 0.72 = 1 / 1.37，1.37 是 base64 相对原字节的膨胀系数，再让一点余量。
  if (file.size <= MAX_DATA_URL_BYTES * 0.72) return toDataURL(file);
  const img = await decodeImage(file);
  let edge = MAX_EDGE;
  let quality = 0.9;
  // 4 轮足够：体积与「边长² × 质量」同阶，第 1 轮通常就落到预算内
  for (let i = 0; i < 4; i++) {
    const out = encodeImage(img, edge, quality);
    if (out.length <= MAX_DATA_URL_BYTES) return out;
    edge = Math.round(edge * 0.75);
    quality = Math.max(0.6, quality - 0.1);
  }
  throw new Error("图片压缩后仍然过大，请换一张");
}

/// 待压缩的图 → 可绘制的解码结果。
///
/// 🔴 不能用 `URL.createObjectURL(file)` 解码：面板的 CSP 是
/// `img-src 'self' data:`，不含 blob:，blob URL 会被直接拦掉
/// （浏览器 DevTools 里能看到 `img-src ← blob` 违规，<img> 收到 error）。
/// createImageBitmap 直接吃 File，不走资源加载，因此不受 img-src 约束。
///
/// imageOrientation: "from-image" 必须显式给 —— <img> 会自动按 EXIF 旋转，
/// createImageBitmap 默认**不**旋转，手机竖拍的照片会被摆成横的。
async function decodeImage(file: File): Promise<ImageBitmap | HTMLImageElement> {
  try {
    return await createImageBitmap(file, { imageOrientation: "from-image" });
  } catch {
    // 浏览器没有 createImageBitmap，或它解不了这个格式 → 退回 <img> + data URL
    return loadImage(await toDataURL(file));
  }
}

/// 取解码结果的像素尺寸：ImageBitmap 用 width/height，<img> 用 natural*
function imageSize(img: ImageBitmap | HTMLImageElement): [number, number] {
  return img instanceof HTMLImageElement
    ? [img.naturalWidth, img.naturalHeight]
    : [img.width, img.height];
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("图片无法解码"));
    img.src = src;
  });
}

/// canvas 重编码：按最长边等比缩放后导出。优先 webp（同画质体积约为
/// jpeg 的七成，且保留透明通道），浏览器不支持时会退化成 png，此时改用
/// jpeg 兜底 —— 代价是带透明像素的图会填成黑色，但这条路径只在不支持
/// webp 的老浏览器上走到，换来的体积优势更大。
/// 动图（gif）经此只留首帧 —— 会走到这里说明它已超过预算，静帧是合理取舍。
function encodeImage(img: ImageBitmap | HTMLImageElement, edge: number, quality: number): string {
  const [sw, sh] = imageSize(img);
  const longest = Math.max(sw, sh) || 1;
  const scale = Math.min(1, edge / longest);
  const w = Math.max(1, Math.round(sw * scale));
  const h = Math.max(1, Math.round(sh * scale));
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("当前浏览器不支持图片压缩");
  ctx.drawImage(img, 0, 0, w, h);
  const webp = canvas.toDataURL("image/webp", quality);
  if (webp.startsWith("data:image/webp")) return webp;
  return canvas.toDataURL("image/jpeg", quality);
}

/* ---------------- 莫奈取色 ---------------- */

const monetizing = ref(false);

/// 按背景图重新生成四个色位（详见 themes/monet.ts）。
///
/// 深浅档跟随**当前编辑态的卡片底色**，不跟随图片本身：图拍得暗不代表主题要
/// 跟着变暗，面板的明暗是用户选预设定的。卡片底色解析不出来时按浅色处理，
/// 与主题 store 判明暗的口径一致（isDarkTheme）。
async function monetize() {
  if (!draft.bg_image) return;
  monetizing.value = true;
  try {
    const { colors, achromatic } = await monetFromImage(
      draft.bg_image,
      isDarkTheme(draft.colors.bg_card),
    );
    Object.assign(draft.colors, colors);
    // 配色已经不是任何内置预设了，下拉框跟着清空（与导入主题包一致）
    presetId.value = "";
    ElMessage.success(
      achromatic ? "这张图没有明显色彩，已按灰度配色" : "已按背景图取色，保存后生效",
    );
  } catch (e) {
    ElMessage.error(e instanceof Error ? e.message : "取色失败");
  } finally {
    monetizing.value = false;
  }
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
  padding: var(--sp-4) var(--sp-5);
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
