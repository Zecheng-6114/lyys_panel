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
  /// 背景图缩放（百分数）。100 = 铺满视口，上限 300 —— 见 bgSize 的推法。
  bg_zoom?: number;
  /// 背景图位置（百分数）：0 靠一边、50 居中、100 靠另一边。
  /// 只在缩放后仍有多余量的那个方向上起作用（铺满的方向没有可移动的余量）。
  bg_x?: number;
  bg_y?: number;
  /// 不透明度（百分数）：100 = 完全不透明，30 = 最透。**一个值管两处** ——
  /// 仪表盘卡片（--panel-card-bg）与应用外壳（左侧菜单栏、上方控件、内容区底板）。
  /// 面板是「整屏浮在背景图上」的一套表面，卡片与外壳共用一个值才不会出现
  /// 「卡片透了、外壳还闷着」的割裂感。
  card_opacity?: number;
  /// 高斯模糊半径（px，`backdrop-filter: blur()`）：卡片与外壳背后那层背景被糊掉
  /// —— 磨砂玻璃观感。0 = 不模糊。与不透明度是两件事：不透明度决定「透多少」，
  /// 模糊决定「透出来的是不是糊的」。
  card_blur?: number;
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

/// 固定的危险色（Element Plus 的默认 --el-color-danger）。
///
/// 🔴 不跟主色派生：破坏性操作（结束进程、删除确认）必须与普通操作可分辨，
/// 定制成浅色主色后若同值，危险按钮就和普通按钮长得一样。
const DANGER_HEX = "#f56c6c";

/// 压在某个底色上的可读文字色。
///
/// 用于危险色这类**不跟随主题**的固定底色：主题固定下来时能选一次
/// `--el-bg-color`（深色主题的浅底 + 深字），但 danger 是常量红，
/// 浅色主题下就需要深字、深色主题下需要浅字，凭主题猜不对 —— 直接按亮度算。
function onColor(hex: string): string {
  return luminance(hex) > 150 ? "#111111" : "#ffffff";
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

/// 背景缩放白名单（P1-2）：有限数 clamp 到 100..300，其余视为未提供。
/// 下限锁在 100：100% 的含义就是「铺满视口」，再小就会露出底板颜色。
function safeZoom(v: unknown): number | null {
  if (typeof v !== "number" || !Number.isFinite(v)) return null;
  return Math.min(300, Math.max(100, v));
}

/// 背景位移白名单（P1-2）：有限数 clamp 到 0..100（百分数）
function safePos(v: unknown): number | null {
  if (typeof v !== "number" || !Number.isFinite(v)) return null;
  return Math.min(100, Math.max(0, v));
}

/// 不透明度白名单（P1-2）：有限数 clamp 到 30..100（百分数）。
/// 下限锁在 30：再低卡片与外壳就连同底下的背景图糊成一片，其上的读数与投影
/// 都失去依托（与背景缩放下限锁 100 是同一个理由 —— 越界即无意义）。
function safeOpacity(v: unknown): number | null {
  if (typeof v !== "number" || !Number.isFinite(v)) return null;
  return Math.min(100, Math.max(30, v));
}

/// 模糊半径白名单（P1-2）：有限数 clamp 到 0..40（px）。
/// 上限 40：再往上 blur 的采样半径超过卡片自身的短边，卡片背面就只剩一团
/// 均匀色块，磨砂感反而消失，只剩一片脏灰。
function safeBlur(v: unknown): number | null {
  if (typeof v !== "number" || !Number.isFinite(v)) return null;
  return Math.min(40, Math.max(0, v));
}

/// 半透底色：把主体色按不透明度摊开，产出全站唯一的 --panel-card-bg。
/// 卡片与外壳（侧栏 / 顶栏 / 内容区底板）共用这一个变量 —— 它们本就是
/// 「整屏浮在背景图上」的同一套表面，各给一个值只会做出割裂的层次。
/// 只有 100（完全不透明）返回 null —— 此时不生成变量，让 CSS 侧回退到
/// 原来的 `var(--el-bg-color)`，与未定制主题逐像素一致。
/// 底色缺失时不返回 null 而是引用 --el-bg-color：只有生成了某个
/// --el-bg-color* 时主题才会改掉这个色，故缺省时直接引用它就是当时的默认色。
function cardBgVar(hex: string | null, opacity: number | null): string | null {
  if (opacity === null || opacity >= 100) return null;
  const rgb = hex ? hexToRgb(hex) : null;
  const base = rgb ? `rgb(${rgb.join(" ")})` : "var(--el-bg-color)";
  return `--panel-card-bg: color-mix(in srgb, ${base} ${opacity}%, transparent);`;
}

/// 卡片的高斯模糊（毛玻璃）变量：卡片用 backdrop-filter 把**背后**那层背景
/// 糊掉。0 返回 null —— 不生成变量，CSS 侧就没有任何滤镜，与从前逐像素一致。
///
/// 只需 `--panel-card-blur` 一个值：CSS 侧同一条规则里既写标准属性也写
/// `-webkit-` 前缀，所以这里给的 `blur(Npx)` 两边通用，不会再派生一份。
/// 变量存在与否同时充当 CSS 的 `@supports` 降级信号 —— 不支持 backdrop-filter
/// 的浏览器会忽略这个未注册的自定义属性，从而忽略引用它的整条声明，
/// 卡片自然退回「只是半透」而不是变成没有滤镜的透明底板。
function cardBlurVar(blur: number | null): string | null {
  if (blur === null || blur <= 0) return null;
  return `--panel-card-blur: blur(${blur}px);`;
}

/// 背景图长宽比（宽 ÷ 高）。buildThemeCss 是同步的、解码是异步的，所以这里
/// 只缓存「最近解出来的那一张」：命中不了就先按 CSS 的 cover 兜底渲染，解出来
/// 再重注入一次；同一张图只解一次。
let bgAspect: { src: string; ratio: number } | null = null;

/// 解 data URL 的像素尺寸取长宽比；解不出来返回 null（保持 cover 兜底）
function decodeAspect(src: string): Promise<number | null> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () =>
      resolve(
        img.naturalWidth > 0 && img.naturalHeight > 0
          ? img.naturalWidth / img.naturalHeight
          : null,
      );
    img.onerror = () => resolve(null);
    img.src = src;
  });
}

