// P4.8 扩 css 契约组：hljs token var / 用户气泡 md 重置 / table-wrap / 标题 / 链接 / blockquote / hr。
// 与 markdown.user.test.ts 配套：css + 渲染器双侧守门。
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { renderMarkdown } from "../utils/markdown";

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

describe("markdown 主题感（docs/markdown-style-refresh）", () => {
  it("hljs token 全部用 --ws-hl-* 主题感变量，取代硬编码 #hex", () => {
    // 共享组内任一 token 出现必须 var(--ws-hl-*)
    const tokenVars = ["--ws-hl-keyword", "--ws-hl-string", "--ws-hl-number", "--ws-hl-name", "--ws-hl-built", "--ws-hl-comment"];
    for (const v of tokenVars) {
      expect(appCss).toContain(`var(${v})`);
    }
  });

  it("共享组包含 .sub-drawer-stream .md（子代理过程流也吃这套契约）与 .skill-body.md（技能详情正文）", () => {
    // 只断言「成员在列表里」，不钉全量顺序与项数：共享组是开放的消费点集合，
    // 往后新增容器（.skill-body.md 等）不应打破本用例。逗号列表的顺序对 CSS 无语义。
    expect(appCss).toMatch(/:is\([^*]+?\.sub-drawer-stream \.md[^*]+?\)/);
    expect(appCss).toMatch(/:is\([^*]+?\.skill-body\.md[^*]+?\)/);
  });

  it("表格 th 底色 + 行 hover + .table-wrap 横向滚动", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+\.table-wrap\s*\{\s*overflow-x:\s*auto/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+th\s*\{\s*background:\s*var\(--ws-bg-nav\)/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+tr:hover\s+td\s*\{\s*background:\s*var\(--ws-bg-nav\)/);
  });

  // [docs/plan-modal-table-scroll]：上条用例曾只守 CSS 文本、不断言 DOM，于是 `.table-wrap` 长期是个
  // 零生产点的死规则（markdown-it 默认输出裸 <table>），宽表格被 overflow-x:hidden 静默裁掉。
  // 本条把契约补成「CSS + DOM 双守」：样式写了就必须真有这个包裹元素。
  it("渲染层真的产出 .table-wrap 包裹（CSS 死规则的解药）", () => {
    const html = renderMarkdown("| 项 | 处置 |\n| --- | --- |\n| a | b |");
    expect(html).toContain('<div class="table-wrap"><table>');
    expect(html).toContain("</table></div>");
    // 包裹层不得嵌套（markdown-it 无嵌套表格语法，出现即说明配对错乱）
    expect(html).not.toContain('<div class="table-wrap"><div class="table-wrap">');
  });

  // 预览弹框容器此前是 .assistant，被 860px 行长上限 + overflow-x:hidden 裁掉宽表格
  it("共享组包含 .viewer-md.md（文件预览弹框正文），且不吃聊天区 860px 行长上限", () => {
    expect(appCss).toMatch(/:is\([^*]+?\.viewer-md\.md[^*]+?\)/);
    expect(appCss).toMatch(/\.viewer-md\.md\s*\{[^}]*max-width:\s*100%/);
  });

  // [docs/plan-modal-table-scroll]：mermaid / katex 那组规则此前全挂 `.assistant .md` 前缀，
  // 预览弹框换容器后整体失配（公式丢 pre-wrap 兜底、.katex-display 丢横向滚动、mermaid 丢底色）。
  // 本条钉住「它们也走共享组」，防下一个新增预览容器再次踩同一个坑。
  it("mermaid / katex 规则也走共享 :is() 组，不再只挂 .assistant 前缀", () => {
    for (const sel of [".ws-diagram", ".ws-diagram svg", ".ws-diagram-error", ".ws-math-raw", ".katex-display"]) {
      expect(appCss).toMatch(new RegExp(`:is\\([^*]+?\\)\\s+${sel.replace(/[.[\]/]/g, "\\$&")}\\s*\\{`));
    }
    // 流式占位规则（含 [data-streaming] 限定）也必须在共享组内
    expect(appCss).toMatch(/:is\([^*]+?\.md\[data-streaming\][^*]+?\)\s+\.ws-diagram\s*\{\s*opacity/);
    // 旧前缀不得残留（残留即意味着有人又把新容器排除在外）
    expect(appCss).not.toMatch(/^\.assistant \.md \.ws-/m);
    expect(appCss).not.toMatch(/^\.assistant \.md \.katex/m);
  });

  it("链接走 var(--ws-accent) + 下划线 + 偏移", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+a\s*\{[^}]*color:\s*var\(--ws-accent\)/);
    expect(appCss).toMatch(/text-underline-offset:\s*3px/);
  });

  it("标题梯度 h1-h6 + 行高 + 颜色 token", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+:is\(h1,\s*h2,\s*h3,\s*h4,\s*h5,\s*h6\)\s*\{[^}]*line-height:\s*1\.4/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+h1\s*\{[^}]*font-size:\s*18\.5px/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+h3\s*\{[^}]*font-size:\s*15px/);
  });

  it("段落 line-height 1.7（[docs/markdown-style-refresh] 阅读感升级）", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+p\s*\{[^}]*line-height:\s*1\.7/);
  });

  it("blockquote 左侧 accent 条 + dim 文字", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+blockquote\s*\{[^}]*border-left:\s*3px solid var\(--ws-accent\)/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+blockquote\s*\{[^}]*color:\s*var\(--ws-text-2\)/);
  });

  it("hr 1px 主题感分隔线", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+hr\s*\{[^}]*border-top:\s*1px solid var\(--ws-border\)/);
  });

  it("del / mark 修饰元素有规则", () => {
    expect(appCss).toMatch(/:is\([^*]+?\)\s+del\s*\{[^}]*line-through/);
    expect(appCss).toMatch(/:is\([^*]+?\)\s+mark\s*\{[^}]*color-mix/);
  });

  it(".user-bubble.md 重置 white-space 与 p margin，避免 markdown 双换行", () => {
    expect(appCss).toMatch(/\.user-bubble\.md\s*\{\s*white-space:\s*normal/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p\s*\{[^}]*line-height:\s*1\.6/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p:first-child\s*\{\s*margin-top:\s*0/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*>\s*p:last-child\s*\{\s*margin-bottom:\s*0/);
  });

  it(".user-bubble.md 含基础 markdown 元素（h1-h3 / ul / blockquote / a / strong / em / code）", () => {
    expect(appCss).toMatch(/\.user-bubble\.md\s*:is\(h1,\s*h2,\s*h3,\s*h4,\s*h5,\s*h6\)\s*\{[^}]*font-weight:\s*600/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*blockquote\s*\{[^}]*border-left:\s*3px solid var\(--ws-accent\)/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*:not\(pre\)\s*>\s*code\s*\{[^}]*background:\s*var\(--ws-bg-nav\)/);
    expect(appCss).toMatch(/\.user-bubble\.md\s*a\s*\{[^}]*color:\s*var\(--ws-accent\)/);
  });

  it("bridge.tsx 输出 --ws-hl-* 与 --ws-code-block-* token", () => {
    const bridgeTsx = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../theme/bridge.tsx"),
      "utf8",
    );
    for (const v of [
      "--ws-hl-keyword",
      "--ws-hl-string",
      "--ws-hl-number",
      "--ws-hl-name",
      "--ws-hl-built",
      "--ws-hl-comment",
      "--ws-hl-attr",
      "--ws-code-block-bg",
      "--ws-code-block-border",
      "--ws-code-block-text",
    ]) {
      expect(bridgeTsx).toContain(v);
    }
  });
});
