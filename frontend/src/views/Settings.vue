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

        <div class="card">
          <div class="card-title">安全入口</div>
          <div class="card-sub">
            给面板加一道登录之前的闸门：只有从指定路径前缀访问、且来源 IP 在白名单内
            的请求才放行。两项都留空即关闭。请谨慎配置 —— 填错可能把自己也挡在门外，
            修改后须立刻换用新地址访问。
          </div>

          <el-form label-position="top" class="form">
            <el-form-item label="访问路径前缀">
              <el-input
                v-model="sec.entrance"
                placeholder="留空为关闭；如填 lyys，则改从 /lyys/ 访问"
                clearable
              />
              <div class="key-hint">
                4–64 位，仅限字母、数字、下划线、连字符；不可占用 api、health、
                assets、fonts 等保留字。
              </div>
            </el-form-item>
            <el-form-item label="IP 白名单">
              <el-input
                v-model="sec.allowlist"
                type="textarea"
                :rows="3"
                resize="vertical"
                placeholder="留空为不限制；每行一个，支持单个 IP 或 CIDR，如 192.168.1.0/24"
              />
              <div class="key-hint">
                每行（或逗号分隔）一条，IPv4 / IPv6 均可。仅在直连或本机反向代理
                场景下按真实来源判断，代理转发的 X-Forwarded-For 仅在请求来自本机时采信。
              </div>
            </el-form-item>
            <el-form-item>
              <el-button type="primary" :loading="secSaving" @click="saveSecurity">
                保存安全入口
              </el-button>
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

            <div class="row">
              <label>阴影</label>
              <el-checkbox v-model="draft.shadow">卡片使用投影</el-checkbox>
            </div>
            <div class="hint">
              面板没有描边，靠底色差与投影分层。这一项只管卡片、内容块这类贴面元素：
              关掉后它们与页面底色直接相接；对话框、抽屉、下拉菜单等浮层仍保留投影
              —— 少了它，浮层与背景的边界在无描边界面里说不清。
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
                <!-- 位置与缩放：滑杆放在气泡里，拖的时候看的就是内容区那张真图 -->
                <el-popover
                  v-if="draft.bg_image"
                  placement="bottom-start"
                  :width="250"
                  trigger="click"
                  popper-class="bg-adjust-pop"
                >
                  <template #reference>
                    <el-button size="small">位置与大小</el-button>
                  </template>
                  <div class="bg-adjust">
                    <div class="bg-adjust-item">
                      <span>大小 {{ draft.bg_zoom }}%</span>
                      <el-slider
                        v-model="draft.bg_zoom"
                        :min="100"
                        :max="300"
                        :step="5"
                        :show-tooltip="false"
                      />
                    </div>
                    <div class="bg-adjust-item">
                      <span>左右 {{ draft.bg_x }}%</span>
                      <el-slider
                        v-model="draft.bg_x"
                        :min="0"
                        :max="100"
                        :step="1"
                        :show-tooltip="false"
                      />
                    </div>
                    <div class="bg-adjust-item">
                      <span>上下 {{ draft.bg_y }}%</span>
                      <el-slider
                        v-model="draft.bg_y"
                        :min="0"
                        :max="100"
                        :step="1"
                        :show-tooltip="false"
                      />
                    </div>
                    <div class="bg-adjust-foot">
                      <el-button link size="small" @click="resetBgAdjust">重置</el-button>
                    </div>
                  </div>
                </el-popover>
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
            <div v-if="draft.bg_image" class="hint">
              铺满内容区并随页面固定；点「位置与大小」调缩放与取景，拖动时即时预览，
              满意后按下面的「保存主题」。
            </div>
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

        <div class="card">
          <div class="card-title">界面字体</div>
          <div class="card-sub">
            默认使用系统字体。面板不再内置第三方字体（中文字体单文件就近 8MB，
            多字重会显著增大程序体积）。如需统一界面字体，请自行获取字体文件
            —— 并遵守该字体自身的许可协议 —— 在此上传：文件保存在服务器上，
            仅本面板使用。
          </div>

          <div class="form">
            <div class="row">
              <label>字体名称</label>
              <el-input
                v-model="fontDraft.family"
                placeholder="如 HarmonyOS Sans SC（须与字体内部名称一致）"
                maxlength="64"
              />
            </div>

            <div class="row">
              <label>添加字重</label>
              <div class="btns">
                <el-select v-model="fontWeight" style="width: 110px">
                  <el-option v-for="w in FONT_WEIGHTS" :key="w" :label="`${w}`" :value="w" />
                </el-select>
                <el-button size="small" :loading="fontUploading" @click="pickFont">
                  选择字体文件
                </el-button>
                <input
                  ref="fontInput"
                  type="file"
                  accept=".ttf,.otf,.woff,.woff2"
                  class="hidden-file"
                  @change="onFontFile"
                />
              </div>
            </div>

            <div v-if="fontDraft.faces.length" class="face-list">
              <div v-for="f in fontDraft.faces" :key="f.weight" class="face-row">
                <span class="face-weight">{{ f.weight }}</span>
                <span class="face-file">{{ f.file }}</span>
                <el-button link type="danger" size="small" @click="removeFace(f.weight)">
                  移除
                </el-button>
              </div>
            </div>
            <div v-else class="hint">尚未上传字体文件。至少上传一个字重才能保存。</div>

            <div class="row">
              <label></label>
              <div class="btns">
                <el-button type="primary" :loading="fontSaving" @click="saveFont">
                  保存字体
                </el-button>
                <el-button :disabled="fontSaving" @click="resetFont">恢复系统字体</el-button>
              </div>
            </div>
            <div class="hint">
              字重按 CSS 数值对应文件（400 常规 / 500 中等 / 700 粗体）；同一字重重复上传
              会覆盖。保存后对所有设备生效。「恢复系统字体」会一并删除已上传的字体文件。
            </div>
          </div>
        </div>
      </el-tab-pane>
    </el-tabs>
  </div>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import http from "../api/http";
