// markdown-it + highlight.js 渲染器；mermaid/katex 产出占位符，由 diagrams.ts 异步升级（懒加载，不进主 chunk）
import MarkdownIt from "markdown-it";
import hljs from "highlight.js";
import "highlight.js/styles/github.css";

// markdown-it 15 起自带类型且默认导出为 callable 常量，实例类型须经 typeof 取
type MarkdownItInstance = ReturnType<typeof MarkdownIt>;

const MATH_LANGS = new Set(["math", "katex", "latex", "tex"]);

/** 代码块包装：代码原文 URI 编码进 data-code（编码结果不含引号/尖括号，可安全作属性值）；
 *  右上复制按钮经 CSS 悬停显隐，点击走事件委托写剪贴板（codecopy.ts 全局委托）。 */
function wrapCodeBlock(innerHtml: string, rawCode: string): string {
  const encoded = encodeURIComponent(rawCode);
  return `<div class="code-wrap"><button type="button" class="code-copy" data-code="${encoded}" aria-label="copy code"><span class="code-copy-hint">Copy</span></button>${innerHtml}</div>`;
}


function createMd() {
  const md = new MarkdownIt({
    html: false,
    linkify: true,
    breaks: true,
    highlight(code: string, lang: string): string {
      if (lang === "mermaid") {
        return `<div class="ws-diagram" data-kind="mermaid">${md.utils.escapeHtml(code)}</div>`;
      }
      if (MATH_LANGS.has(lang)) {
        return `<div class="ws-math" data-kind="block">${md.utils.escapeHtml(code)}</div>`;
      }
      if (lang && hljs.getLanguage(lang)) {
        try {
          return wrapCodeBlock(
            `<pre class="hljs"><code>${hljs.highlight(code, { language: lang, ignoreIllegals: true }).value}</code></pre>`,
            code,
          );
        } catch {
          /* 高亮失败则降级为转义输出 */
        }
      }
      return wrapCodeBlock(`<pre class="hljs"><code>${md.utils.escapeHtml(code)}</code></pre>`, code);
    },
  });
  // 外部链接新窗口打开
  const defLink = md.renderer.rules.link_open;
  md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
    tokens[idx].attrSet("target", "_blank");
    tokens[idx].attrSet("rel", "noopener");
    return defLink ? defLink(tokens, idx, options, env, self) : self.renderToken(tokens, idx, options);
  };
  // 表格包一层 .table-wrap（[docs/plan-modal-table-scroll](../../../docs/plan-modal-table-scroll.md)）：
  // app.css 里的 `.table-wrap { overflow-x: auto }` 此前是死 CSS——本文件没有任何规则产出这个类，
  // 于是宽表格在窄容器里被 `overflow-x: hidden` 静默裁掉且无法横向滚动（markdown-it 默认只 push
  // table_open/table_close，renderToken 输出裸 <table>）。这里补上唯一的生产点。
  // 窄表格仍然撑满容器宽度（`.table-wrap > table { width: 100% }`），与既有观感一致。
  // 注意：只接管首尾 token 的输出，thead/tbody/tr/th/td 仍走默认 renderToken，配对不受影响。
  md.renderer.rules.table_open = () => '<div class="table-wrap"><table>';
  md.renderer.rules.table_close = () => "</table></div>";
  installMath(md);
  return md;
}

// ---------- 行内 $...$ 与块级 $$...$$（算法移植自 markdown-it-katex，BSD-2） ----------

/** 判定 pos 处的 `$` 能否作为数学定界符开/闭（KaTeX auto-render 语义）。
 * 开符后跟 `$` 或空白不能开；闭符前面是 `$`、*_~ 或空白不能闭。
 * 金额场景由「开符后不能是空白」规则保护（"5$ and 10$" 的第一个 $ 后跟空格），
 * 而「闭符前是数字」并不禁止——否则 $E=mc^2$ 这类普通 TeX 会全部失效。 */
function isValidDelim(state: any, pos: number): { can_open: boolean; can_close: boolean } {
  const max = state.posMax;
  const prev = pos > 0 ? state.src.charCodeAt(pos - 1) : -1;
  const next = pos + 1 <= max ? state.src.charCodeAt(pos + 1) : -1;
  let can_close = true;
  let can_open = true;
  if (prev === 0x2a /* * */ || prev === 0x5f /* _ */ || prev === 0x7e /* ~ */ || prev === 0x24 /* $ */) {
    can_close = false;
  }
  if (next === 0x24 /* $ */) {
    can_open = false;
  }
  if (state.md.utils.isWhiteSpace(prev)) {
    can_close = false;
  }
  if (state.md.utils.isWhiteSpace(next)) {
    can_open = false;
  }
  return { can_open, can_close };
}

function mathInline(state: any, silent: boolean): boolean {
  const start = state.pos;
  if (state.src[start] !== "$") return false;
  if (!isValidDelim(state, start).can_open) {
    if (!silent) state.pending += "$";
    state.pos += 1;
    return true;
  }
  let pos = start + 1;
  let match: number | null = null;
  while ((match === null) && pos < state.posMax) {
    const ch = state.src[pos];
    if (ch === "\\") {
      pos += 2;
      continue;
    }
    if (ch !== "$") {
      pos += 1;
      continue;
    }
    if (isValidDelim(state, pos).can_close) {
      match = pos;
      break;
    }
    pos += 1;
  }
  if (match === null) {
    if (!silent) state.pending += "$";
    state.pos = start + 1;
    return true;
  }
  const content = state.src.slice(start + 1, match);
  if (!silent) {
    const token = state.push("math_inline", "math", 0);
    token.markup = "$";
    token.content = content;
  }
  state.pos = match + 1;
  return true;
}

