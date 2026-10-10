// 用户气泡保守 markdown 子集（[docs/markdown-style-refresh]）：白名单 + 关闭 math / mermaid / target。
import { describe, it, expect } from "vitest";
import { renderMarkdown, renderUserMarkdown } from "../utils/markdown";

describe("用户气泡 markdown 子集 (renderUserMarkdown)", () => {
  it("保留基础 markdown：粗体 / 斜体 / 链接 / 行内 code / 标题 / 列表 / 引用", () => {
    const src = "**粗体** 与 *斜体*、\x60inline code\x60\n\n外部 [link](https://example.com)\n\n# H1\n\n## H2\n\n- 列表项 A\n- 列表项 B\n\n> 引用块";
    const html = renderUserMarkdown(src);
    expect(html).toContain("<strong>粗体</strong>");
    expect(html).toContain("<em>斜体</em>");
    expect(html).toContain("<code>inline code</code>");
    expect(html).toContain('<a href="https://example.com">link</a>');
    expect(html).toContain("<h1>");
    expect(html).toContain("<h2>");
    expect(html).toContain("<ul>");
    expect(html).toContain("<li>列表项 A</li>");
    expect(html).toContain("<blockquote>");
  });

  it("不输出 ws-math 占位（用户消息不进 math 管道）", () => {
    const inline = "质能方程 $E=mc^2$ 很有名";
    const block = "$$\n\\int_0^1 x\\,dx = \\frac{1}{2}\n$$";
    expect(renderUserMarkdown(inline)).not.toContain("ws-math");
    expect(renderUserMarkdown(block)).not.toContain("ws-math");
    // 后处理：inline math 应还原为 $<code>…</code>$ 的可见源串
    expect(renderUserMarkdown(inline)).toContain("<code>E=mc^2</code>");
  });

  it("不输出 ws-diagram 占位（用户消息的 mermaid 当代码块渲染）", () => {
    const src = "```mermaid\ngraph TD\nA-->B\n```";
    const html = renderUserMarkdown(src);
    expect(html).not.toContain("ws-diagram");
    // mermaid 块被改写为 <pre class="hljs"><code>…
    expect(html).toContain('<pre class="hljs"><code>');
  });

  it("不注入 target=_blank / rel=noopener（链接走 linkhandler 委托）", () => {
    const html = renderUserMarkdown("外部 [link](https://example.com)");
    expect(html).not.toContain('target="_blank"');
    expect(html).not.toContain('rel="noopener"');
    // 链接本身仍在
    expect(html).toContain('<a href="https://example.com">link</a>');
  });

  it("空串 / undefined 兜底", () => {
    expect(renderUserMarkdown("")).toBe("");
    // @ts-expect-error 故意传入 undefined 走兜底分支
    expect(renderUserMarkdown(undefined)).toBe("");
  });

  it("渲染助手消息的 renderMarkdown 行为不回归：math / mermaid 占位仍存在", () => {
    // 这条用例守住 renderMarkdown 不被 renderUserMarkdown 误改
    const inline = "质能方程 $E=mc^2$ 很有名";
    const block = "```mermaid\ngraph TD\nA-->B\n```";
    expect(renderMarkdown(inline)).toContain('class="ws-math"');
    expect(renderMarkdown(block)).toContain('class="ws-diagram"');
    expect(renderMarkdown("外部 [link](https://example.com)")).toContain('target="_blank"');
  });
});