import {
  clearBgPreview,
  isDarkTheme,
  previewBg,
  useThemeStore,
  type ThemeColors,
  type ThemeConfig,
} from "../stores/theme";
import { useFontStore, type FontFace } from "../stores/font";
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
    ElMessage.error(e.response?.data?.error || "保存失败");
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
    ElMessage.error(e.response?.data?.error || "操作失败");
  } finally {
    saving.value = false;
  }
}

/* ---------------- 系统设置：安全入口（访问前缀 + IP 白名单） ---------------- */

const sec = reactive({ entrance: "", allowlist: "" });
const secSaving = ref(false);

async function loadSecurity() {
  const { data } = await http.get<{ entrance: string; allowlist: string }>("/security");
  sec.entrance = data.entrance || "";
  sec.allowlist = data.allowlist || "";
}

async function saveSecurity() {
  secSaving.value = true;
  try {
    const { data } = await http.post<{ entrance: string; allowlist: string }>("/security", {
      entrance: sec.entrance.trim(),
      allowlist: sec.allowlist,
    });
    sec.entrance = data.entrance;
    sec.allowlist = data.allowlist;
    if (data.entrance) {
      ElMessage.success(`已保存，请改用 ${location.origin}/${data.entrance}/ 访问面板`);
    } else {
      ElMessage.success("已保存，安全入口已关闭");
    }
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error || "保存失败");
  } finally {
    secSaving.value = false;
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
  shadow: boolean;
  colors: DraftColors;
  bg_image: string | null;
  /// 背景图缩放（百分数，100 = 铺满视口）
  bg_zoom: number;
  /// 背景图位置（百分数，50 = 居中）
  bg_x: number;
  bg_y: number;
}

