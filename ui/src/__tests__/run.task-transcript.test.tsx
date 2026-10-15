import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { App } from "antd";
import "../i18n";
import ChatMessages from "../features/chat/ChatMessages";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { ipc } from "../ipc/client";
import { copyText } from "../utils/clipboard";
import type { RunDisplay } from "../ipc/types";

const wire = vi.hoisted(() => ({ channel: null as { onmessage: (frame: unknown) => void } | null }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class { onmessage: unknown; } }));
vi.mock("../utils/clipboard", () => ({ copyText: vi.fn(async () => true) }));
vi.mock("../ipc/client", () => ({ ipc: {
  startChat: vi.fn(async (_session: string, _text: string, _images: unknown[], channel: { onmessage: (frame: unknown) => void }) => { wire.channel = channel; return "r1"; }),
  gitStatus: vi.fn(async () => []),
  sessionRunSnapshot: vi.fn(),
} }));

const session = "live-task-transcript";
const preface = "我先检查源码，再核实测试。\n\n";
const finalAnswer = "## 总结\n已完成核查。\n\n## 建议\n执行手动验收。";
const metadata: RunDisplay = {
  run_id: "r1", status: "done", started_at: "2026-10-16T01:00:00Z",
  user_created_at: "2026-10-16T01:00:00.000001Z", finished_at: "2026-10-16T01:00:20Z",
  duration_ms: 20_000, final_answer: finalAnswer, final_answer_created_at: "2026-10-16T01:00:19.123456Z",
};

beforeEach(() => {
  vi.mocked(copyText).mockReset().mockResolvedValue(true);
  wire.channel = null;
  useRun.setState({ tabs: {}, drafts: {} });
  useRun.getState().initTab(session);
  useSessions.setState({ tabs: [], activeKey: session });
  useUi.setState({ outputDisplay: "compact" });
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  useRun.setState({ tabs: {}, drafts: {} });
  useSessions.setState({ tabs: [], activeKey: null });
  useUi.setState({ outputDisplay: "detailed" });
});

