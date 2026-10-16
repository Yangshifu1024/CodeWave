import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../i18n";
import { TimelineSegsView } from "../features/chat/segments";
import { messagesToSubStream } from "../stores/runFrames";
import { useRun } from "../stores/run";
import { useUi } from "../stores/ui";
import type { Content, Message } from "../ipc/types";
import type { TimelineSeg, ToolView } from "../stores/run.types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  useRun.setState({ tabs: {} });
  useUi.setState({ outputDisplay: "detailed" });
});

const cases: { name: string; after: Content[] }[] = [
  { name: "纯思考", after: [] },
  { name: "思考后正文", after: [{ type: "text", text: "结论" }] },
  { name: "思考后工具", after: [{ type: "tool_use", id: "call", name: "read", args: { files: [{ path: "a.ts" }] } }] },
  { name: "思考后子代理", after: [{ type: "tool_use", id: "call", name: "subagent", args: { role: "explore", task: "检查源码" } }] },
];

describe("historical thinking has no live timing", () => {
  it.each(cases)("主会话恢复：$name", ({ after }) => {
    const messages: Message[] = [{ role: "assistant", content: [
      { type: "thinking", text: "已完成" }, { type: "thinking", text: "的推理" }, ...after,
    ] }, { role: "tool", content: [{ type: "tool_result", tool_use_id: "call", is_error: false,
      content: '{"sub_id":"child","report":"done"}',
    }] }];
    useRun.setState({ tabs: {} });
    useRun.getState().initTab("history");
    useRun.getState().restoreFromMessages("history", messages);
    const assistant = useRun.getState().tabs.history.items.find(item => item.kind === "assistant");
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("Missing restored assistant");
    expect(assistant.streaming).toBe(false);
    expect(assistant.timeline[0]).toEqual({ kind: "thinking", text: "已完成的推理" });
    expect(assistant.timeline).toHaveLength(1 + after.length);
  });

  it.each(cases)("子代理恢复：$name", ({ after }) => {
    const messages: Message[] = [{ role: "assistant", content: [{ type: "thinking", text: "已完成的推理" }, ...after] }];
    for (const settleRunning of [false, true]) {
      const restored = messagesToSubStream(messages, settleRunning);
      expect(restored.timeline[0]).toEqual({ kind: "thinking", text: "已完成的推理" });
      expect(restored.timeline).toHaveLength(1 + after.length);
    }
  });
});

describe("thinking timer follows the active stream", () => {
  const now = 1_800_000_000_000;
  const thinking: TimelineSeg = { kind: "thinking", text: "推理内容", startedAt: now - 29_000 };
  const label = (container: HTMLElement) => container.querySelector(".thinking-title")?.textContent;

  it("restored or interrupted thinking with a stale start stays static", () => {
    vi.useFakeTimers(); vi.setSystemTime(now);
    const { container } = render(<TimelineSegsView timeline={[thinking]} toolsMap={{}} streaming={false} />);
    expect(label(container)).toBe("思考过程");
    act(() => vi.advanceTimersByTime(8_500_000));
    expect(label(container)).toBe("思考过程");
  });

  it("live tail thinking grows, then stops on termination even without a saved duration", () => {
    vi.useFakeTimers(); vi.setSystemTime(now);
    const { container, rerender } = render(<TimelineSegsView timeline={[thinking]} toolsMap={{}} streaming />);
    expect(label(container)).toContain("29s");
    act(() => vi.advanceTimersByTime(2_000));
    expect(label(container)).toContain("31s");
    rerender(<TimelineSegsView timeline={[thinking]} toolsMap={{}} streaming={false} />);
    expect(label(container)).toBe("思考过程");
    act(() => vi.advanceTimersByTime(8_500_000));
    expect(label(container)).toBe("思考过程");
  });

  it("known duration stays frozen, and a non-tail segment never starts a timer", () => {
    vi.useFakeTimers(); vi.setSystemTime(now);
    const { container, rerender } = render(<TimelineSegsView timeline={[{ ...thinking, durationMs: 29_000 }]} toolsMap={{}} streaming />);
    expect(label(container)).toBe("思考完成（29s）");
    act(() => vi.advanceTimersByTime(2_000));
    expect(label(container)).toBe("思考完成（29s）");
    rerender(<TimelineSegsView timeline={[thinking, { kind: "thinking", text: "新的推理", startedAt: now }]} toolsMap={{}} streaming />);
    expect(container.querySelectorAll(".thinking-title")[0]?.textContent).toBe("思考过程");
  });
});

describe("concise folded summary of restored thinking", () => {
  const readTool: ToolView = {
    callKey: "call", tool: "read", status: "ok", progressTail: "",
    outcome: { ok: true, data: { files: [{ path: "a.ts" }] } },
  };
  it("reports a count and never a fabricated zero-second duration", () => {
    useUi.setState({ outputDisplay: "compact" });
    // 恢复的历史思考既无 startedAt 也无 durationMs（appendDelta 的 restored 分支不记时）
    const { container } = render(<TimelineSegsView
      timeline={[{ kind: "thinking", text: "已完成的推理" }, { kind: "tool", callKey: "call" }]}
      toolsMap={{ call: readTool }} />);
    const summary = container.querySelector(".tool-group-label")!.textContent!;
    expect(summary).toContain("思考 1 次");
    expect(summary).not.toContain("0s");
    expect(container.textContent).not.toContain("思考完成（0s）");
  });

  it("shows live seconds only for a tail segment that is still streaming", () => {
    const now = 1_800_000_000_000;
    vi.useFakeTimers(); vi.setSystemTime(now);
    useUi.setState({ outputDisplay: "compact" });
    // 思考在尾部且消息仍在流式 => 摘要跳秒
    const live: TimelineSeg[] = [
      { kind: "tool", callKey: "call" },
      { kind: "thinking", text: "推理中", startedAt: now - 6_000 },
    ];
    const { container, rerender } = render(<TimelineSegsView timeline={live} toolsMap={{ call: readTool }} streaming />);
    expect(container.querySelector(".tool-group-label")!.textContent).toContain("6s");
    act(() => vi.advanceTimersByTime(3_000));
    expect(container.querySelector(".tool-group-label")!.textContent).toContain("9s");
    // 定稿后定格为次数，不再跳秒
    rerender(<TimelineSegsView timeline={live} toolsMap={{ call: readTool }} streaming={false} />);
    expect(container.querySelector(".tool-group-label")!.textContent).toContain("思考 1 次");
  });

  it("does not count a thinking segment followed by a finished tool as live", () => {
    useUi.setState({ outputDisplay: "compact" });
    // 思考之后又跑完了工具：这段思考已经定格，摘要只能报次数
    const { container } = render(<TimelineSegsView
      timeline={[{ kind: "thinking", text: "推理", startedAt: 1_799_999_000_000 }, { kind: "tool", callKey: "call" }]}
      toolsMap={{ call: readTool }} streaming />);
    expect(container.querySelector(".tool-group-label")!.textContent).toContain("思考 1 次");
  });
});
