import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";

/// 主题配置（主题包 JSON 结构）。所有字段可选，缺省 = 沿用 theme.css 默认值。
/// 面板只有单一主题（无亮/暗模式），颜色定制直接应用到 :root。
export interface ThemeConfig {
  version?: number;
  name?: string;
  /// 全局圆角（px）。theme.css 里所有圆角都引用 --radius，改这里全站生效。
  radius?: number;
  /// 卡片/面板投影开关。缺省（含未提供）= true，沿用 theme.css 的默认投影；
  /// false 时把全站唯一投影变量 --panel-card-shadow 归零。
  shadow?: boolean;
  /// 颜色覆盖
  colors?: ThemeColors;
  /// 背景图（data URL，内嵌在配置里，不落盘）
  bg_image?: string;
}

export interface ThemeColors {
  /// 主色（按钮/选中块/焦点）
  primary?: string;
  /// 页面底色（内容区背景）
  bg_page?: string;
  /// 卡片/面板底色（侧栏、顶栏、表格）
  bg_card?: string;
  /// 主文本色
  text?: string;
}

/// 把 hex 颜色解析成 [r,g,b]；非 #rrggbb 返回 null
function hexToRgb(hex: string): [number, number, number] | null {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

/// 从 from 向 to 按 ratio 混合（0=from，1=to）；任一非法 hex 返回 from
function mix(from: string, to: string, ratio: number): string {
  const a = hexToRgb(from);
  const b = hexToRgb(to);
  if (!a || !b) return from;
  const out = a.map((v, i) => Math.round(v + (b[i] - v) * ratio));
  return rgbToHex(out[0], out[1], out[2]);
}

function rgbToHex(r: number, g: number, b: number): string {
  const h = (v: number) => v.toString(16).padStart(2, "0");
  return `#${h(r)}${h(g)}${h(b)}`;
}

function rgbTriplet(hex: string): string {
  const rgb = hexToRgb(hex);
  return rgb ? rgb.join(", ") : "";
}

/// 亮度（0-255），用于判断卡片底色偏深还是偏浅，决定派生色的淡出方向
function luminance(hex: string): number {
  const rgb = hexToRgb(hex);
  return rgb ? 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2] : 128;
}

/// 主题是深色还是浅色：由卡片底色判断。这个判断同时决定了两件事 ——
/// 所有派生色往哪个方向淡出（这里），以及莫奈取色该展开成深色档还是
/// 浅色档（设置页）。规则只有一处，所以导出给设置页复用。
export function isDarkTheme(bgCard: string | null | undefined): boolean {
  return !!bgCard && luminance(bgCard) < 128;
}

/// EP 主色变体的混合比例（与 Element Plus 官方算法一致）
const LIGHT_RATIOS: [string, number][] = [
  ["light-3", 0.3],
  ["light-5", 0.5],
  ["light-7", 0.7],
  ["light-8", 0.8],
  ["light-9", 0.9],
];

/// 输出某个颜色的完整变体集（light-3..9 / dark-2 / rgb）。
/// fade 是淡出目标：亮色主题混向白、暗色主题混向黑 —— EP 官方暗色主题
/// 的 light-N 就是往深色背景方向混，若一律混向白，暗色主题下按钮/标签
/// 的浅色变体会变成刺眼的白块。
function pushColorVars(lines: string[], name: string, hex: string, fade: string) {
  lines.push(`--el-color-${name}: ${hex};`);
  for (const [sfx, ratio] of LIGHT_RATIOS) {
    lines.push(`--el-color-${name}-${sfx}: ${mix(hex, fade, ratio)};`);
  }
  lines.push(
    `--el-color-${name}-dark-2: ${mix(hex, fade === "#ffffff" ? "#000000" : "#ffffff", 0.2)};`,
  );
  const tri = rgbTriplet(hex);
  if (tri) lines.push(`--el-color-${name}-rgb: ${tri};`);
}

/// 颜色白名单：#rrggbb（6 位十六进制）——其余值一律不进 CSS（P1-2，
/// 与后端 validate_theme 规则一致，双端校验）
function isHexColor(s: unknown): s is string {
  return typeof s === "string" && /^#[0-9a-fA-F]{6}$/.test(s);
}

/// 圆角白名单：有限数值 clamp 到 0..64，其余视为未提供（P1-2）
function safeRadius(r: unknown): number | null {
  if (typeof r !== "number" || !Number.isFinite(r)) return null;
  return Math.min(64, Math.max(0, r));
}

