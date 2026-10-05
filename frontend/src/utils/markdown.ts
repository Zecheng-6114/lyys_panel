/**
 * 极简 Markdown 渲染（聊天回复专用）。
 *
 * 为什么自己写而不用 marked + DOMPurify：
 * - 聊天回复只需要很小一个子集（标题/列表/代码/引用/链接/粗体）；
 * - 模型输出是不可信文本，任何 HTML 渲染都必须先转义。这里**先整体转义、再拼自己
 *   生成的标签**，结构上就不存在注入面，不需要再引一层消毒库；
 * - 少两个运行时依赖，符合本仓库对体积的取舍。
 *
 * 支持的语法：``` 围栏代码 ```、# 标题、- / 1. 列表、> 引用、--- 分隔线，
 * 行内 `code`、**粗体**、*斜体*、[文字](链接)、裸链接自动成链。
 * 链接只放行 http/https/mailto，其余原样当文字显示。
 */

const ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

/** HTML 转义：所有进入输出的文本都要先过这里 */
export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/** 只放行安全协议的链接，其余按纯文本返回 */
function link(label: string, href: string): string {
  const h = href.trim();
  if (!/^(https?:\/\/|mailto:)/i.test(h)) return label;
  return `<a href="${h}" target="_blank" rel="noopener noreferrer">${label}</a>`;
}

/** 行内元素。入参必须是**已转义**的文本。 */
function inline(src: string): string {
  let t = src;
  // 行内代码先处理，避免其中的 ** / [ ] 被后续规则吃掉
  t = t.replace(/`([^`]+)`/g, "<code>$1</code>");
  t = t.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  t = t.replace(/(^|[^*\w])\*([^*\n]+)\*/g, "$1<em>$2</em>");
  t = t.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, label: string, href: string) =>
    link(label, href),
  );
  // 裸链接自动成链：前面要求是行首/空白/左括号，避免破坏已成链的标签
  t = t.replace(/(^|[\s(])(https?:\/\/[^\s<>()]+)/g, (_m, pre: string, url: string) =>
    `${pre}${link(url, url)}`,
  );
  return t;
}

/**
 * 渲染为 HTML 字符串（已转义，可安全 v-html）。
 * 段落内的换行保留为 <br>：聊天里手动换行通常是有意的。
 */
export function renderMarkdown(src: string): string {
  if (!src) return "";
  const text = src.replace(/\r\n?/g, "\n");

  // 1) 先摘出围栏代码块，用占位符换掉——不然代码里的 markdown 记号会被当语法处理
  const blocks: string[] = [];
  const withTokens = text.replace(
    /```([^\n`]*)\n([\s\S]*?)(?:```|$)/g,
    (_m, lang: string, code: string) => {
      const i = blocks.length;
      const l = escapeHtml(String(lang).trim());
      const body = escapeHtml(String(code).replace(/\n$/, ""));
      blocks.push(
        `<pre class="md-pre"${l ? ` data-lang="${l}"` : ""}><code>${body}</code></pre>`,
      );
      return `\n\u0000CB${i}\u0000\n`;
    },
  );

  const escaped = escapeHtml(withTokens);
  const lines = escaped.split("\n");
  const out: string[] = [];
  let para: string[] = [];
  let list: "ul" | "ol" | null = null;

  const flushPara = () => {
    if (para.length) {
      out.push(`<p>${para.map(inline).join("<br />")}</p>`);
      para = [];
    }
  };
  const closeList = () => {
    if (list) {
      out.push(`</${list}>`);
      list = null;
    }
  };

  for (const raw of lines) {
    const line = raw.trimEnd();
    const trimmed = line.trim();

    if (!trimmed) {
      flushPara();
      closeList();
      continue;
    }
    const codeToken = /^\u0000CB(\d+)\u0000$/.exec(trimmed);
    if (codeToken) {
      flushPara();
      closeList();
      out.push(blocks[Number(codeToken[1])] || "");
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushPara();
      closeList();
      const level = heading[1].length;
      out.push(`<h${level}>${inline(heading[2])}</h${level}>`);
      continue;
    }
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flushPara();
      closeList();
      out.push('<hr class="md-hr" />');
      continue;
    }
    const bullet = /^[-*+]\s+(.*)$/.exec(trimmed);
    if (bullet) {
      flushPara();
      if (list !== "ul") {
        closeList();
        out.push("<ul>");
        list = "ul";
      }
      out.push(`<li>${inline(bullet[1])}</li>`);
      continue;
    }
    const numbered = /^\d+[.)]\s+(.*)$/.exec(trimmed);
    if (numbered) {
      flushPara();
      if (list !== "ol") {
        closeList();
        out.push("<ol>");
        list = "ol";
      }
      out.push(`<li>${inline(numbered[1])}</li>`);
      continue;
    }
    const quote = /^&gt;\s?(.*)$/.exec(trimmed);
    if (quote) {
      flushPara();
      closeList();
      out.push(`<blockquote>${inline(quote[1])}</blockquote>`);
      continue;
    }
    para.push(trimmed);
  }
  flushPara();
  closeList();
  return out.join("");
}

/** 把字节数说成人话（工具结果的体积标记用） */
export function humanBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