/// 保证这张图的长宽比已经解出来；没解过就异步解一次，解完回调（重渲染样式）。
function ensureAspect(image: string, done: () => void) {
  if (bgAspect?.src === image) return;
  void decodeAspect(image).then((ratio) => {
    if (ratio === null) return;
    bgAspect = { src: image, ratio };
    done();
  });
}

/// 背景尺寸算式：把 cover 展开成显式表达式，再乘缩放系数。
///
/// CSS 的 cover 是按图片实际长宽比算出来的，样式里没法再乘一个系数 ——
/// 所以这里自己算一遍：cover 的宽 = max(视口宽, 视口高 × 图片长宽比)。
/// 用 vw/vh 表达有个前提：背景是 background-attachment: fixed，定位区
/// （positioning area）恰好就是视口本身，100vw/100vh 正是 cover 要比的那两个
/// 尺寸；窗口尺寸变化时浏览器自己重算，不需要 JS 参与。
/// 长宽比未知时返回 null，由调用方退回 CSS 的 cover。
function bgSize(zoom: number, ratio: number | null): string | null {
  if (ratio === null) return null;
  const z = zoom / 100;
  const n = (v: number) => Number(v.toFixed(4));
  return `max(calc(100vw * ${n(z)}), calc(100vh * ${n(z * ratio)})) auto`;
}

