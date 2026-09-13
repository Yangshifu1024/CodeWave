// Scroll storm regression: mermaid placeholders must skip rendering while streaming, then render once finalized.
// Root cause per the fix record: per-frame DOM rebuilds + per-frame cache misses on incomplete code trigger repeated async renders → height jumps cause scroll oscillation.
import { describe, it, expect, vi, beforeEach } from "vitest";

const renderMock = vi.fn(async (_id: string, _code: string) => ({
  svg: "<svg>OK</svg>",
  diagramType: "flowchart",
}));
// Code containing BAD counts as a parse failure (mermaid suppressErrors semantics: failure returns false)
const parseMock = vi.fn(async (code: string) =>
  code.includes("BAD") ? false : { diagramType: "flowchart" },
);

vi.mock("mermaid", () => ({
  default: {
    initialize: vi.fn(),
    parse: (code: string) => parseMock(code),
    render: (id: string, code: string) => renderMock(id, code),
  },
}));

import { upgradeDiagrams } from "../utils/diagrams";

function diagramRoot(code: string, streaming: boolean) {
  const root = document.createElement("div");
  root.innerHTML =
    `<div class="md"${streaming ? ' data-streaming="true"' : ""}>` +
    `<div class="ws-diagram" data-kind="mermaid">${code}</div>` +
    `</div>`;
  return root;
}

beforeEach(() => {
  renderMock.mockClear();
  parseMock.mockClear();
});

describe("mermaid 流式防抖动", () => {
  it("流式中（.md[data-streaming]）占位符保持原文，不触发 render", async () => {
    const root = diagramRoot("graph TD;A--&gt;B;", true);
    await upgradeDiagrams(root);
    const el = root.querySelector<HTMLElement>(".ws-diagram")!;
    expect(el.dataset.done).toBeUndefined(); // not marked done; can render after finalization
    expect(el.innerHTML).toContain("A--&gt;B;");
    expect(renderMock).not.toHaveBeenCalled();
  });

  it("定稿后（无 data-streaming）补渲染：render 一次且 innerHTML 为 svg", async () => {
    const root = diagramRoot("graph TD;A--&gt;B;", false);
    await upgradeDiagrams(root);
    const el = root.querySelector<HTMLElement>(".ws-diagram")!;
    expect(el.dataset.done).toBe("1");
    expect(el.innerHTML).toBe("<svg>OK</svg>");
    expect(renderMock).toHaveBeenCalledTimes(1);
  });

  it("parse 失败：错误态只写一行摘要，不回写全文，不进缓存（可重试）", async () => {
    const root = diagramRoot("BAD graph", false);
    await upgradeDiagrams(root);
    const el = root.querySelector<HTMLElement>(".ws-diagram")!;
    expect(el.classList.contains("ws-diagram-error")).toBe(true);
    expect(el.textContent).toMatch(/^\[mermaid\]/);
    expect(el.textContent).not.toContain("BAD graph"); // error text must not grow the height
    expect(renderMock).not.toHaveBeenCalled();
    // Not cached: a fresh placeholder re-parses on the next upgrade (retryable)
    const root2 = diagramRoot("BAD graph", false);
    await upgradeDiagrams(root2);
    expect(parseMock).toHaveBeenCalledTimes(2);
  });

  it("并发同代码去重：共享 in-flight Promise，render 只调一次", async () => {
    let resolveRender!: (v: { svg: string; diagramType: string }) => void;
    renderMock.mockImplementationOnce(
      () => new Promise<{ svg: string; diagramType: string }>((r) => (resolveRender = r)),
    );
    const a = diagramRoot("graph LR;X--&gt;Y;", false);
    const b = diagramRoot("graph LR;X--&gt;Y;", false);
    const p = Promise.all([upgradeDiagrams(a), upgradeDiagrams(b)]);
    // Flush the microtask chain (import → parse → renderDedup); both upgrades are already waiting, render is called once
    await new Promise((r) => setTimeout(r, 0));
    expect(renderMock).toHaveBeenCalledTimes(1);
    resolveRender({ svg: "<svg>OK</svg>", diagramType: "flowchart" });
    await p;
    expect(a.querySelector<HTMLElement>(".ws-diagram")!.innerHTML).toBe("<svg>OK</svg>");
    expect(b.querySelector<HTMLElement>(".ws-diagram")!.innerHTML).toBe("<svg>OK</svg>");
  });
});
