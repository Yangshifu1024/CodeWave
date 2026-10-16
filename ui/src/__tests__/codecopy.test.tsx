// Code block copy button pipeline (markdown.ts wrapCodeBlock + codecopy.ts delegation)
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderMarkdown, renderUserMarkdown } from "../utils/markdown";
import { bindCodeCopyDelegate } from "../utils/codecopy";

describe("markdown 代码块包装（复制按钮）", () => {
  it.each(["rust", "text", "unknown-language", ""])("%s 围栏只生成一个代码框，保留原文与复制载荷", (language) => {
    const code = 'let s = "你好 <script>&";\n';
    for (const render of [renderMarkdown, renderUserMarkdown]) {
      const container = document.createElement("div");
      container.innerHTML = render("```" + language + "\n" + code + "```");
      expect(container.querySelectorAll("pre")).toHaveLength(1);
      expect(container.querySelectorAll(".code-wrap")).toHaveLength(1);
      expect(container.querySelector(".code-wrap")?.parentElement).toBe(container);
      expect(container.querySelector(".code-wrap > pre > code")?.textContent).toBe(code);
      expect(container.querySelector("pre pre, pre button, code div, script")).toBeNull();
      expect(decodeURIComponent(container.querySelector(".code-copy")!.getAttribute("data-code")!)).toBe(code);
      if (language === "rust") expect(container.querySelector(".hljs-keyword")).not.toBeNull();
    }
  });

  it("列表里的代码框和相邻代码框保持独立，空围栏保留空复制载荷", () => {
    const container = document.createElement("div");
    container.innerHTML = renderMarkdown("- 示例\n\n  ```text\n  first\n  ```\n\n```\n```\n\n```text\nlast\n```");
    expect(container.querySelectorAll("pre")).toHaveLength(3);
    expect(container.querySelectorAll(".code-wrap > pre")).toHaveLength(3);
    expect(container.querySelector("li > .code-wrap > pre > code")?.textContent).toBe("first\n");
    expect([...container.querySelectorAll(".code-copy")].map((button) => decodeURIComponent(button.getAttribute("data-code")!)))
      .toEqual(["first\n", "", "last\n"]);
  });

  it.each(["mermaid", "math", "katex", "latex", "tex"])("%s 占位符在正文中，不进入代码框", (language) => {
    const container = document.createElement("div");
    container.innerHTML = renderMarkdown("```" + language + "\nsource <tag>\n```");
    expect(container.querySelector("pre, code, .code-wrap, .code-copy")).toBeNull();
    expect(container.firstElementChild?.matches(".ws-diagram, .ws-math")).toBe(true);
    expect(container.firstElementChild?.textContent).toBe("source <tag>\n");
  });

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
    // mockReset + 重置实现：避免上一个用例的 mockRejectedValueOnce 泄漏到下一个用例。
    writeText.mockReset();
    writeText.mockImplementation(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    document.body.innerHTML = "";
  });

  function clickOn(btn: HTMLElement) {
    btn.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  }

  /** copyText 先 await 插件探测再写，断言前需排空微任务队列（这里用假定时器，不能用 waitFor）。 */
  async function flush() {
    for (let i = 0; i < 8; i++) await Promise.resolve();
  }

  it("点击按钮写剪贴板并反馈已复制，1.5s 复原；非按钮点击不触发", async () => {
    const unbind = bindCodeCopyDelegate();
    document.body.innerHTML = `<div class="code-wrap"><button class="code-copy" data-code="${encodeURIComponent(
      "const a = 1;",
    )}"><span class="code-copy-hint">Copy</span></button><pre class="hljs"><code>const a = 1;</code></pre></div>`;
    const btn = document.querySelector(".code-copy") as HTMLElement;

    clickOn(btn);
    await flush();
    expect(writeText).toHaveBeenCalledWith("const a = 1;");
    // Feedback: hint text + done class
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
    await flush();
    vi.advanceTimersByTime(1000);
    clickOn(btn); // second click resets the timer
    await flush();
    vi.advanceTimersByTime(1000); // 2s since the first click, but only 1s since the reset
    expect(btn.className).toContain("code-copy-done");
    vi.advanceTimersByTime(600);
    expect(btn.className).not.toContain("code-copy-done");

    unbind();
    clickOn(btn);
    await flush();
    expect(writeText).toHaveBeenCalledTimes(2); // no more increments after unbind
  });

  it("写入失败时 hint 显「失败」且带 code-copy-fail（不静默，回归钉子）", async () => {
    writeText.mockRejectedValueOnce(new Error("NotAllowedError"));
    const unbind = bindCodeCopyDelegate();
    document.body.innerHTML = `<div class="code-wrap"><button class="code-copy" data-code="${encodeURIComponent(
      "写不进去",
    )}"><span class="code-copy-hint">Copy</span></button></div>`;
    const btn = document.querySelector(".code-copy") as HTMLElement;

    clickOn(btn);
    await flush();

    expect(btn.className).toContain("code-copy-fail");
    expect(btn.querySelector(".code-copy-hint")!.textContent).toBe("失败");
    unbind();
  });

  it("clipboard 不存在时降级到 execCommand 兜底（不抛 TypeError）", async () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    document.execCommand = vi.fn(() => true) as unknown as typeof document.execCommand;
    const unbind = bindCodeCopyDelegate();
    document.body.innerHTML = `<div class="code-wrap"><button class="code-copy" data-code="${encodeURIComponent(
      "兜底内容",
    )}"><span class="code-copy-hint">Copy</span></button></div>`;
    const btn = document.querySelector(".code-copy") as HTMLElement;

    expect(() => clickOn(btn)).not.toThrow();
    await flush();

    // 兜底成功 → 显已复制（而不是抛错或静默）
    expect(btn.className).toContain("code-copy-done");
    unbind();
  });
});