/// 背景图相关的 CSS 变量（图 / 尺寸 / 位置）。主题注入与设置页的即时预览
/// 共用这一份 —— 滑杆里看到的算式必须和保存后生效的完全是同一套。
function bgVars(cfg: ThemeConfig): string[] {
  const image = safeBgImage(cfg.bg_image);
  if (!image) return [];
  const out = [`--panel-bg-image: url("${image}");`];
  const zoom = safeZoom(cfg.bg_zoom);
  if (zoom !== null) {
    const size = bgSize(zoom, bgAspect?.src === image ? bgAspect.ratio : null);
    if (size) out.push(`--panel-bg-size: ${size};`);
  }
  const x = safePos(cfg.bg_x);
  const y = safePos(cfg.bg_y);
  if (x !== null || y !== null) {
    out.push(`--panel-bg-position: ${x ?? 50}% ${y ?? 50}%;`);
  }
  return out;
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
    // 浮层仍用不透明底：下拉菜单 / 对话框 / 气泡常常悬在文字之上，
    // 透出背后的正文会让弹层内容难读 —— 这是刻意保留的一处实底。
    lines.push(`--el-bg-color-overlay: ${bgCard};`);
    // 白底变量：EP 的「空白表面」令牌，十几个组件变量都从它取值
    // （复选框 --el-checkbox-bg-color、按钮、卡片、菜单、分页、单选框、
    // 表格、标签、树、日历…）。它默认取 --el-fill-color-blank 的实色，
    // 于是这些控件全成了纯色块 —— 这正是「还有纯色控件」的根因。
    // 按其不透明度摊开，一处托底；缺省 100% 时等价于原来的卡片色。
    lines.push(
      `--el-fill-color-blank: color-mix(in srgb, ${bgCard} var(--panel-surface-opacity, 100%), transparent);`,
    );
  }
  if (bgPage) {
    lines.push(`--el-bg-color-page: ${bgPage};`);
  } else if (bgCard && text) {
    // 没单独给页面底色时，从卡片色向文本色微微偏移派生
    lines.push(`--el-bg-color-page: ${mix(bgCard, text, 0.04)};`);
  }
  // 半透底色：卡片与外壳共用同一个变量。放在底色之后 —— 它要引用的正是上面
  // 这两条（bg_card 缺省时由 color-mix 直接引用 --el-bg-color 兜底）。
  const opacity = safeOpacity(cfg.card_opacity);
  const cardBg = cardBgVar(bgCard, opacity);
  if (cardBg) {
    lines.push(cardBg);
    // 内容区底板与表单控件（输入框/文本域/下拉）也读这同一个不透明度 ——
    // 面板没有各自的滑杆，它们摊的颜色不同、透的档位相同。
    // 值为裸百分数：CSS 侧直接塞进 color-mix() 的百分比槽位。
    lines.push(`--panel-surface-opacity: ${opacity}%;`);
  }
  // 毛玻璃：与上面的半透底色是一对 —— 透出来的东西要糊掉才叫磨砂玻璃
  const cardBlur = cardBlurVar(safeBlur(cfg.card_blur));
  if (cardBlur) lines.push(cardBlur);
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
    // success 仍与主色同值：黑白灰设计里「成功」没有必须区分的语义负担
    pushColorVars(lines, "success", primary, fade);
  }
  // 🔴 danger / error **不跟主色派生**，固定用真实的红。
  //
  // 原先它们与 primary 同值（黑白灰设计）。但破坏性操作的语义必须能一眼分辨：
  // 定制成浅紫主色后，「结束进程」按钮被染成浅紫，与普通按钮无从区分 ——
  // 这正是「面板的杀伤力」最需要清楚的地方。用户明确选择全站 danger 用红。
  //
  // 红值沿用 Element Plus 的 --el-color-danger（#f56c6c）而不是自造：
  // 它已通过对比度检验，且与 EP 自带的错误提示风格一致；
  // light-N 档仍走同一套 mix（暗色主题下混向深底，不会糊成白块）。
  pushColorVars(lines, "danger", DANGER_HEX, fade);
  pushColorVars(lines, "error", DANGER_HEX, fade);
  lines.push(`--panel-on-danger: ${onColor(DANGER_HEX)};`);
  lines.push(`--panel-on-error: ${onColor(DANGER_HEX)};`);
  // warning/info 默认是中灰（文本色向背景混合约 0.4），定制时同样派生
  if (text && bgCard) {
    const mid = mix(text, bgCard, 0.4);
    pushColorVars(lines, "warning", mid, fade);
    pushColorVars(lines, "info", mid, fade);
  }
  lines.push(...bgVars(cfg));
  if (!lines.length) return "";
  return `:root {\n${lines.join("\n")}\n}`;
}

