import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode, useState } from "react";
import { act, cleanup, render } from "@testing-library/react";
import { readFileSync } from "node:fs";
import "../i18n";
import { advanceFinalReveal, __resetReducedMotionCache, prefersReducedMotion, sliceRevealed, TimelineSegsView } from "../features/chat/segments";
import { useRun } from "../stores/run";
import { useUi } from "../stores/ui";
import { upgradeDiagrams } from "../utils/diagrams";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));
vi.mock("katex", () => ({ renderToString: vi.fn(() => "<span>MATH</span>") }));
vi.mock("mermaid", () => ({ default: { initialize: vi.fn(), parse: vi.fn(async () => true), render: vi.fn(async () => ({ svg: "<svg>DIAGRAM</svg>" })) } }));

let now = 10_000;
let frameId = 0;
const frames = new Map<number, FrameRequestCallback>();
function paint(ms: number) {
  act(() => {
    now += ms;
    const batch = [...frames.values()];
    frames.clear();
    batch.forEach(callback => callback(now));
  });
}
beforeEach(() => {
  now = 10_000; frameId = 0; frames.clear();
  __resetReducedMotionCache();
  useUi.setState({ outputDisplay: "detailed" });
  vi.spyOn(performance, "now").mockImplementation(() => now);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.set(++frameId, callback); return frameId; });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
});
afterEach(() => {
  cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); __resetReducedMotionCache();
});

