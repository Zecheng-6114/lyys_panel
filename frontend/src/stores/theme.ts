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

/// 把主题配置翻译成注入用的 CSS 文本；空配置返回空串（不注入）
export function buildThemeCss(cfg: ThemeConfig): string {
  const lines: string[] = [];
  if (typeof cfg.radius === "number") {
    lines.push(`--radius: ${cfg.radius}px;`);
  }
  const c = cfg.colors;
  // 主题明暗由卡片底色判断：决定所有派生色的淡出方向
  const isDark = !!c?.bg_card && luminance(c.bg_card) < 128;
  const fade = isDark ? "#000000" : "#ffffff";

  if (c?.bg_card) {
    lines.push(`--el-bg-color: ${c.bg_card};`);
    lines.push(`--el-bg-color-overlay: ${c.bg_card};`);
    lines.push(`--el-fill-color-blank: ${c.bg_card};`);
  }
  if (c?.bg_page) {
    lines.push(`--el-bg-color-page: ${c.bg_page};`);
  } else if (c?.bg_card && c?.text) {
    // 没单独给页面底色时，从卡片色向文本色微微偏移派生
    lines.push(`--el-bg-color-page: ${mix(c.bg_card, c.text, 0.04)};`);
  }
  if (c?.text) {
    lines.push(`--el-text-color-primary: ${c.text};`);
    // 派生文本层级（regular/secondary/placeholder/disabled）：
    // 从文本色向卡片底色逐级混合。不派生的话，暗色主题下这些
    // 亮色基底值（#333/#777/#999…）会和深底糊在一起或糊成一片。
    if (c?.bg_card) {
      lines.push(`--el-text-color-regular: ${mix(c.text, c.bg_card, 0.2)};`);
      lines.push(`--el-text-color-secondary: ${mix(c.text, c.bg_card, 0.45)};`);
      lines.push(`--el-text-color-placeholder: ${mix(c.text, c.bg_card, 0.58)};`);
      lines.push(`--el-text-color-disabled: ${mix(c.text, c.bg_card, 0.72)};`);
      // 填充色阶（表头/hover/输入框底）：从卡片底色向文本色逐级混合
      lines.push(`--el-fill-color-lighter: ${mix(c.bg_card, c.text, 0.03)};`);
      lines.push(`--el-fill-color-light: ${mix(c.bg_card, c.text, 0.06)};`);
      lines.push(`--el-fill-color: ${mix(c.bg_card, c.text, 0.1)};`);
      lines.push(`--el-fill-color-dark: ${mix(c.bg_card, c.text, 0.14)};`);
      lines.push(`--el-fill-color-darker: ${mix(c.bg_card, c.text, 0.18)};`);
    }
  }
  if (c?.primary) {
    pushColorVars(lines, "primary", c.primary, fade);
    // 语义色默认与主色同值（黑白灰设计），定制主色时一并跟随，
    // 否则暗色主题下 danger/success 还是近黑，按钮直接隐形
    pushColorVars(lines, "success", c.primary, fade);
    pushColorVars(lines, "danger", c.primary, fade);
    pushColorVars(lines, "error", c.primary, fade);
  }
  // warning/info 默认是中灰（文本色向背景混合约 0.4），定制时同样派生
  if (c?.text && c?.bg_card) {
    const mid = mix(c.text, c.bg_card, 0.4);
    pushColorVars(lines, "warning", mid, fade);
    pushColorVars(lines, "info", mid, fade);
  }
  if (cfg.bg_image) lines.push(`--panel-bg-image: url("${cfg.bg_image}");`);
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
