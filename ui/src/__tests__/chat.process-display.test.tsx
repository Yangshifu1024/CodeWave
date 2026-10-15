import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import ChatMessages from "../features/chat/ChatMessages";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import type { UiItem } from "../stores/run.types";
import { captureAnchor, collectNodes, restoreAnchor } from "../utils/scrollAnchor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));

const sessionId = "process-display";
const createdAt = "2026-10-14T13:14:27+08:00";
const makeCommands = (owner: string, count: number, time: string): UiItem => {
  const calls = Array.from({ length: count }, (_, index) => `${owner}${index}`);
  return { kind: "assistant", streaming: false, createdAt: time,
    timeline: [...calls.map(callKey => ({ kind: "tool" as const, callKey })), { kind: "text", text: "\n  " }],
    toolsMap: Object.fromEntries(calls.map(callKey => [callKey, {
      callKey, tool: "command", status: "ok" as const, progressTail: "",
      argsPreview: JSON.stringify({ command: `echo ${callKey}` }),
      outcome: { ok: true, data: { exit_code: 0, output: `result-${callKey}` } },
    }])),
  };
};
const processItem: UiItem = {
  kind: "assistant", createdAt, streaming: false,
  timeline: [{ kind: "tool", callKey: "read" }],
  toolsMap: { read: {
    callKey: "read", tool: "read", status: "ok", progressTail: "",
    outcome: { ok: true, data: { files: [{ path: "src/a.ts", content: "original file content" }] } },
  } },
};

beforeEach(() => {
  useSessions.setState({ tabs: [], activeKey: sessionId });
  useRun.setState({ tabs: {} });
  useRun.getState().initTab(sessionId);
  useUi.setState({ outputDisplay: "compact" });
});

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null });
  useRun.setState({ tabs: {} });
  useUi.setState({ outputDisplay: "detailed" });
});