describe("step-aware text deferral and painted final handoff", () => {
  function controlledPaints() {
    let next = 0;
    let now = performance.now();
    vi.spyOn(performance, "now").mockImplementation(() => now);
    const frames = new Map<number, FrameRequestCallback>();
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.set(++next, callback); return next; });
    vi.stubGlobal("cancelAnimationFrame", (id: number) => frames.delete(id));
    return (elapsedMs = 16) => act(() => {
      now += elapsedMs;
      const batch = [...frames.values()];
      frames.clear();
      for (const callback of batch) callback(performance.now());
    });
  }
  const emit = (text: string, gen = 0) => wire.channel!.onmessage({ type: "delta_text", gen, text });
  const step = (gen = 0) => wire.channel!.onmessage({ type: "step_start", gen });
  const textOf = () => useRun.getState().tabs[session].items.flatMap(item => item.kind === "assistant"
    ? item.timeline.flatMap(seg => seg.kind === "text" ? [seg.text] : []) : []).join("");

  it("holds candidate text, releases only confirmed previous steps, and paints collapsed process before its final", async () => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => { step(); emit(preface); });
    expect(container.textContent).not.toContain(preface.trim());
    expect(container.querySelector('[role="status"]')).not.toBeNull();
    act(() => {
      handlers["tool:start"]({ session, call_key: "b:0", tool: "read", phase: "running" });
      handlers["tool:result"]({ session, call_key: "b:0", tool: "read", outcome: { ok: true, data: { files: [] } } });
    });
    expect(container.querySelector('[data-output-key="tool:b:0"]')).not.toBeNull();
    expect(container.textContent).not.toContain(preface.trim());
    act(() => step(1));
    expect(container.textContent).toContain(preface.trim());
    act(() => { emit(finalAnswer.slice(0, 8), 1); emit(finalAnswer.slice(8), 1); });
    expect(container.querySelector("h2")).toBeNull();
    expect(textOf()).toBe(preface + finalAnswer);
    act(() => handlers["run:done"]({ session, run_id: "r1", task_run: metadata, suggestions: ["建议继续"] }));
    const header = container.querySelector<HTMLElement>(".task-process .ant-collapse-header")!;
    expect(header.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector("h2")).toBeNull();
    expect(container.querySelector(".chips")).toBeNull();
    expect(container.querySelector(".final-answer-actions")).toBeNull();
    paint();
    expect(container.querySelector("h2")).toBeNull();
    // The user's reading action during handoff must not be reset when final text is revealed.
    fireEvent.click(header);
    expect(header.getAttribute("aria-expanded")).toBe("true");
    paint();
    expect(container.querySelector('[data-final-revealing]')).not.toBeNull();
    expect(container.querySelector(".chips")).toBeNull();
    expect(container.querySelector(".final-answer-actions")).toBeNull();
    expect(useRun.getState().tabs[session].running).toBe(false);
    paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector(".chips")?.textContent).toContain("建议继续");
    expect(header.getAttribute("aria-expanded")).toBe("true");
    expect(container.querySelector(".task-process")?.textContent).not.toContain("已完成核查");
    expect(useRun.getState().tabs[session].pendingFinals).toEqual({});
    expect(container.querySelectorAll(".final-answer-actions")).toHaveLength(1);
    await act(async () => { fireEvent.click(container.querySelector('button[aria-label="复制答复"]')!); });
    expect(copyText).toHaveBeenCalledExactlyOnceWith(finalAnswer);
    expect(container.querySelector(".final-answer-actions")?.textContent).toContain("已复制");
  });

  it.each(["false", "reject"])("copies a restored answer verbatim and exposes clipboard failure (%s) with retry", async failure => {
    const complete = "## 总结\n保留 **Markdown** 和换行。\n\n```ts\n  const ready = true;\n```\n\n## 建议\n参考 [说明](https://example.com) 🚀";
    useRun.getState().restoreFromMessages(session, [
      { role: "user", created_at: metadata.user_created_at, content: [{ type: "text", text: "检查改动" }] },
      { role: "assistant", created_at: metadata.started_at, content: [{ type: "text", text: preface }] },
      { role: "assistant", created_at: metadata.final_answer_created_at, content: [{ type: "text", text: complete }] },
    ], { task_runs: [{ ...metadata, final_answer: complete }] });
    const { container } = render(<App><ChatMessages /></App>);
    const button = container.querySelector<HTMLButtonElement>('button[aria-label="复制答复"]')!;
    expect(button).not.toBeNull();
    expect(button.closest(".task-process")).toBeNull();
    expect(container.querySelectorAll(".final-answer-actions")).toHaveLength(1);
    if (failure === "reject") vi.mocked(copyText).mockRejectedValueOnce(new Error("Clipboard unavailable"));
    else vi.mocked(copyText).mockResolvedValueOnce(false);
    await act(async () => { fireEvent.click(button); });
    expect(copyText).toHaveBeenLastCalledWith(complete);
    expect(container.querySelector(".final-answer-actions")?.textContent).toContain("复制失败");
    expect(container.querySelector(".final-answer-actions")?.textContent).not.toContain("已复制");
    await act(async () => { fireEvent.click(button); });
    expect(copyText).toHaveBeenCalledTimes(2);
    expect(copyText).toHaveBeenLastCalledWith(complete);
    expect(container.querySelector(".final-answer-actions")?.textContent).toContain("已复制");
  });

  it.each(["", finalAnswer.slice(0, 9), "模型留下的不同候选"])("reconciles authoritative done before the final Channel tail (%s)", async candidate => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => { step(); emit(preface); step(1); emit(candidate, 1); });
    act(() => handlers["run:done"]({ session, run_id: "r1", task_run: metadata }));
    expect(container.querySelector(".task-process .ant-collapse-header")?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector("h2")).toBeNull();
    const beforeLateFrame = textOf();
    act(() => emit(finalAnswer, 1));
    expect(textOf()).toBe(beforeLateFrame);
    paint(); paint(); paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector(".task-process")?.textContent ?? "").not.toContain("已完成核查");
  });

  it("stages a pure text answer without inventing an empty process header", async () => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("纯文本问题"); });
    act(() => { step(); emit(finalAnswer); });
    expect(container.querySelector("h2")).toBeNull();
    act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    expect(container.querySelector(".task-process")).toBeNull();
    expect(container.textContent).toContain("总耗时 20 秒");
    expect(container.querySelector("h2")).toBeNull();
    paint(); paint(); paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
  });

  it("keeps an authoritative final even when done overtakes the entire Channel", async () => {
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(textOf()).toBe(finalAnswer);
    act(() => { step(); emit(finalAnswer); });
    expect(textOf()).toBe(finalAnswer);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
  });

  it("does not duplicate an already complete final when a notice arrives at the tail", async () => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => {
      step(); emit(preface); step(1); emit(finalAnswer, 1);
      handlers["run:inject"]({ session, texts: ["追加说明"], count: 1 });
      handlers["run:done"]({ session, run_id: "r1", task_run: metadata });
    });
    expect(textOf()).toBe(preface + finalAnswer);
    expect(container.querySelector("h2")).toBeNull();
    paint(); paint(); paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    const header = container.querySelector<HTMLElement>(".task-process .ant-collapse-header")!;
    fireEvent.click(header);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector(".task-process")?.textContent).not.toContain("已完成核查");
  });

  it("keeps the previous final handoff when a queued run starts before its paint", async () => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); step(1); emit(finalAnswer, 1); });
    await act(async () => { await useRun.getState().send("排队的第二轮"); });
    vi.mocked(ipc.startChat).mockImplementationOnce(async (_s, _t, _i, channel) => { wire.channel = channel; return "r2"; });
    await act(async () => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    expect(useRun.getState().tabs[session].runId).toBe("r2");
    expect(useRun.getState().tabs[session].pendingFinals?.r1).toBe(true);
    act(() => { step(); emit("第二轮暂存正文"); });
    expect(container.querySelector("h2")).toBeNull();
    expect(container.textContent).not.toContain("第二轮暂存正文");
    paint(); paint(); paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.textContent).not.toContain("第二轮暂存正文");
    expect(useRun.getState().tabs[session].finalReveals?.r1).toBeUndefined();
    const finalUser = [...container.querySelectorAll(".msg.user")].find(node => node.textContent?.includes("排队的第二轮"));
    expect(finalUser?.closest(".task-process")).toBeNull();
  });

  it("finishes an offscreen handoff without replaying it when returning to its Tab", async () => {
    controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); step(1); emit(finalAnswer, 1); });
    act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    expect(container.querySelector("h2")).toBeNull();
    act(() => { useRun.getState().initTab("other"); useSessions.setState({ activeKey: "other" }); });
    expect(useRun.getState().tabs[session].pendingFinals).toEqual({});
    act(() => useSessions.setState({ activeKey: session }));
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector(".task-process .ant-collapse-header")?.getAttribute("aria-expanded")).toBe("false");
  });

  it("finishes a partially animated final on Tab leave and never replays it on return", async () => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); step(1); emit(finalAnswer, 1); });
    act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    paint(); paint();
    expect(container.querySelector("[data-final-revealing]")).not.toBeNull();
    expect(useRun.getState().tabs[session].finalReveals?.r1).toBe(true);
    act(() => { useRun.getState().initTab("other"); useSessions.setState({ activeKey: "other" }); });
    act(() => useSessions.setState({ activeKey: session }));
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector("[data-final-revealing]")).toBeNull();
    expect(useRun.getState().tabs[session].finalReveals?.r1).toBeUndefined();
  });

  it("finishes a local final animation when the page becomes hidden", async () => {
    const paint = controlledPaints();
    const previousRequests = vi.mocked(ipc.startChat).mock.calls.length;
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(finalAnswer); });
    act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
    paint(); paint();
    expect(container.querySelector("[data-final-revealing]")).not.toBeNull();
    act(() => { vi.spyOn(document, "hidden", "get").mockReturnValue(true); document.dispatchEvent(new Event("visibilitychange")); });
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector("[data-final-revealing]")).toBeNull();
    expect(useRun.getState().tabs[session].running).toBe(false);
    expect(ipc.startChat).toHaveBeenCalledTimes(previousRequests + 1);
  });

  it.each([false, true])("follows revealed final text only while the reader stays at the bottom (reading=%s)", async reading => {
    const paint = controlledPaints();
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); step(1); emit(finalAnswer, 1); });
    paint(); // The original send-to-bottom frame has already painted before a user can start reading.
    const scroller = container.querySelector<HTMLElement>(".chat-messages")!;
    Object.defineProperties(scroller, {
      scrollHeight: { configurable: true, get: () => container.querySelector("h2") ? 1000 : 600 },
      clientHeight: { configurable: true, value: 200 },
    });
    scroller.scrollTo = options => { if (typeof options === "object") scroller.scrollTop = Math.max(0, Math.min(options.top ?? 0, scroller.scrollHeight - scroller.clientHeight)); };
    const geometry = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function(this: HTMLElement) {
      const logical = this.matches(".task-process [data-anchor-aliases]") || this.closest(".msg.assistant") ? 100 : 0;
      const top = this === scroller ? 0 : logical - scroller.scrollTop;
      return { top, bottom: top + 32, height: 32, left: 0, right: 300, width: 300, x: 0, y: top, toJSON: () => ({}) };
    });
    try {
      scroller.scrollTop = reading ? 110 : 400;
      if (reading) { fireEvent.wheel(scroller, { deltaY: -50 }); fireEvent.scroll(scroller); }
      act(() => useRun.getState().bindGlobalHandlers()["run:done"]({ session, run_id: "r1", task_run: metadata }));
      paint(); paint(); paint(1200);
      expect(container.querySelectorAll("h2")).toHaveLength(2);
      expect(scroller.scrollTop).toBe(reading ? 100 : 800);
    } finally { geometry.mockRestore(); }
  });

  it.each(["error", "cancelled", "no final"])("releases all candidate text on %s and can start a fresh step-aware run", async terminal => {
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); });
    expect(container.textContent).not.toContain(preface.trim());
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => handlers[terminal === "no final" ? "run:done" : `run:${terminal}`]({ session, run_id: "r1", error: "失败原因" }));
    expect(container.textContent).toContain(preface.trim());
    expect(useRun.getState().tabs[session].pendingFinals).toBeUndefined();
    vi.mocked(ipc.startChat).mockImplementationOnce(async (_s, _t, _i, channel) => { wire.channel = channel; return "r2"; });
    await act(async () => { await useRun.getState().send("继续"); });
    act(() => { step(); emit("新一轮候选"); });
    expect(container.textContent).not.toContain("新一轮候选");
    act(() => handlers["run:cancelled"]({ session, run_id: "r2" }));
    expect(container.textContent).toContain("新一轮候选");
  });

  it("handles retry deltas before their event, rejects late retries, and preserves earlier tools and subagents", async () => {
    await useRun.getState().send("检查改动");
    const handlers = useRun.getState().bindGlobalHandlers();
    step(); emit(preface); step(1);
    emit("失败候选", 1);
    handlers["tool:result"]({ session, call_key: "late", tool: "read", outcome: { ok: true, data: { files: [] } } });
    handlers["sub:spawn"]({ session, sub_id: "late-sub", role: "tester", description: "已执行的并行检查" });
    emit(finalAnswer.slice(0, 8), 2); // retry's Channel overtakes its event
    handlers["run:retry"]({ session, gen: 2, attempt: 1 });
    emit(finalAnswer.slice(8), 2);
    emit("旧失败帧", 1);
    expect(textOf()).toBe(preface + finalAnswer);
    const assistant = useRun.getState().tabs[session].items.find(item => item.kind === "assistant")!;
    expect(assistant.kind === "assistant" && assistant.toolsMap.late).toBeDefined();
    expect(assistant.kind === "assistant" && assistant.timeline.some(seg => seg.kind === "sub" && seg.subId === "late-sub")).toBe(true);
    step(3); // actual next step releases the previous successful retry
    handlers["run:retry"]({ session, gen: 2, attempt: 1 });
    expect(useRun.getState().tabs[session].streamGen).toBe(3);
    expect(textOf()).toBe(preface + finalAnswer);
    emit("下一步骤", 3);
    handlers["run:retry"]({ session, gen: 4, attempt: 2 });
    expect(textOf()).toBe(preface + finalAnswer);
    expect(useRun.getState().tabs[session].items.some(item => item.kind === "assistant" && !!item.toolsMap.late)).toBe(true);
  });

  it("rejects retry events for a different run or an already settled run", async () => {
    await useRun.getState().send("检查改动");
    const handlers = useRun.getState().bindGlobalHandlers();
    step(); emit(preface);
    handlers["run:retry"]({ session, run_id: "old", gen: 99, attempt: 1 });
    expect(textOf()).toBe(preface);
    expect(useRun.getState().tabs[session].streamGen).toBe(0);
    handlers["run:cancelled"]({ session, run_id: "r1" });
    const before = useRun.getState().tabs[session].items;
    handlers["run:retry"]({ session, run_id: "r1", gen: 99, attempt: 1 });
    expect(useRun.getState().tabs[session].items).toBe(before);
  });

  it("uses step_start rather than a generation change to release and separate adjacent text", async () => {
    await useRun.getState().send("检查改动");
    step(); emit(preface); step(); emit("本步骤候选");
    const assistant = useRun.getState().tabs[session].items.find(item => item.kind === "assistant")!;
    expect(assistant.kind === "assistant" && assistant.timeline).toMatchObject([
      { kind: "text", text: preface }, { kind: "text", text: "本步骤候选", deferred: true },
    ]);
    expect(assistant.kind === "assistant" && assistant.timeline[0].kind === "text" && assistant.timeline[0].deferred).toBeUndefined();
  });

  it("ignores an unverified retry identity while start_chat is pending", async () => {
    let finishStart!: (runId: string) => void;
    vi.mocked(ipc.startChat).mockImplementationOnce(async (_s, _t, _i, channel) => {
      wire.channel = channel;
      return new Promise<string>(resolve => { finishStart = resolve; });
    });
    const sending = useRun.getState().send("检查改动");
    useRun.getState().bindGlobalHandlers()["run:retry"]({ session, run_id: "old", gen: 99 });
    step(); emit(preface);
    expect(useRun.getState().tabs[session].streamGen).toBe(0);
    expect(textOf()).toBe(preface);
    finishStart("r1");
    await sending;
    expect(useRun.getState().tabs[session].runId).toBe("r1");
  });

  it("recovers a lost terminal through snapshot and does not replay the handoff on history restore", async () => {
    const paint = controlledPaints();
    const { container, unmount } = render(<App><ChatMessages /></App>);
    await act(async () => { await useRun.getState().send("检查改动"); });
    act(() => { step(); emit(preface); step(1); emit(finalAnswer, 1); });
    vi.mocked(ipc.sessionRunSnapshot).mockResolvedValue({ running: false, run_id: "r1", terminal: { run_id: "r1", kind: "done", task_run: metadata } });
    await act(async () => { await useRun.getState().reconcileRun(session); });
    expect(container.querySelector(".task-process")).not.toBeNull();
    expect(container.querySelector("h2")).toBeNull();
    paint(); paint(); paint(1200);
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    unmount();
    useRun.getState().restoreFromMessages(session, [
      { role: "user", created_at: metadata.user_created_at, content: [{ type: "text", text: "检查改动" }] },
      { role: "assistant", created_at: metadata.started_at, content: [{ type: "text", text: preface }] },
      { role: "assistant", created_at: metadata.final_answer_created_at, content: [{ type: "text", text: finalAnswer }] },
    ], { task_runs: [metadata] });
    const restored = render(<App><ChatMessages /></App>).container;
    expect(restored.querySelectorAll("h2")).toHaveLength(2);
    expect(useRun.getState().tabs[session].pendingFinals).toBeUndefined();
  });
});