describe("fast final-answer reveal", () => {
  it("starts small, uses 500 chars/s, and finishes long answers within 1.2s", () => {
    expect(advanceFinalReveal(0, 1000, 0)).toBe(12);
    expect(advanceFinalReveal(12, 200, 100)).toBe(62);
    expect(advanceFinalReveal(12, 10_000, 600)).toBe(5006);
    expect(advanceFinalReveal(5006, 10_000, 1200)).toBe(10_000);
    expect(sliceRevealed("123😀结尾", 4)).toBe("123");
    expect(sliceRevealed("123😀结尾", 5)).toBe("123😀");
  });

  it("shares one cursor across multiple text segments without per-frame store writes or replay", () => {
    const done = vi.fn();
    const progress = vi.fn();
    const timeline = [{ kind: "text" as const, text: "A".repeat(40) }, { kind: "text" as const, text: "B".repeat(80) }];
    const { container, rerender } = render(<TimelineSegsView timeline={timeline} toolsMap={{}} finalRevealKey="run-a" onFinalRevealComplete={done} onFinalRevealProgress={progress} />);
    const storeChanged = vi.fn();
    const visible = () => [...container.querySelectorAll(".md p")].map(node => node.textContent).join("");
    const off = useRun.subscribe(storeChanged);
    try {
      expect(visible()).toBe("A".repeat(12));
      expect(container.querySelector('[data-output-key="text:1"]')).toBeNull();
      paint(50);
      expect(visible()).toBe("A".repeat(37));
      paint(50);
      expect(visible()).toBe("A".repeat(40) + "B".repeat(22));
      expect(done).not.toHaveBeenCalled();
      paint(1200);
      expect(visible()).toBe("A".repeat(40) + "B".repeat(80));
      expect(container.querySelector("[data-final-revealing]")).toBeNull();
      expect(done).toHaveBeenCalledTimes(1);
      expect(progress.mock.calls.some(([finished]) => finished === true)).toBe(true);
      rerender(<TimelineSegsView timeline={timeline} toolsMap={{}} finalRevealKey="run-a" onFinalRevealComplete={done} />);
      expect(visible()).toBe("A".repeat(40) + "B".repeat(80));
      expect(done).toHaveBeenCalledTimes(1);
      expect(storeChanged).not.toHaveBeenCalled();
    } finally { off(); }
  });

  it("defers math/mermaid upgrades and code-copy controls until the full Markdown converges", async () => {
    const text = "$x^2$\n\n```ts\nconst answer = 42;\n```\n\n" + "正文".repeat(100) + "\n\n```mermaid\ngraph TD; A-->B\n```";
    const { container } = render(<TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} finalRevealKey="formatted" />);
    await upgradeDiagrams(container);
    expect(container.querySelector(".ws-math")?.getAttribute("data-done")).toBeNull();
    paint(100);
    expect(container.querySelector("[data-final-revealing] .code-copy")).not.toBeNull();
    expect(readFileSync("src/theme/app.css", "utf8")).toContain(".md[data-final-revealing] .code-copy { display: none; }");
    paint(1200);
    expect(container.querySelector("[data-final-revealing]")).toBeNull();
    expect(container.querySelector(".code-wrap")?.textContent).toContain("const answer = 42;");
    await upgradeDiagrams(container);
    expect(container.querySelector(".ws-math")?.getAttribute("data-done")).toBe("1");
    expect(container.querySelector("svg")?.textContent).toBe("DIAGRAM");
  });

  it("shows reduced-motion finals immediately and completes only once in StrictMode", () => {
    vi.spyOn(window, "matchMedia").mockImplementation(query => ({ matches: query.includes("prefers-reduced-motion"), media: query,
      onchange: null, addListener: vi.fn(), removeListener: vi.fn(), addEventListener: vi.fn(), removeEventListener: vi.fn(), dispatchEvent: vi.fn() }));
    const done = vi.fn();
    const text = "无需播放".repeat(100);
    const { container } = render(<StrictMode><TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} finalRevealKey="reduce" onFinalRevealComplete={done} /></StrictMode>);
    expect(container.textContent).toContain(text);
    expect(container.querySelector("[data-final-revealing]")).toBeNull();
    expect(done).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(0);
  });

  it("history and ordinary completed output never schedule a final animation", () => {
    const text = "历史全文".repeat(100);
    const { container, rerender } = render(<TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} />);
    expect(container.textContent).toContain(text);
    rerender(<TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} />);
    expect(container.textContent).toContain(text);
    expect(frames.size).toBe(0);
  });

  it("honors current reduced motion even if a previous streaming reply cached false", () => {
    expect(prefersReducedMotion()).toBe(false);
    vi.spyOn(window, "matchMedia").mockImplementation(query => ({ matches: query.includes("prefers-reduced-motion"), media: query,
      onchange: null, addListener: vi.fn(), removeListener: vi.fn(), addEventListener: vi.fn(), removeEventListener: vi.fn(), dispatchEvent: vi.fn() }));
    const text = "本轮应直接显示".repeat(100);
    const done = vi.fn();
    const { container } = render(<TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} finalRevealKey="new-preference" onFinalRevealComplete={done} />);
    expect(container.textContent).toContain(text);
    expect(container.querySelector("[data-final-revealing]")).toBeNull();
    expect(done).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(0);
  });

  it("finishes immediately when reduced motion changes during an animation", () => {
    let reduced = false;
    let changed: (() => void) | undefined;
    vi.spyOn(window, "matchMedia").mockImplementation(query => ({ get matches() { return reduced; }, media: query,
      onchange: null, addListener: vi.fn(), removeListener: vi.fn(), addEventListener: (_event: string, listener: unknown) => { changed = listener as () => void; }, removeEventListener: vi.fn(), dispatchEvent: vi.fn() }));
    const done = vi.fn();
    const text = "正在展示的总结".repeat(100);
    function Wrapper() {
      const [key, setKey] = useState<string | undefined>("motion-change");
      return <TimelineSegsView timeline={[{ kind: "text", text }]} toolsMap={{}} finalRevealKey={key} onFinalRevealComplete={() => { done(); setKey(undefined); }} />;
    }
    const { container } = render(<Wrapper />);
    expect(container.textContent).not.toContain(text);
    act(() => { reduced = true; changed?.(); });
    expect(container.textContent).toContain(text);
    expect(done).toHaveBeenCalledTimes(1);
    expect(frames.size).toBe(0);
  });
});
