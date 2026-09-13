// Code block copy button pipeline (markdown.ts wrapCodeBlock + codecopy.ts delegation)
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderMarkdown } from "../utils/markdown";
import { bindCodeCopyDelegate } from "../utils/codecopy";

describe("markdown 代码块包装（复制按钮）", () => {
  it("fence 代码块产出 .code-wrap + .code-copy 按钮，data-code 为 URI 编码原文", () => {
    const html = renderMarkdown("```rust\nlet s = \"你好\";\n```");
    expect(html).toContain('class="code-wrap"');
    expect(html).toContain('class="code-copy"');
    // data-code attribute safety: the encoded value contains no quotes/angle brackets
    expect(html).toMatch(/data-code="[^"]*"/);
    expect(html).toContain(encodeURIComponent('let s = "你好";'));
    // Highlight output still lives inside pre.hljs
    expect(html).toContain("<pre class=\"hljs\"><code>");
  });

  it("无语言代码块同样包装（escapeHtml 路径）", () => {
    const html = renderMarkdown("```\n<script>alert(1)</script>\n```");
    expect(html).toContain('class="code-wrap"');
    // Raw source is URI-encoded into the attribute while the body keeps escaped text; no injection surface in either channel
    expect(html).toContain(encodeURIComponent("<script>alert(1)</script>"));
    expect(html).toContain("&lt;script&gt;");
  });

  it("mermaid/katex 占位符不被包装", () => {
    expect(renderMarkdown("```mermaid\ngraph TD\n```")).toContain("ws-diagram");
    expect(renderMarkdown("```mermaid\ngraph TD\n```")).not.toContain("code-copy");
    expect(renderMarkdown("```math\nE=mc^2\n```")).toContain("ws-math");
    expect(renderMarkdown("```math\nE=mc^2\n```")).not.toContain("code-copy");
  });
});

describe("codecopy 点击委托", () => {
  const writeText = vi.fn(async () => {});

  beforeEach(() => {
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    writeText.mockClear();
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function clickOn(btn: HTMLElement) {
    btn.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  }

  it("点击按钮写剪贴板并反馈已复制，1.5s 复原；非按钮点击不触发", async () => {
    const unbind = bindCodeCopyDelegate();
    document.body.innerHTML = `<div class="code-wrap"><button class="code-copy" data-code="${encodeURIComponent(
      "const a = 1;",
    )}"><span class="code-copy-hint">Copy</span></button><pre class="hljs"><code>const a = 1;</code></pre></div>`;
    const btn = document.querySelector(".code-copy") as HTMLElement;

    clickOn(btn);
    expect(writeText).toHaveBeenCalledWith("const a = 1;");
    // Feedback: hint text + done class (visible after a microtask flush)
    await Promise.resolve();
    expect(btn.className).toContain("code-copy-done");
    expect(btn.querySelector(".code-copy-hint")!.textContent).toBe("已复制");

    // Restored after 1.5s
    vi.advanceTimersByTime(1600);
    expect(btn.className).not.toContain("code-copy-done");
    expect(btn.querySelector(".code-copy-hint")!.textContent).toBe("Copy");

    // Clicking a non-button area inside the wrap does not trigger
    clickOn(document.querySelector("pre") as HTMLElement);
    expect(writeText).toHaveBeenCalledTimes(1);
    unbind();
  });

  it("重复点击重置计时；unbind 后不再响应", async () => {
    const unbind = bindCodeCopyDelegate();
    document.body.innerHTML = `<div class="code-wrap"><button class="code-copy" data-code="x"><span class="code-copy-hint">Copy</span></button></div>`;
    const btn = document.querySelector(".code-copy") as HTMLElement;

    clickOn(btn);
    await Promise.resolve();
    vi.advanceTimersByTime(1000);
    clickOn(btn); // second click resets the timer
    await Promise.resolve();
    vi.advanceTimersByTime(1000); // 2s since the first click, but only 1s since the reset
    expect(btn.className).toContain("code-copy-done");
    vi.advanceTimersByTime(600);
    expect(btn.className).not.toContain("code-copy-done");

    unbind();
    clickOn(btn);
    expect(writeText).toHaveBeenCalledTimes(2); // no more increments after unbind
    document.body.innerHTML = "";
  });
});