describe("real send/channel/terminal to task rendering", () => {
  it.each(["tool step", "merged text"])("collapses a live multi-step item after %s while exposing only the final answer", async variant => {
    const { container } = render(<App><ChatMessages /></App>);
    await act(async () => { expect(await useRun.getState().send("检查改动")).toBe(true); });
    const handlers = useRun.getState().bindGlobalHandlers();
    const text = (value: string) => wire.channel!.onmessage({ type: "delta_text", gen: 0, text: value });
    act(() => {
      text(preface);
      if (variant === "tool step") {
        handlers["tool:start"]({ session, call_key: "b:0", tool: "read", phase: "running", args_preview: "{}" });
        handlers["tool:result"]({ session, call_key: "b:0", tool: "read", outcome: { ok: true, data: { files: [] } }, duration_ms: 10 });
      }
      text(finalAnswer.slice(0, 8));
      text(finalAnswer.slice(8));
    });
    expect(useRun.getState().tabs[session].items.filter(item => item.kind === "assistant")).toHaveLength(1);
    expect(container.querySelector(".task-process")).toBeNull();

    act(() => handlers["run:done"]({ session, run_id: "r1", task_run: metadata }));
    const tab = useRun.getState().tabs[session];
    const assistant = tab.items.find(item => item.kind === "assistant")!;
    expect(assistant.kind === "assistant" && assistant.timeline.filter(seg => seg.kind === "text").map(seg => seg.text).join("")).toBe(preface + finalAnswer);
    const header = container.querySelector<HTMLElement>(".task-process .ant-collapse-header");
    expect(header?.getAttribute("aria-expanded")).toBe("false");
    expect(header?.textContent).toContain("总耗时 20 秒");
    expect(container.querySelectorAll("h2")).toHaveLength(2);
    expect(container.querySelector("h2")?.closest("[hidden]")).toBeNull();
    fireEvent.click(header!);
    expect(container.querySelector(".task-process")?.textContent).toContain(preface.trim());
    expect(container.querySelector(".task-process")?.textContent).not.toContain("已完成核查");

    vi.mocked(ipc.startChat).mockResolvedValueOnce("r2");
    await act(async () => { expect(await useRun.getState().send("下一轮新问题")).toBe(true); });
    const nextUser = [...container.querySelectorAll(".msg.user")].find(node => node.textContent?.includes("下一轮新问题"));
    expect(nextUser).toBeDefined();
    expect(nextUser!.closest(".task-process")).toBeNull();
  });

  it("uses the same final suffix after a lost terminal is recovered by snapshot", async () => {
    await useRun.getState().send("检查改动");
    wire.channel!.onmessage({ type: "delta_text", gen: 0, text: preface + finalAnswer });
    vi.mocked(ipc.sessionRunSnapshot).mockResolvedValue({ running: false, run_id: "r1", terminal: { run_id: "r1", kind: "done", task_run: metadata } });
    await useRun.getState().reconcileRun(session);
    const { container } = render(<App><ChatMessages /></App>);
    expect(container.querySelector(".task-process .ant-collapse-header")?.getAttribute("aria-expanded")).toBe("false");
    expect(container.querySelector("h2")?.textContent).toBe("总结");

    cleanup();
    useRun.getState().restoreFromMessages(session, [
      { role: "user", created_at: metadata.user_created_at, content: [{ type: "text", text: "检查改动" }] },
      { role: "assistant", created_at: metadata.started_at, content: [{ type: "text", text: preface }] },
      { role: "assistant", created_at: metadata.final_answer_created_at, content: [{ type: "text", text: finalAnswer }] },
    ], { task_runs: [metadata] });
    const restored = render(<App><ChatMessages /></App>).container;
    expect(restored.querySelector(".task-process .ant-collapse-header")?.getAttribute("aria-expanded")).toBe("false");
    expect(restored.querySelectorAll("h2")).toHaveLength(2);
    expect(restored.querySelector("h2")?.closest("[hidden]")).toBeNull();
  });
});
