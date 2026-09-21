// B4: markdown math/diagram placeholders + katex upgrade
import { describe, expect, it } from "vitest";
import { renderMarkdown } from "../utils/markdown";
import { upgradeDiagrams } from "../utils/diagrams";
import { renderCached } from "../features/chat/segments";

describe("markdown math/mermaid", () => {
  it("行内 $...$ 产出 ws-math 占位符（TeX 原文转义保留）", () => {
    const html = renderMarkdown("质能方程 $E=mc^2$ 很有名");
    expect(html).toContain('class="ws-math"');
    expect(html).toContain("E=mc^2");
    // No false positives outside math: numeric amounts produce no placeholder
    const plain = renderMarkdown("价格是 5 元和 10 元");
    expect(plain).not.toContain("ws-math");
  });

  it("mermaid 占位提示文案写进 data-pending（i18n 化；CSS 用 content: attr() 取用）", () => {
    const src = ["```mermaid", "graph TD", "```"].join("\n");
    expect(renderMarkdown(src, "PENDING")).toContain('data-pending="PENDING"');
    // 不传文案 = 不写该属性（非流式调用与子代理汇报不需要它）
    expect(renderMarkdown(src)).not.toContain("data-pending");
  });

  it("渲染缓存的键含提示文案：切语言不会取回旧语言的占位提示", () => {
    const src = ["```mermaid", "graph TD", "```"].join("\n");
    expect(renderCached(src, "A")).not.toBe(renderCached(src, "B"));
    expect(renderCached(src, "A")).toBe(renderCached(src, "A")); // 同文案仍命中缓存
  });

  it("块级 $$...$$ 产出 ws-math 块占位符", () => {
    const html = renderMarkdown("$$\n\\int_0^1 x\\,dx = \\frac{1}{2}\n$$");
    expect(html).toContain('data-kind="block"');
    expect(html).toContain("\\int_0^1");
  });

  it("fenced mermaid/math 代码块产出对应占位符", () => {
    const m = renderMarkdown("```mermaid\ngraph TD;A-->B;\n```");
    expect(m).toContain('class="ws-diagram"');
    expect(m).toContain("A--&gt;B");
    const k = renderMarkdown("```math\nx^2 + y^2 = z^2\n```");
    expect(k).toContain('class="ws-math"');
  });

  it("escape 拦截：\\$ 视为美元符号不产公式", () => {
    const html = renderMarkdown("花 \\$5 和 \\$10");
    expect(html).not.toContain("ws-math");
  });

  it("upgradeDiagrams 将 ws-math 占位符渲染为 katex HTML", async () => {
    const root = document.createElement("div");
    root.innerHTML = '<span class="ws-math" data-kind="inline">E=mc^2</span>';
    await upgradeDiagrams(root);
    const el = root.querySelector<HTMLElement>(".ws-math")!;
    expect(el.dataset.done).toBe("1");
    expect(el.innerHTML).toContain("katex");
  });

  it("upgradeDiagrams 对非法 TeX 回退原文（不抛错、不产 katex）", async () => {
    const root = document.createElement("div");
    root.innerHTML = '<span class="ws-math" data-kind="inline">\\frac{{{{</span>';
    await upgradeDiagrams(root);
    const el = root.querySelector<HTMLElement>(".ws-math")!;
    expect(el.classList.contains("ws-math-raw") || el.innerHTML.length > 0).toBe(true);
  });
});