describe("compact process metadata", () => {
  it("repairs glued headings only in assistant messages while retaining the original text", () => {
    const text = "## 两点提示1. **登录**：正文\n2. **统计**：正文";
    useRun.setState(state => {
      state.tabs[sessionId].items = [
        { kind: "user", text },
        { kind: "assistant", streaming: false, toolsMap: {}, timeline: [{ kind: "text", text }] },
      ];
    });
    const originalItems = useRun.getState().tabs[sessionId].items;
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".assistant h2")?.textContent).toBe("两点提示");
    expect(container.querySelectorAll(".assistant li")).toHaveLength(2);
    expect(container.querySelector(".user h2")?.textContent).toContain("两点提示1.");
    expect(useRun.getState().tabs[sessionId].items).toBe(originalItems);
    expect(originalItems[0].kind === "user" && originalItems[0].text).toBe(text);
  });

  it("keeps transcript identities and open tool content when switching to detailed mode", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [processItem, {
        kind: "assistant", createdAt, streaming: false,
        timeline: [{ kind: "text", text: "排查结论保持原样" }], toolsMap: {},
      }];
      state.tabs[sessionId].itemKeys = ["process", "answer"];
    });
    const originalItems = useRun.getState().tabs[sessionId].items;
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".output-display-select")).toBeNull();
    const process = container.querySelector('[data-key="process"]')!;
    expect(process.classList.contains("process-only")).toBe(true);
    expect(container.querySelector('[data-key="answer"]')?.classList.contains("process-only")).toBe(false);
    expect(container.querySelector(".msg .role")).toBeNull();
    fireEvent.click(process.querySelector(".tool-compact-head")!);
    expect(process.textContent).toContain("original file content");
    act(() => useUi.getState().setOutputDisplay("detailed"));
    expect(useUi.getState().outputDisplay).toBe("detailed");
    expect(process.classList.contains("process-only")).toBe(false);
    expect(container.querySelector(".msg .role")).toBeNull();
    expect(process.textContent).toContain("original file content");
    expect(useRun.getState().tabs[sessionId].items).toBe(originalItems);
  });

  it("keeps mixed prose visible and does not invent a missing timestamp", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [{
        ...processItem, createdAt: undefined,
        timeline: [{ kind: "text", text: "正在核对源码" }, { kind: "tool", callKey: "read" }],
      }];
    });
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".process-only")).toBeNull();
    expect(container.querySelector(".md")?.textContent).toContain("正在核对源码");
    expect(container.querySelector(".msg .role")).toBeNull();
  });

  it("uses process spacing for mixed messages ending in a tool, without empty text rows", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [{
        ...processItem,
        timeline: [{ kind: "text", text: "交叉验证" }, { kind: "tool", callKey: "read" }, { kind: "text", text: "\n  " }],
      }, processItem];
      state.tabs[sessionId].itemKeys = ["mixed", "next"];
    });
    const originalItems = useRun.getState().tabs[sessionId].items;
    const { container } = render(<App><ChatMessages /></App>);
    const mixed = container.querySelector('[data-key="mixed"]')!;
    expect(mixed.classList.contains("process-only")).toBe(false);
    expect(mixed.classList.contains("process-tail")).toBe(true);
    expect(mixed.querySelectorAll(".md")).toHaveLength(1);
    expect(mixed.querySelector('.md[data-output-key="text:0"]')?.textContent).toContain("交叉验证");
    expect(container.querySelector('[data-key="next"]')?.classList.contains("process-tail")).toBe(true);
    expect(useRun.getState().tabs[sessionId].items).toBe(originalItems);
    act(() => useUi.getState().setOutputDisplay("detailed"));
    expect(mixed.classList.contains("process-tail")).toBe(false);
    expect(mixed.querySelectorAll(".md")).toHaveLength(2);
  });

  it("does not treat a filtered subagent tool as the last visible process row", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [{
        ...processItem,
        timeline: [{ kind: "text", text: "已收到汇报" }, { kind: "tool", callKey: "child" }],
        toolsMap: { child: { callKey: "child", tool: "subagent", status: "ok", progressTail: "" } },
      }];
    });
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".process-tail")).toBeNull();
    expect(container.querySelector(".tool-card")).toBeNull();
    expect(container.querySelector(".md")?.textContent).toContain("已收到汇报");
  });

  it("shows five commands once across three messages and keeps an opened call across mode switches", () => {
    const first = makeCommands("a", 2, createdAt);
    if (first.kind === "assistant") first.timeline.unshift({ kind: "text", text: "1097 行日志已落盘。开始聚类分析。" });
    const secondTime = "2026-10-14T13:14:25+08:00";
    useRun.setState(state => {
      state.tabs[sessionId].items = [first, makeCommands("b", 2, secondTime), makeCommands("c", 1, "2026-10-14T13:14:27+08:00")];
      state.tabs[sessionId].itemKeys = ["owner-a", "owner-b", "owner-c"];
    });
    const originalItems = useRun.getState().tabs[sessionId].items;
    const { container } = render(<App><ChatMessages /></App>);
    const groups = container.querySelectorAll(".tool-group-toggle");
    expect(groups).toHaveLength(1);
    expect(groups[0].textContent).toContain("已运行 5 条命令");
    expect(collectNodes(container).map(node => node.key)).toEqual(["owner-a", "owner-b", "owner-c"]);
    expect(container.querySelector(".md")?.textContent).toContain("1097 行日志已落盘");
    fireEvent.click(groups[0]);
    expect(container.querySelectorAll(".tool-compact-head")).toHaveLength(5);
    expect(container.querySelectorAll(`.tool-time[title="${secondTime}"]`)).toHaveLength(2);
    fireEvent.click(container.querySelector('[data-output-key="tool:b0"] .tool-compact-head')!);
    expect(container.querySelector('[data-key="owner-b"]')?.textContent).toContain("result-b0");
    act(() => useUi.getState().setOutputDisplay("detailed"));
    expect(container.querySelector('[data-key="owner-b"]')?.textContent).toContain("result-b0");
    act(() => useUi.getState().setOutputDisplay("compact"));
    expect(container.querySelector('[data-key="owner-b"]')?.textContent).toContain("result-b0");
    expect(useRun.getState().tabs[sessionId].items).toBe(originalItems);
  });

  it("keeps an opened group and its reading anchor when earlier history is prepended", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [makeCommands("a", 2, createdAt), makeCommands("b", 2, createdAt), makeCommands("c", 1, createdAt)];
      state.tabs[sessionId].itemKeys = ["owner-a", "owner-b", "owner-c"];
    });
    const { container } = render(<App><ChatMessages /></App>);
    const root = container.querySelector<HTMLElement>('[data-key="owner-a"]')!;
    const group = root.querySelector<HTMLElement>(".tool-group-toggle")!;
    fireEvent.click(group);
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(scroller, { scrollTop: { value: 250, writable: true }, scrollHeight: { value: 1000 }, clientHeight: { value: 200 } });
    scroller.getBoundingClientRect = () => ({ top: 0, height: 200 } as DOMRect);
    let shift = 0;
    root.getBoundingClientRect = () => ({ top: shift - scroller.scrollTop, height: 300 } as DOMRect);
    root.querySelectorAll<HTMLElement>("[data-anchor-aliases]").forEach((node, index) => {
      node.getBoundingClientRect = () => ({ top: shift + index * 50 - scroller.scrollTop, height: 32 } as DOMRect);
    });
    const anchor = captureAnchor(scroller);
    expect(anchor.kind === "item" && anchor.key).toBe("owner-c");
    act(() => useRun.setState(state => {
      state.tabs[sessionId].items.unshift(makeCommands("p", 1, createdAt));
      state.tabs[sessionId].itemKeys!.unshift("earlier");
    }));
    expect(container.querySelector('[data-key="owner-a"]')).toBe(root);
    expect(root.querySelector(".tool-group-toggle")).toBe(group);
    expect(group.getAttribute("aria-expanded")).toBe("true");
    expect(root.querySelectorAll(".tool-compact-head")).toHaveLength(5);
    shift = 40;
    expect(restoreAnchor(scroller, anchor)).toBe(true);
    expect(scroller.scrollTop).toBe(290);
    act(() => useUi.getState().setOutputDisplay("detailed"));
    act(() => useUi.getState().setOutputDisplay("compact"));
    expect(root.querySelector(".tool-group-toggle")?.getAttribute("aria-expanded")).toBe("true");
  });

  it("allows aggregation again after closing a single call's details", () => {
    useRun.setState(state => {
      state.tabs[sessionId].items = [makeCommands("b", 1, createdAt)];
      state.tabs[sessionId].itemKeys = ["owner-b"];
    });
    const { container } = render(<App><ChatMessages /></App>);
    fireEvent.click(container.querySelector(".tool-compact-head")!);
    expect(container.textContent).toContain("result-b0");
    fireEvent.click(container.querySelector(".tool-compact-head")!);
    act(() => useRun.setState(state => {
      state.tabs[sessionId].items.unshift(makeCommands("a", 1, createdAt));
      state.tabs[sessionId].itemKeys!.unshift("owner-a");
    }));
    expect(container.querySelectorAll(".tool-group-toggle")).toHaveLength(1);
    expect(container.querySelector(".tool-group-toggle")?.textContent).toContain("已运行 2 条命令");
    expect(container.querySelector('[data-key="owner-b"]')).toBeNull();
  });

  it("merges three thinking-plus-tool messages into one row and keeps every message anchor", () => {
    const withThinking = (owner: string, count: number): UiItem => {
      const base = makeCommands(owner, count, createdAt);
      if (base.kind === "assistant") base.timeline.unshift({ kind: "thinking", text: `推理 ${owner}` });
      return base;
    };
    useRun.setState(state => {
      state.tabs[sessionId].items = [withThinking("a", 1), withThinking("b", 1), withThinking("c", 1)];
      state.tabs[sessionId].itemKeys = ["owner-a", "owner-b", "owner-c"];
    });
    const originalItems = useRun.getState().tabs[sessionId].items;
    const { container } = render(<App><ChatMessages /></App>);
    const groups = container.querySelectorAll(".tool-group-toggle");
    expect(groups).toHaveLength(1);
    expect(groups[0].textContent).toContain("思考 3 次");
    expect(groups[0].textContent).toContain("已运行 3 条命令");
    // 三条源消息仍各自可被滚动锚点定位
    expect(collectNodes(container).map(node => node.key)).toEqual(["owner-a", "owner-b", "owner-c"]);
    expect(useRun.getState().tabs[sessionId].items).toBe(originalItems);
  });

  it("keeps an opened folded group across display mode switches", () => {
    useRun.setState(state => {
      const first = makeCommands("a", 1, createdAt);
      if (first.kind === "assistant") first.timeline.unshift({ kind: "thinking", text: "推理" });
      const second = makeCommands("b", 1, createdAt);
      if (second.kind === "assistant") second.timeline.unshift({ kind: "thinking", text: "继续推理" });
      state.tabs[sessionId].items = [first, second];
      state.tabs[sessionId].itemKeys = ["owner-a", "owner-b"];
    });
    const { container } = render(<App><ChatMessages /></App>);
    const group = container.querySelector(".tool-group-toggle")!;
    fireEvent.click(group);
    expect(container.querySelectorAll(".tool-group-body .thinking-block")).toHaveLength(2);
    act(() => useUi.getState().setOutputDisplay("detailed"));
    act(() => useUi.getState().setOutputDisplay("compact"));
    // 展开态按会话 / 原消息键 / 根键保存，来回切换不自动关闭
    expect(container.querySelector(".tool-group-toggle")?.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelectorAll(".tool-group-body .thinking-block")).toHaveLength(2);
  });
});
