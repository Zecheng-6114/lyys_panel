import { defineStore } from "pinia";
import { ref } from "vue";
import http from "../api/http";
import { basePath } from "../base";

/// 单个字重文件。weight 为 CSS 字重，file 是后端返回的文件名
/// （内容 sha256 前 16 位 + 扩展名，由服务端生成）。
export interface FontFace {
  weight: number;
  file: string;
}

/// 界面字体配置。null = 未定制，使用系统字体。
export interface FontConfig {
  version?: number;
  family?: string;
  faces?: FontFace[];
}

/// 系统字体栈：自定义字体之后的兜底。面板不内置任何第三方字体，
/// 未配置自定义字体时全站就走这一栈。
export const SYSTEM_FONT_STACK =
  '-apple-system, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", sans-serif';

/// 字体文件名白名单，与后端 valid_font_file 同规则（双端校验）
const FILE_RE = /^[0-9a-f]{16}\.(ttf|otf|woff|woff2)$/;

/// 字体名白名单：与后端 valid_font_family 同规则（P1-2 双端校验）。
/// 这个名字会被拼进 @font-face 与 --el-font-family，引号/分号/花括号/
/// 反斜杠/圆括号/逗号/控制字符能把字符串闭合出去，出现即整份配置作废。
function safeFamily(s: unknown): string | null {
  if (typeof s !== "string") return null;
  if (!s || s.length > 64) return null;
  if (/["';{}\\(),]/.test(s) || /[\x00-\x1f]/.test(s)) return null;
  return s;
}

/// 按扩展名给 CSS format() 提示；扩展名由后端按文件头判定，这里只做映射
function formatHint(file: string): string {
  if (file.endsWith(".woff2")) return "woff2";
  if (file.endsWith(".woff")) return "woff";
  if (file.endsWith(".otf")) return "opentype";
  return "truetype";
}

/// 把字体配置翻译成注入用的 CSS；无有效配置返回空串（不注入）。
/// 所有字段注入前过白名单，非法值直接丢弃，防止库里的恶意值进入样式上下文。
export function buildFontCss(cfg: FontConfig | null): string {
  const family = safeFamily(cfg?.family);
  if (!family) return "";
  const faces = (cfg?.faces ?? []).filter(
    (f) =>
      Number.isInteger(f.weight) &&
      f.weight >= 100 &&
      f.weight <= 900 &&
      typeof f.file === "string" &&
      FILE_RE.test(f.file),
  );
  if (!faces.length) return "";
  const blocks = faces.map(
    (f) => `@font-face {
  font-family: "${family}";
  font-weight: ${f.weight};
  font-style: normal;
  font-display: swap;
  src: url("${basePath()}fonts/custom/${f.file}") format("${formatHint(f.file)}");
}`,
  );
  return `${blocks.join("\n")}
:root {
  --el-font-family: "${family}", ${SYSTEM_FONT_STACK};
  --panel-mono: "${family}", ${SYSTEM_FONT_STACK};
}`;
}

/// 注入/替换单个 <style>。与主题同理：必须晚于打包 CSS，变量才覆盖得住。
function injectStyle(css: string) {
  let el = document.getElementById("panel-font") as HTMLStyleElement | null;
  if (!css) {
    if (el) el.textContent = "";
    return;
  }
  if (!el) {
    el = document.createElement("style");
    el.id = "panel-font";
    document.head.appendChild(el);
  }
  el.textContent = css;
}

// 字体 store：界面字体定制，配置持久化到后端 settings 表（对所有设备生效）
export const useFontStore = defineStore("font", () => {
  /// 当前生效的字体配置（null = 用系统字体）
  const config = ref<FontConfig | null>(null);

  /// 把配置注入页面（不写库）
  function applyConfig(cfg: FontConfig | null) {
    config.value = cfg;
    injectStyle(buildFontCss(cfg));
  }

  /// 登录后从服务端拉取配置
  async function load() {
    try {
      const { data } = await http.get("/font");
      applyConfig(data?.config ?? null);
    } catch {
      // 拉取失败保持系统字体，不打断页面
    }
  }

  /// 保存配置到服务端并即时生效；传 null 恢复系统字体
  async function save(cfg: FontConfig | null) {
    await http.post("/font", cfg ?? null);
    applyConfig(cfg);
  }

  /// 上传单个字重文件，返回服务端保存的文件名
  async function upload(file: File): Promise<string> {
    const fd = new FormData();
    fd.append("file", file);
    const { data } = await http.post("/font/upload", fd);
    return data.file as string;
  }

  return { config, applyConfig, load, save, upload };
});
