import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "../i18n";
import { useUi } from "../stores/ui";
import { OUTPUT_DISPLAY_KEY, readStoredOutputDisplay } from "../utils/outputDisplay";
import OutputDisplaySelect from "../features/tools/OutputDisplaySelect";
import { AppearanceSettings } from "../features/panels/FontSettings";
import ToolCallCard from "../features/tools/ToolCallCard";
import { canGroupTool } from "../features/tools/toolPresentation";
import { TimelineSegsView } from "../features/chat/segments";
import type { TimelineSeg, ToolView } from "../stores/run.types";
import { captureOutputAnchor, restoreOutputAnchor } from "../utils/outputAnchor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));

const tool = (overrides: Partial<ToolView> = {}): ToolView => ({
  callKey: "read", tool: "read", status: "ok", progressTail: "",
  argsPreview: '{"files":[{"path":"src/a.ts"}]}',
  outcome: { ok: true, data: { files: [{ path: "src/a.ts", content: "text" }] } }, ...overrides,
});
afterEach(() => {
  cleanup();
  useUi.setState({ outputDisplay: "detailed" });
  localStorage.removeItem(OUTPUT_DISPLAY_KEY);
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("output display preference", () => {
  it("defaults to detailed for absent, malformed or unavailable storage", () => {
    expect(readStoredOutputDisplay()).toBe("detailed");
    localStorage.setItem(OUTPUT_DISPLAY_KEY, "unknown");
    expect(readStoredOutputDisplay()).toBe("detailed");
    vi.stubGlobal("localStorage", { getItem: () => { throw new Error("unavailable"); }, removeItem: () => {} });
    expect(readStoredOutputDisplay()).toBe("detailed");
  });

  it("keeps an immediate, persisted selector in appearance settings", () => {
    render(<AppearanceSettings />);
    expect(document.querySelectorAll(".output-display-select")).toHaveLength(1);
    fireEvent.click(screen.getByText("简洁"));
    expect(useUi.getState().outputDisplay).toBe("compact");
    expect(readStoredOutputDisplay()).toBe("compact");
    expect(document.querySelectorAll('.ant-segmented-item-selected').length).toBe(1);
    fireEvent.click(screen.getByText("详细"));
    expect(useUi.getState().outputDisplay).toBe("detailed");
    expect(readStoredOutputDisplay()).toBe("detailed");
  });
});

describe("compact tool rows", () => {
  it("keeps the original detailed card by default", () => {
    render(<ToolCallCard tool={tool()} />);
    expect(document.querySelector(".tool-compact")).toBeNull();
    expect(document.querySelector(".tool-name")?.textContent).toBe("read");
  });

  it("lists every edited file with real counts and opens only that file's diff", () => {
    render(<ToolCallCard display="compact" tool={tool({ tool: "edit",
      argsPreview: JSON.stringify({ files: [
        { path: "a/index.ts", changes: [{ oldText: "first old", newText: "first new" }] },
        { path: "b/index.ts", changes: [{ oldText: "second old", newText: "second new" }] },
      ] }),
      outcome: { ok: true, data: { edited: ["a/index.ts", "b/index.ts"], line_changes: [
        { path: "a/index.ts", added: 2, removed: 1 }, { path: "b/index.ts", added: 1, removed: 3 },
      ] } },
    })} />);
    expect(document.querySelector(".tool-name")).toBeNull();
    expect(screen.getByText("a/index.ts")).toHaveProperty("title", "a/index.ts");
    expect(screen.getByText("+2 −1")).toBeTruthy();
    fireEvent.click(screen.getByText("b/index.ts"));
    expect(screen.getByText("second new")).toBeTruthy();
    expect(screen.queryByText("first new")).toBeNull();
  });

  it("omits unknown or invalid counts in restored history", () => {
    render(<ToolCallCard display="compact" tool={tool({ tool: "create",
      argsPreview: '{"_args_truncated":true}',
      outcome: { ok: true, data: { path: "restored.txt", line_changes: [{ path: "restored.txt", added: -1, removed: 0 }] } },
    })} />);
    expect(screen.getByText("restored.txt")).toBeTruthy();
    expect(document.querySelector(".line-count")).toBeNull();
  });

  it.each(["compact", "detailed"] as const)("folds error details in %s mode while retaining the failure status", (display) => {
    render(<ToolCallCard display={display} tool={tool({ tool: "command", status: "error",
      argsPreview: '{"command":"which aliyun"}',
      outcome: { ok: false, data: null, error: { code: "E_PLAN_READONLY", message: "计划模式只读拦截" }, warnings: ["check path"] },
    })} />);
    const header = document.querySelector(display === "compact" ? ".tool-compact-head" : ".tool-head")!;
    expect(header.textContent).toContain("失败");
    expect(document.querySelector(".tool-card.st-error")).toBeTruthy();
    expect(screen.queryByText("E_PLAN_READONLY: 计划模式只读拦截")).toBeNull();
    if (display === "compact") expect(screen.getByText("check path")).toBeTruthy();
    fireEvent.click(header);
    expect(screen.getAllByText("E_PLAN_READONLY: 计划模式只读拦截")).toHaveLength(1);
    expect(screen.getByText("check path")).toBeTruthy();
    fireEvent.click(header);
    expect(screen.queryByText("E_PLAN_READONLY: 计划模式只读拦截")).toBeNull();
  });

  it("keeps error details folded when a running command fails", () => {
    const running = tool({ tool: "command", status: "running", outcome: undefined,
      argsPreview: '{"command":"which aliyun"}',
    });
    const { rerender } = render(<ToolCallCard display="compact" tool={running} />);
    const failed = { ...running, status: "error" as const,
      outcome: { ok: false, data: null, error: { code: "E_EXIT_CODE", message: "命令退出码 1" } },
    };
    rerender(<ToolCallCard display="compact" tool={failed} />);
    const header = screen.getByRole("button");
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByText("E_EXIT_CODE: 命令退出码 1")).toBeNull();
    fireEvent.click(header);
    expect(header.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getAllByText("E_EXIT_CODE: 命令退出码 1")).toHaveLength(1);
    fireEvent.click(header);
    expect(screen.queryByText("E_EXIT_CODE: 命令退出码 1")).toBeNull();
  });

  it("hides a running command tail until expanded while preserving full command", () => {
    const command = "pnpm --dir ui build " + "long-argument ".repeat(15);
    render(<ToolCallCard display="compact" tool={tool({ tool: "command", status: "running", outcome: undefined,
      argsPreview: JSON.stringify({ command }), progressTail: "building output",
    })} />);
    expect(screen.queryByText("building output")).toBeNull();
    const button = screen.getByRole("button");
    expect(button.textContent).toContain(command);
    fireEvent.click(button);
    expect(screen.getByText("building output")).toBeTruthy();
  });
});

describe("completed tool aggregation", () => {
  it("preserves open details on completion and across display changes", () => {
    useUi.setState({ outputDisplay: "compact" });
    const timeline: TimelineSeg[] = [{ kind: "tool", callKey: "read" }];
    const running = tool({ tool: "command", status: "running", argsPreview: '{"command":"echo hello"}', outcome: undefined });
    const { rerender } = render(<TimelineSegsView timeline={timeline} toolsMap={{ read: running }} />);
    fireEvent.click(screen.getByText("echo hello"));
    const complete = { ...running, status: "ok" as const, outcome: { ok: true, data: { exit_code: 0, output: "hello" } } };
    rerender(<TimelineSegsView timeline={timeline} toolsMap={{ read: complete }} />);
    expect(screen.getByText("hello")).toBeTruthy();
    fireEvent.click(document.querySelector(".tool-compact-head")!);
    expect(screen.queryByText("hello")).toBeNull();
    fireEvent.click(screen.getByText("echo hello"));
    // 切换使用两个真实选择器，避免绕过订阅更新。
    render(<OutputDisplaySelect />);
    fireEvent.click(screen.getByText("详细"));
    expect(screen.getByText("hello")).toBeTruthy();
    fireEvent.click(screen.getByText("简洁"));
    expect(screen.getByText("hello")).toBeTruthy();
  });
  it("only groups clean success, with nonzero exits and partial reads excluded", () => {
    expect(canGroupTool(tool())).toBe(true);
    for (const item of [
      tool({ status: "waiting" }), tool({ status: "running" }), tool({ tool: "ask" }), tool({ tool: "render_html" }),
      tool({ outcome: { ok: true, warnings: ["warning"] } }),
      tool({ tool: "command", outcome: { ok: true, data: { exit_code: 1 } } }),
      tool({ outcome: { ok: true, data: { files: [{ path: "a", error: "not found" }] } } }),
    ]) expect(canGroupTool(item)).toBe(false);
  });

  it("preserves prose order, never crosses prose/waiting/error boundaries, and expands details", () => {
    useUi.setState({ outputDisplay: "compact" });
    const toolsMap = {
      a: tool({ callKey: "a" }),
      b: tool({ callKey: "b", tool: "command", argsPreview: '{"command":"echo ok"}', outcome: { ok: true, data: { exit_code: 0, output: "ok" } } }),
      c: tool({ callKey: "c" }),
      wait: tool({ callKey: "wait", status: "waiting" }),
      fail: tool({ callKey: "fail", status: "error", outcome: { ok: false, error: { code: "E_IO", message: "failure" } } }),
    };
    const timeline: TimelineSeg[] = [
      { kind: "text", text: "before" }, { kind: "tool", callKey: "a" }, { kind: "tool", callKey: "b" },
      { kind: "text", text: "after" }, { kind: "tool", callKey: "c" },
      { kind: "tool", callKey: "wait" }, { kind: "tool", callKey: "fail" },
    ];
    render(<TimelineSegsView timeline={timeline} toolsMap={toolsMap} />);
    const groups = document.querySelectorAll(".tool-group-toggle");
    expect(groups.length).toBe(1);
    expect(groups[0].textContent).toContain("已运行 1 条命令");
    expect(screen.getByText("等待确认 读取")).toBeTruthy();
    const failure = document.querySelector(".tool-card.st-error")!;
    expect(failure.textContent).toContain("失败");
    expect(screen.queryByText("E_IO: failure")).toBeNull();
    fireEvent.click(failure.querySelector(".tool-compact-head")!);
    expect(screen.getByText("E_IO: failure")).toBeTruthy();
    expect(document.querySelectorAll(".md")[0].textContent?.trim()).toBe("before");
    expect(document.querySelectorAll(".md")[1].textContent?.trim()).toBe("after");
    fireEvent.click(groups[0]);
    fireEvent.click(screen.getByText("echo ok"));
    expect(screen.getByText("ok")).toBeTruthy();
  });

  it("keeps ask answers and widget preview out of collapsed groups", () => {
    useUi.setState({ outputDisplay: "compact" });
    const toolsMap = {
      ask: tool({ callKey: "ask", tool: "ask", argsPreview: JSON.stringify({ questions: [{ id: "q", question: "是否执行？", options: [{ id: "yes", label: "执行" }] }] }), outcome: { ok: true, data: { raw: { answers: { q: { selections: ["yes"] } } } } } }),
      widget: tool({ callKey: "widget", tool: "render_html", outcome: { ok: true, data: { html: "<div>preview</div>", title: "Widget" } } }),
    };
    render(<TimelineSegsView timeline={[{ kind: "tool", callKey: "ask" }, { kind: "tool", callKey: "widget" }]} toolsMap={toolsMap} />);
    expect(document.querySelector(".tool-group-toggle")).toBeNull();
    expect(screen.getByText("是否执行？")).toBeTruthy();
    expect(screen.getByRole("button", { name: /预\s*览/ })).toBeTruthy();
  });
});

describe("output reading anchor", () => {
  it.each([
    ["summary round trip", 110, "none", 110],
    ["first child round trip", 150, "none", 150],
    ["first child folded into summary", 250, "fold", 100],
    ["summary changed to detailed card", 110, "detail", 134],
  ] as const)("distinguishes matching summary/child keys: %s", (_, scroll, change, expected) => {
    const container = document.createElement("div");
    container.innerHTML = '<div data-key="message"><button data-output-key="tool:x" data-output-group="[&quot;tool:x&quot;]"></button><div class="body"><div data-output-key="tool:x"></div></div></div>';
    document.body.append(container);
    Object.defineProperties(container, { scrollTop: { value: scroll, writable: true }, scrollHeight: { value: 1200 }, clientHeight: { value: 200 } });
    container.getBoundingClientRect = () => ({ top: 0, height: 200 } as DOMRect);
    const summary = container.querySelector<HTMLElement>("button")!;
    const child = container.querySelector<HTMLElement>(".body > div")!;
    summary.getBoundingClientRect = () => ({ top: 100 - container.scrollTop, bottom: 132 - container.scrollTop, height: 32 } as DOMRect);
    child.getBoundingClientRect = () => ({ top: 134 - container.scrollTop, bottom: 334 - container.scrollTop, height: 200 } as DOMRect);
    const anchor = captureOutputAnchor(container);
    if (change === "fold") container.querySelector<HTMLElement>(".body")!.hidden = true;
    if (change === "detail") summary.remove();
    restoreOutputAnchor(container, anchor);
    expect(container.scrollTop).toBe(expected);
    container.remove();
  });

  it("anchors the second visible child of an expanded group, then finds it in detailed mode", () => {
    const container = document.createElement("div");
    container.innerHTML = '<div data-key="message"><div class="tool-activity-group"><button data-output-key="tool:a"></button><div data-output-key="tool:a"></div><div data-output-key="tool:b"></div></div></div>';
    document.body.append(container);
    Object.defineProperties(container, { scrollTop: { value: 150, writable: true }, scrollHeight: { value: 1200 }, clientHeight: { value: 200 } });
    container.getBoundingClientRect = () => ({ top: 0, bottom: 200, height: 200 } as DOMRect);
    const nodes = [...container.querySelectorAll<HTMLElement>("[data-output-key]")];
    nodes[0].getBoundingClientRect = () => ({ top: -150, bottom: -118, height: 32 } as DOMRect);
    nodes[1].getBoundingClientRect = () => ({ top: -118, bottom: -18, height: 100 } as DOMRect);
    nodes[2].getBoundingClientRect = () => ({ top: -18, bottom: 82, height: 100 } as DOMRect);
    const anchor = captureOutputAnchor(container);
    expect(anchor.segmentKey).toBe("tool:b");
    nodes[0].remove();
    nodes[1].getBoundingClientRect = () => ({ top: -150, bottom: -50, height: 100 } as DOMRect);
    nodes[2].getBoundingClientRect = () => ({ top: -50, bottom: 50, height: 100 } as DOMRect);
    restoreOutputAnchor(container, anchor);
    expect(container.scrollTop).toBe(118);
    container.remove();
  });

  it("finds the concise group summary when a detailed tool is folded into it", () => {
    const container = document.createElement("div");
    container.innerHTML = '<div data-key="message"><button data-output-key="tool:a" data-output-group="[&quot;tool:a&quot;,&quot;tool:b&quot;]"></button></div>';
    document.body.append(container);
    container.scrollTop = 200;
    container.getBoundingClientRect = () => ({ top: 0, bottom: 200, height: 200 } as DOMRect);
    container.querySelector("button")!.getBoundingClientRect = () => ({ top: -50, bottom: -18, height: 32 } as DOMRect);
    restoreOutputAnchor(container, { fallback: { kind: "bottom" }, messageKey: "message", segmentKey: "tool:b", offset: 10, atBottom: false });
    expect(container.scrollTop).toBe(150);
    container.remove();
  });
  it("preserves the visible paragraph when tools above it shrink inside a long message", () => {
    const container = document.createElement("div");
    container.innerHTML = '<div data-key="message"><div data-output-key="tool:a"></div><div data-output-key="text:2">reading</div></div>';
    document.body.append(container);
    let scroll = 520;
    let paragraphTop = 500;
    Object.defineProperties(container, {
      scrollTop: { get: () => scroll, set: (n: number) => { scroll = n; }, configurable: true },
      scrollHeight: { value: 1500, configurable: true }, clientHeight: { value: 300, configurable: true },
    });
    container.getBoundingClientRect = () => ({ top: 0, bottom: 300, height: 300 } as DOMRect);
    const toolNode = container.querySelector<HTMLElement>('[data-output-key="tool:a"]')!;
    const paragraph = container.querySelector<HTMLElement>('[data-output-key="text:2"]')!;
    toolNode.getBoundingClientRect = () => ({ top: -scroll, bottom: paragraphTop - scroll, height: paragraphTop } as DOMRect);
    paragraph.getBoundingClientRect = () => ({ top: paragraphTop - scroll, bottom: paragraphTop + 400 - scroll, height: 400 } as DOMRect);
    const anchor = captureOutputAnchor(container);
    expect(anchor.segmentKey).toBe("text:2");
    paragraphTop = 100;
    restoreOutputAnchor(container, anchor);
    expect(scroll).toBe(120);
    container.remove();
  });

  it("uses a visible segment in a child drawer without message anchors", () => {
    const container = document.createElement("div");
    container.innerHTML = '<div data-output-key="text:0">reading</div>';
    document.body.append(container);
    Object.defineProperties(container, {
      scrollTop: { value: 100, writable: true }, scrollHeight: { value: 1000 }, clientHeight: { value: 200 },
    });
    container.getBoundingClientRect = () => ({ top: 0, bottom: 200, height: 200 } as DOMRect);
    container.firstElementChild!.getBoundingClientRect = () => ({ top: -100, bottom: 400, height: 500 } as DOMRect);
    const anchor = captureOutputAnchor(container);
    expect(anchor.atBottom).toBe(false);
    expect(anchor.segmentKey).toBe("text:0");
    restoreOutputAnchor(container, anchor);
    expect(container.scrollTop).toBe(100);
    container.remove();
  });
});