/// 背景图白名单（P1-2）：仅允许 data:image/ 前缀的 data URL，且不含
/// 引号/右括号/反斜杠/控制字符 —— 这些字符可闭合 CSS 的 url("...")
/// 字符串构成样式注入逃逸，出现即整个字段丢弃。
function safeBgImage(s: unknown): string | null {
  if (typeof s !== "string" || !s.startsWith("data:image/")) return null;
  if (/[")\\]/.test(s) || /[\x00-\x1f]/.test(s)) return null;
  return s;
}

/// 把主题配置翻译成注入用的 CSS 文本；空配置返回空串（不注入）。
/// P1-2：所有字段注入前过白名单，非法值直接丢弃（该条 CSS 变量不生成），
/// 防止服务端或导入主题包里的恶意值进入样式上下文。
export function buildThemeCss(cfg: ThemeConfig): string {
  const lines: string[] = [];
  const radius = safeRadius(cfg.radius);
  if (radius !== null) {
    lines.push(`--radius: ${radius}px;`);
  }
  const c = cfg.colors ?? {};
  const primary = isHexColor(c.primary) ? c.primary : null;
  const bgCard = isHexColor(c.bg_card) ? c.bg_card : null;
  const bgPage = isHexColor(c.bg_page) ? c.bg_page : null;
  const text = isHexColor(c.text) ? c.text : null;
  // 主题明暗由卡片底色判断：决定所有派生色的淡出方向
  const isDark = isDarkTheme(bgCard);
  const fade = isDark ? "#000000" : "#ffffff";

  // 阴影按同一套三档层级输出，但深色下整体加浓：黑投影落在深色底上对比极弱，
  // 照浅色那套画等于没画，而浮层与模态恰恰最依赖这层投影交代边界
  // （面板无描边，浮层下面又常常没有遮罩）。三档的含义见 theme.css。
  const shadows = isDark
    ? ["0 1px 2px rgb(0 0 0 / 30%)", "0 6px 16px rgb(0 0 0 / 45%)", "0 16px 40px rgb(0 0 0 / 60%)"]
    : ["0 1px 2px rgb(0 0 0 / 6%)", "0 4px 12px rgb(0 0 0 / 10%)", "0 12px 32px rgb(0 0 0 / 18%)"];
  // shadow === false 只关掉贴面那一档 —— 卡片与页面齐平。浮层和模态的投影
  // 是功能性的：少了它，对话框、下拉菜单与背景的边界在无描边界面里说不清。
  lines.push(`--panel-shadow-1: ${cfg.shadow === false ? "none" : shadows[0]};`);
  lines.push(`--panel-shadow-2: ${shadows[1]};`);
  lines.push(`--panel-shadow-3: ${shadows[2]};`);

  // 接缝影：面板外框（顶栏下沿 / 侧栏右沿）与内容区之间那两条硬色阶。
  // 浓度与贴面档一致、也随同一个「阴影」开关关闭 —— 它表达的就是
  // 「面板贴在页面上」这件事。12px 偏移 / -12px spread / 10px blur 这组搭配
  // 与浅深两档为什么是这个百分数，见 theme.css 的 --panel-shadow-seam*。
  const seamAlpha = isDark ? "60%" : "12%";
  const seamTop = `inset 0 12px 10px -12px rgb(0 0 0 / ${seamAlpha})`;
  const seamLeft = `inset 12px 0 10px -12px rgb(0 0 0 / ${seamAlpha})`;
  const seamOff = cfg.shadow === false ? "none" : null;
  // 两个令牌各自都是合法值（开关关闭时都是 none，不会被拼成非法的 `none, none`）
  lines.push(`--panel-shadow-seam: ${seamOff ?? `${seamTop}, ${seamLeft}`};`);
  lines.push(`--panel-shadow-seam-top: ${seamOff ?? seamTop};`);

  // 文字托底影（第 4 类层次，与上面三档独立）：深色主题下亮字落在深底上，
  // 托一层更深的柔影把字形衬出来；浅色主题是暗字落浅底，影一浓就发脏，
  // 只留最淡的一层保持「文字有托底」的语言一致。它不受「阴影」开关控制 ——
  // 那个开关只管贴面卡，文字这一层是功能性的（花色底上分不清字形就失去可读性）。
  lines.push(
    `--panel-text-shadow: ${isDark ? "0 1px 2px rgb(0 0 0 / 45%)" : "0 1px 1px rgb(0 0 0 / 8%)"};`,
  );

  if (bgCard) {
    lines.push(`--el-bg-color: ${bgCard};`);
    lines.push(`--el-bg-color-overlay: ${bgCard};`);
    lines.push(`--el-fill-color-blank: ${bgCard};`);
  }
  if (bgPage) {
    lines.push(`--el-bg-color-page: ${bgPage};`);
  } else if (bgCard && text) {
    // 没单独给页面底色时，从卡片色向文本色微微偏移派生
    lines.push(`--el-bg-color-page: ${mix(bgCard, text, 0.04)};`);
  }
  if (text) {
    lines.push(`--el-text-color-primary: ${text};`);
    // 派生文本层级（regular/secondary/placeholder/disabled）：
    // 从文本色向卡片底色逐级混合。不派生的话，暗色主题下这些
    // 亮色基底值（#333/#777/#999…）会和深底糊在一起或糊成一片。
    if (bgCard) {
      lines.push(`--el-text-color-regular: ${mix(text, bgCard, 0.2)};`);
      lines.push(`--el-text-color-secondary: ${mix(text, bgCard, 0.45)};`);
      lines.push(`--el-text-color-placeholder: ${mix(text, bgCard, 0.58)};`);
      lines.push(`--el-text-color-disabled: ${mix(text, bgCard, 0.72)};`);
      // 填充色阶（表头/hover/输入框底）：从卡片底色向文本色逐级混合
      lines.push(`--el-fill-color-lighter: ${mix(bgCard, text, 0.03)};`);
      lines.push(`--el-fill-color-light: ${mix(bgCard, text, 0.06)};`);
      lines.push(`--el-fill-color: ${mix(bgCard, text, 0.1)};`);
      lines.push(`--el-fill-color-dark: ${mix(bgCard, text, 0.14)};`);
      lines.push(`--el-fill-color-darker: ${mix(bgCard, text, 0.18)};`);
    }
  }
  if (primary) {
    pushColorVars(lines, "primary", primary, fade);
    // 语义色默认与主色同值（黑白灰设计），定制主色时一并跟随，
    // 否则暗色主题下 danger/success 还是近黑，按钮直接隐形
    pushColorVars(lines, "success", primary, fade);
    pushColorVars(lines, "danger", primary, fade);
    pushColorVars(lines, "error", primary, fade);
  }
  // warning/info 默认是中灰（文本色向背景混合约 0.4），定制时同样派生
  if (text && bgCard) {
    const mid = mix(text, bgCard, 0.4);
    pushColorVars(lines, "warning", mid, fade);
    pushColorVars(lines, "info", mid, fade);
  }
  const bgImage = safeBgImage(cfg.bg_image);
  if (bgImage) lines.push(`--panel-bg-image: url("${bgImage}");`);
  if (!lines.length) return "";
  return `:root {\n${lines.join("\n")}\n}`;
}

/// 注入/替换单个 <style>，保证级联顺序在打包 CSS 之后（后者胜）
function injectStyle(css: string) {
  let el = document.getElementById("panel-theme") as HTMLStyleElement | null;
  if (!css) {
    if (el) el.textContent = "";
    return;
  }
  if (!el) {
    el = document.createElement("style");
    el.id = "panel-theme";
    document.head.appendChild(el);
  }
  el.textContent = css;
}

// 主题 store：主题定制（颜色/圆角/背景），定制持久化到后端 settings 表
export const useThemeStore = defineStore("theme", () => {
  /// 当前生效的主题配置（null = 未定制，用默认）
  const config = ref<ThemeConfig | null>(null);

  /// 把配置注入页面（不写库）
  function applyConfig(cfg: ThemeConfig | null) {
    config.value = cfg;
    injectStyle(cfg ? buildThemeCss(cfg) : "");
  }

  /// 登录后从服务端拉取定制配置
  async function load() {
    try {
      const { data } = await http.get("/theme");
      applyConfig(data?.config ?? null);
    } catch {
      // 拉取失败保持默认样式，不打断页面
    }
  }

  /// 保存配置到服务端并即时生效；传 null 恢复默认
  async function save(cfg: ThemeConfig | null) {
    await http.post("/theme", cfg ?? null);
    applyConfig(cfg);
  }

  return { config, applyConfig, load, save };
});