function emptyDraft(): Draft {
  return {
    name: "自定义主题",
    radius: 6,
    shadow: true,
    colors: { primary: null, bg_page: null, bg_card: null, text: null },
    bg_image: null,
    bg_zoom: 100,
    bg_x: 50,
    bg_y: 50,
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
    // 缺省视为开：老主题包没有这个字段，行为应与从前（有投影）一致
    if (typeof cfg.shadow === "boolean") d.shadow = cfg.shadow;
    if (cfg.colors) Object.assign(d.colors, pickColors(cfg.colors));
    if (cfg.bg_image) d.bg_image = cfg.bg_image;
    if (typeof cfg.bg_zoom === "number") d.bg_zoom = cfg.bg_zoom;
    if (typeof cfg.bg_x === "number") d.bg_x = cfg.bg_x;
    if (typeof cfg.bg_y === "number") d.bg_y = cfg.bg_y;
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
  cfg.shadow = draft.shadow;
  const colors = compact(draft.colors);
  if (Object.keys(colors).length) cfg.colors = colors;
  if (draft.bg_image) {
    cfg.bg_image = draft.bg_image;
    // 位置/缩放只在有图时有意义，跟着图一起写（没图时不留这三个零值字段）
    cfg.bg_zoom = draft.bg_zoom;
    cfg.bg_x = draft.bg_x;
    cfg.bg_y = draft.bg_y;
  }
  return cfg;
}

function compact(c: DraftColors): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(c)) if (v) out[k] = v;
  return out;
}

// 主题配置由 AppLayout 异步拉取，到位后同步进编辑态
watch(() => theme.config, (cfg) => fromConfig(cfg), { immediate: true });

/* 背景图的即时预览。
 * 颜色/圆角要等「保存主题」才生效，背景的位置与缩放却必须边拖边看 ——
 * 拖动滑杆时先把这套几何注入页面（不写库），满意了再保存。预览注入的是
 * 与主题完全相同的算式（stores/theme.ts 的 bgVars），所以「拖的时候看到的」
 * 就是「保存后生效的」。离开本页时清掉，避免未保存的几何留在页面上。 */
watch(
  () => [draft.bg_image, draft.bg_zoom, draft.bg_x, draft.bg_y],
  () =>
    previewBg({
      version: 1,
      bg_image: draft.bg_image ?? undefined,
      bg_zoom: draft.bg_zoom,
      bg_x: draft.bg_x,
      bg_y: draft.bg_y,
    }),
  { immediate: true },
);
onBeforeUnmount(clearBgPreview);

/// 位置与缩放回到出厂值（图片本身不动）
function resetBgAdjust() {
  draft.bg_zoom = 100;
  draft.bg_x = 50;
  draft.bg_y = 50;
}