function mathBlock(state: any, start: number, end: number, silent: boolean): boolean {
  let pos = state.bMarks[start] + state.tShift[start];
  let max = state.eMarks[start];
  if (pos + 2 > max) return false;
  if (state.src.slice(pos, pos + 2) !== "$$") return false;
  pos += 2;
  let firstLine = state.src.slice(pos, max);
  if (silent) return true;
  let last = start;
  let found = false;
  if (firstLine.trim().slice(-2) === "$$") {
    firstLine = firstLine.trim().slice(0, -2);
    found = true;
  }
  const lines: string[] = [];
  while (!found) {
    last += 1;
    if (last >= end) break;
    pos = state.bMarks[last] + state.tShift[last];
    max = state.eMarks[last];
    if (pos < max && state.tShift[last] < state.blkIndent) break;
    const line = state.src.slice(pos, max).trim();
    if (line.slice(-2) === "$$") {
      lines.push(line.slice(0, -2));
      found = true;
    } else {
      lines.push(state.src.slice(pos, max));
    }
  }
  state.line = last + 1;
  const token = state.push("math_block", "math", 0);
  token.block = true;
  token.content = (firstLine ? firstLine + "\n" : "") + lines.join("\n") + (lines.length ? "\n" : "");
  token.markup = "$$";
  token.map = [start, state.line];
  return true;
}

/** 就地装上数学插件（不叫 use* —— 它不是 Hook，旧名 useMath 会触发 rules-of-hooks 误报） */
function installMath(md: MarkdownItInstance) {
  md.inline.ruler.after("escape", "math_inline", mathInline);
  md.block.ruler.before("fence", "math_block", mathBlock, { alt: ["paragraph", "blockquote"] });
  const esc = (s: string) => md.utils.escapeHtml(s);
  md.renderer.rules.math_inline = (tokens, idx) =>
    `<span class="ws-math" data-kind="inline">${esc(tokens[idx].content)}</span>`;
  md.renderer.rules.math_block = (tokens, idx) =>
    `<div class="ws-math" data-kind="block">${esc(tokens[idx].content)}</div>`;
}

/** 全局共享的 markdown-it 实例（html 关闭：不信任 LLM 输出中的原始 HTML） */
export const md = createMd();

/**
 * 将 markdown 源串渲染为 HTML。
 *
 * `diagramPending`：流式期间 mermaid 占位提示的文案（i18n 由调用方传入）。它写进占位符的
 * `data-pending`，由 app.css 的 ::after 取用（CSS 里写死中文会漏译，见版式审计）。
 * 不传 = 不写该属性（非流式调用与子代理汇报不需要它）。
 */
export function renderMarkdown(src: string, diagramPending?: string): string {
  const html = md.render(src ?? "");
  if (!diagramPending) return html;
  const attr = md.utils.escapeHtml(diagramPending);
  return html.replace(/<div class="ws-diagram"/g, `<div class="ws-diagram" data-pending="${attr}"`);
}


/**
 * 用户气泡的 markdown 渲染（[docs/markdown-style-refresh]）：保守子集。
 *
 * 关键约束：
 * - 不渲染 math（既无 ws-math 占位，也无 katex 升级——用户消息不进入图表/公式管道）
 * - 不升级 mermaid（fence 走普通 escape，输出代码块，不返 ws-diagram）
 * - 不注入 target="_blank"，链接走 linkhandler.ts 的 document 委托 → IPC open_url 走系统浏览器
 * - 失败兜底：md.render 抛错时回退到空串（调用方再走原纯文本路径）
 *
 * 实现路径：复用同一个 markdown-it 实例（md），渲染后做一次后处理：
 * 1) <span class="ws-math"…>/<div class="ws-math"…> → 还原为可见源串（$<code>…</code>$）
 * 2) <div class="ws-diagram"…> → 改写为 <pre class="hljs"><code>（mermaid 当代码块）
 * 3) 移除 target="_blank" 与 rel="noopener"（链接行为交还 linkhandler）
 */
export function renderUserMarkdown(src: string): string {
  try {
    return postProcessForUser(md.render(src ?? ""));
  } catch {
    return "";
  }
}

function postProcessForUser(html: string): string {
  // 后处理 1：math 占位 → 可见源串（inline 与 block）
  html = html.replace(/<span class="ws-math"[^>]*>([\s\S]*?)<\/span>/g, (_m, c) => "$<code>" + c + "</code>$");
  html = html.replace(/<div class="ws-math"[^>]*>([\s\S]*?)<\/div>/g, (_m, c) => "\n\n$$$\n" + c + "\n$$$\n");
  // 后处理 2：mermaid 占位 → 当代码块（保留原转义后的内容）
  html = html.replace(/<div class="ws-diagram"[^>]*>([\s\S]*?)<\/div>/g, (_m, c) => '<pre class="hljs"><code>' + c + "</code></pre>");
  // 后处理 3：链接去 target/rel —— 走 linkhandler 委托
  html = html.replace(/ target="_blank"/g, "");
  html = html.replace(/ rel="noopener"/g, "");
  return html;
}