/// 注入/替换单个 <style>，保证级联顺序在打包 CSS 之后（后者胜）。
/// 同 id 复用同一个元素，避免反复插拔；不同 id（预览另占一条）各留一个位置。
function injectStyle(css: string, id = "panel-theme") {
  let el = document.getElementById(id) as HTMLStyleElement | null;
  if (!css) {
    if (el) el.textContent = "";
    return;
  }
  if (!el) {
    el = document.createElement("style");
    el.id = id;
    document.head.appendChild(el);
  }
  el.textContent = css;
}

/// 设置页正在预览的那份配置（null = 没在预览）。字段可能只有背景图几何、
/// 也可能只有卡片不透明度 —— 预览是「当前正在拖的那一项」，不是整份主题。
let previewCfg: ThemeConfig | null = null;

function renderPreview() {
  const vars = previewCfg ? bgVars(previewCfg) : [];
  if (previewCfg) {
    // 仪表盘卡片不透明度：滑杆拖到哪就透到哪。100 时显式压回完全不透明，
    // 否则已经保存进主题的旧值会留在画面上，看着像滑杆没生效。
    const opacity = safeOpacity(previewCfg.card_opacity);
    if (opacity !== null) {
      const v = cardBgVar(isHexColor(previewCfg.colors?.bg_card) ? previewCfg.colors.bg_card : null, opacity);
      vars.push(v ?? "--panel-card-bg: var(--el-bg-color);");
      // 内容区底板读的是同一个不透明度（摊的是页面色），预览里也要一起给，
      // 否则拖滑杆时只有卡片在动、底板不动
      vars.push(`--panel-surface-opacity: ${opacity}%;`);
    }
    // 模糊滑杆拖到哪就糊到哪；0 时显式压回 none，否则已保存的旧值会留在画面上
    const blur = safeBlur(previewCfg.card_blur);
    if (blur !== null) {
      vars.push(cardBlurVar(blur) ?? "--panel-card-blur: none;");
    }
  }
  // 预览态下「没有背景图」也要显式压成 none：主题样式里可能还留着已保存的那张，
  // 不压的话点「清除」后画面上还是旧图，看着像没生效。
  if (previewCfg && !safeBgImage(previewCfg.bg_image)) {
    vars.push("--panel-bg-image: none;");
  }
  injectStyle(vars.length ? `:root {\n${vars.join("\n")}\n}` : "", "panel-bg-preview");
}

/// 主题变量的即时预览（不写库）：设置页拖动背景缩放/位置、不透明度与模糊滑杆、
/// 换图时先落到页面上。
///
/// 单独一条 <style>，且固定排在主题样式之后 —— 两边都是 :root 规则、特异性
/// 相同，后出现在文档里的那条胜出。离开设置页（clearThemePreview）或保存主题后
/// 主题样式重新接管。
export function previewTheme(cfg: ThemeConfig | null) {
  previewCfg = cfg;
  renderPreview();
  const image = cfg ? safeBgImage(cfg.bg_image) : null;
  // 长宽比晚一步才解出来时，预览里的缩放算式不完整 —— 解完补渲染一次
  if (image) {
    ensureAspect(image, () => {
      if (previewCfg === cfg) renderPreview();
    });
  }
}

/// 收起预览，主题样式重新接管
export function clearThemePreview() {
  previewCfg = null;
  injectStyle("", "panel-bg-preview");
}

/// 旧名（背景几何预览）保留为别名，行为与 previewTheme 完全相同
export const previewBg = previewTheme;
export const clearBgPreview = clearThemePreview;

// 主题 store：主题定制（颜色/圆角/背景），定制持久化到后端 settings 表
export const useThemeStore = defineStore("theme", () => {
  /// 当前生效的主题配置（null = 未定制，用默认）
  const config = ref<ThemeConfig | null>(null);

  /// 把配置注入页面（不写库）
  function applyConfig(cfg: ThemeConfig | null) {
    config.value = cfg;
    injectStyle(cfg ? buildThemeCss(cfg) : "");
    // 缩放算式要图片长宽比，而解码是异步的：先把 CSS 的 cover 兜底渲染出去，
    // 解出来再重注入一次（缩放 = 100% 时两者本就等价，看不出来）。
    const image = cfg ? safeBgImage(cfg.bg_image) : null;
    if (image) {
      ensureAspect(image, () => {
        if (config.value && safeBgImage(config.value.bg_image) === image) {
          injectStyle(buildThemeCss(config.value));
        }
      });
    }
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