function applyPreset(id: string) {
  const p = PRESETS.find((x) => x.id === id);
  if (!p) return;
  const shadow = draft.shadow;
  // 预设显式给了 radius（如高对比=0）则用预设值，否则保留用户现值
  fromConfig(p.config, p.config.radius === undefined);
  // 投影开关不属于配色，选预设不改动它
  if (p.config.shadow === undefined) draft.shadow = shadow;
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

/* ---------------- 界面设置：字体 ---------------- */

const font = useFontStore();

/// 上传时可选的 CSS 字重
const FONT_WEIGHTS = [100, 200, 300, 400, 500, 600, 700, 800, 900];

interface FontDraft {
  family: string;
  faces: FontFace[];
}

const fontDraft = reactive<FontDraft>({ family: "", faces: [] });
const fontWeight = ref(400);
const fontUploading = ref(false);
const fontSaving = ref(false);
const fontInput = ref<HTMLInputElement>();

// 字体配置由 AppLayout 异步拉取，到位后同步进编辑态
watch(
  () => font.config,
  (cfg) => {
    fontDraft.family = cfg?.family ?? "";
    fontDraft.faces = (cfg?.faces ?? [])
      .map((f) => ({ ...f }))
      .sort((a, b) => a.weight - b.weight);
  },
  { immediate: true },
);

function pickFont() {
  fontInput.value?.click();
}

function sortFaces(faces: FontFace[]): FontFace[] {
  return faces.sort((a, b) => a.weight - b.weight);
}

async function onFontFile(e: Event) {
  const input = e.target as HTMLInputElement;
  const file = input.files?.[0];
  // 清空 value，否则连续选同一个文件不会再触发 change
  input.value = "";
  if (!file) return;
  fontUploading.value = true;
  try {
    const name = await font.upload(file);
    // 同一字重覆盖：一个字重只留一个文件
    const faces = fontDraft.faces.filter((f) => f.weight !== fontWeight.value);
    faces.push({ weight: fontWeight.value, file: name });
    fontDraft.faces = sortFaces(faces);
    // 首次上传且没填名称时，用文件名兜个底（用户可改）
    if (!fontDraft.family.trim()) {
      fontDraft.family = file.name.replace(/\.(ttf|otf|woff2?)$/i, "");
    }
    ElMessage.success(`已上传：${name}`);
  } catch (err: unknown) {
    const e2 = err as { response?: { data?: { error?: string } } };
    ElMessage.error(e2.response?.data?.error ?? "上传失败");
  } finally {
    fontUploading.value = false;
  }
}

function removeFace(weight: number) {
  fontDraft.faces = fontDraft.faces.filter((f) => f.weight !== weight);
}

async function saveFont() {
  const family = fontDraft.family.trim();
  if (!family) {
    ElMessage.warning("请填写字体名称");
    return;
  }
  if (!fontDraft.faces.length) {
    ElMessage.warning("请至少上传一个字重文件");
    return;
  }
  fontSaving.value = true;
  try {
    await font.save({ version: 1, family, faces: fontDraft.faces });
    ElMessage.success("字体已保存并对所有设备生效");
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "保存失败");
  } finally {
    fontSaving.value = false;
  }
}

async function resetFont() {
  fontSaving.value = true;
  try {
    await font.save(null);
    ElMessage.success("已恢复系统字体");
  } catch (e: unknown) {
    const err = e as { response?: { data?: { error?: string } } };
    ElMessage.error(err.response?.data?.error ?? "操作失败");
  } finally {
    fontSaving.value = false;
  }
}

onMounted(() => {
  load().catch(() => ElMessage.error("配置加载失败"));
  loadSecurity().catch(() => ElMessage.error("安全入口配置加载失败"));
});
</script>

<style scoped>
/* 🔴 设置页撑满 .content，让滚动收进页签主体 —— 与列表页同一套
   「框架不滚、内容滚」的语言（见 theme.css 里那条 :has() 规则）。
   不加这三条的话，页签内容一旦高出可用高度，长高的就是 .settings
   自己，撑破 .content 的内容盒、在整块内容区右侧多出一条滚动条，
   顶栏下的整页跟着滚 —— 而除仪表盘外，面板其余页面都不滚，这是刻意
   留出的直观感（设置页是唯一的例外，此前一直漏了这层约束）。 */
.settings {
  height: 100%;
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.settings :deep(.el-tabs) {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
}
/* 页签栏按内容占高、卡片区吃剩下的高度：窗口再矮也只滚卡片区，
   页签栏保持不动。内容装得下时这里不出滚动条。 */
.settings :deep(.el-tabs__content) {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

.card {
  background: var(--el-bg-color);
  border-radius: var(--radius, 8px);
  /* 纵向内距比全局卡片再紧一档：设置页两列卡片里总有一列特别长，
     这里省下的 8px 直接决定底部那颗按钮落不落在折线上。 */
  padding: var(--sp-3) var(--sp-5);
  /* 与仪表盘卡片同一档：全站「白块浮在底色上」的语言只此一套 */
  box-shadow: var(--panel-shadow-1);
  /* 栅格子项默认 min-width:auto，长文件名一类的不可折内容会把卡片撑破 */
  min-width: 0;
}
/* 每个页签里的卡片走响应式栅格：窄屏一列堆叠，宽屏并排。
   此前卡片是相邻的块级元素、彼此没有外边距，两块白底直接贴在一起，
   只靠投影勉强分界；宽屏时右侧还空着一大片。改为按内容最小宽自动分列后，
   既有明确间隔，也把横向空间用起来。 */
.settings :deep(.el-tab-pane) {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(400px, 1fr));
  gap: var(--sp-4);
  /* 不拉平高度：宁可两块高度不一，也不要短卡片被撑出大片空白 */
  align-items: start;
}
/* 纵向节奏再收紧一档（--sp-3 → --sp-2）：设置页是面板里最长的一页表单，
   字段数量多；Element Plus 默认的表单项/页签栏间距是按通用页面给的。
   上一版收到 --sp-3 时刚好落进一屏，之后外框留白（--frame-gap × 3 = 30px）
   从内容区拿走了一截，两个页签又双双多出 ~35px、底部的按钮正好压在折线上。
   字段之间的呼吸感靠标签与内距已经够了，这一档让出去才重新合上一屏。 */
.settings :deep(.el-tabs__header) {
  margin-bottom: var(--sp-2);
}
.settings :deep(.el-form-item) {
  margin-bottom: var(--sp-2);
}
/* 顶置标签的 22px 行高是固定的，EP 默认再往下补 8px；标签与它自己的
   输入框本来就贴在一起读，这 8 全是富余，收一档。 */
.settings :deep(.el-form-item__label) {
  margin-bottom: var(--sp-1);
}
.card-title {
  font-size: 14px;
  font-weight: 600;
  margin-bottom: var(--sp-3);
}
.card-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-bottom: var(--sp-2);
  line-height: 1.6;
}
.form {
  margin-top: 0;
}
/* 卡片占满内容区，但输入控件保持可读宽度 */
.settings :deep(.el-form-item__content) {
  max-width: 480px;
}
.key-hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: var(--sp-1);
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
/* 界面设置表单行（横向 gap 保持 --sp-3：标签与控件之间的距离；
   纵向下边距与上面两处一起收到 --sp-2，两个页签才同时合得上一屏） */
.settings .form .row {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  margin-bottom: var(--sp-2);
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
.settings .form .row > .el-select,
.settings .form .row > .el-checkbox {
  flex: 1;
}
.btns {
  display: flex;
  gap: var(--sp-2);
}
.group {
  font-size: 12px;
  font-weight: 500;
  color: var(--el-text-color-primary);
  margin: var(--sp-1) 0 var(--sp-3);
}
.hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-bottom: var(--sp-3);
}
.hidden-file {
  display: none;
}
/* 已上传的字重文件列表 */
.face-list {
  max-width: 480px;
  margin-bottom: var(--sp-3);
}
.face-row {
  display: flex;
  align-items: center;
  gap: var(--sp-3);
  padding: var(--sp-2) var(--sp-3);
  border-radius: var(--radius, 6px);
  background: var(--el-fill-color-light);
  font-size: 12px;
}
.face-row + .face-row {
  margin-top: var(--sp-1);
}
.face-weight {
  flex: none;
  width: 40px;
  color: var(--el-text-color-secondary);
  font-variant-numeric: tabular-nums;
}
.face-file {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--panel-mono);
}
</style>

<!-- 气泡里的内容被 EP 传送到 body，scoped 样式够不到，只能另起一个
     不加 scoped 的块；`.bg-adjust` 是这段的唯一入口类名，不污染别处。 -->
<style>
.bg-adjust-pop {
  padding: var(--sp-3) var(--sp-4);
}
.bg-adjust-item + .bg-adjust-item {
  margin-top: var(--sp-1);
}
.bg-adjust-item > span {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  font-variant-numeric: tabular-nums;
}
/* 滑杆上下各留 6：EP 默认给 6px，加上下面那条鞋垫线共 18px，
   三条并排时气泡会显得很空，收成 2px 让三组读起来是一块 */
.bg-adjust-item .el-slider {
  margin: 2px 0;
}
.bg-adjust-foot {
  display: flex;
  justify-content: flex-end;
  margin-top: var(--sp-1);
}
</style>
