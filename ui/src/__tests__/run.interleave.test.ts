import { beforeEach, describe, expect, it } from "vitest";
import { applyFrameToTab } from "../stores/runFrames";
import { useRun, type TimelineSeg } from "../stores/run";

// Interleaving order contract ([docs/thinking-interleave-report](../../../docs/thinking-interleave-report.md)): frame arrival order = timeline display order; history restore is isomorphic; retry leaves no residue.
// applyFrameToTab is the real reducer shared by send()'s onmessage and the tests.

function tabOf(session: string) {
  return useRun.getState().tabs[session]!;
}

function timelineOf(session: string): TimelineSeg[] {
  return tabOf(session)
    .items.filter((i) => i.kind === "assistant")
    .flatMap((i) => (i.kind === "assistant" ? i.timeline : []));
}

describe("思考与工具穿插顺序", () => {
  const session = "s-interleave";

  beforeEach(() => {
    // zustand store is a module-level singleton: reset the state bucket between tests (pitfall list)
    useRun.setState((s) => {
      s.tabs[session] = {
        items: [],
        running: true,
        streamGen: 0,
        ask: null,
        breakdown: null,
        todos: [],
        suggestions: [],
        subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
        gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
  });

  it("delta_thinking/delta_text 帧按到达序穿插进 timeline", () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
    });
    // simulated frame order after backend 64ms throttling: thinking → text → thinking → text
    const frames = [
      { type: "delta_thinking", gen: 0, text: "先想一想" },
      { type: "delta_text", gen: 0, text: "结论甲" },
      { type: "delta_thinking", gen: 0, text: "再想一想" },
      { type: "delta_text", gen: 0, text: "结论乙" },
    ];
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      for (const f of frames) applyFrameToTab(t, f as any);
    });
    const tl = timelineOf(session);
    expect(tl.map((s) => s.kind)).toEqual(["thinking", "text", "thinking", "text"]);
    expect(tl[0]).toMatchObject({ text: "先想一想" });
    expect(tl[2]).toMatchObject({ text: "再想一想" });
    expect(tl[3]).toMatchObject({ text: "结论乙" });
  });

  it("思考计时：异类新段/run 收尾时收口定格，连续思考合并不重置起点", () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
    });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "思考A" } as any);
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "-续" } as any); // merged into the same segment, start time unchanged
      const seg = (t.items[0] as any).timeline[0];
      expect(seg.startedAt).toBeTypeOf("number");
      expect(seg.durationMs).toBeUndefined(); // still in progress
    });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "正文" } as any); // different-kind new segment → close out
    });
    const tl = timelineOf(session);
    expect((tl[0] as any).durationMs).toBeTypeOf("number");
    // run:done also closes segments (tail is text, no in-progress thinking segment, unaffected)
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["run:done"]({ session });
    const last = tabOf(session).items[0] as any;
    expect(last.streaming).toBe(false);
  });

  it("tool_progress 帧在当前位置落锚点，工具卡穿插在两个思考段之间", () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
    });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "step1" } as any);
      applyFrameToTab(t, { type: "tool_progress", batch: "b1", index: 0, chunk: "..." } as any);
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "step2" } as any);
    });
    const a = tabOf(session).items.find((i) => i.kind === "assistant") as any;
    expect(a.timeline.map((s: TimelineSeg) => s.kind)).toEqual(["thinking", "tool", "thinking"]);
    expect(a.toolsMap["b1:0"].progressTail).toBe("...");
    expect(a.toolsMap["b1:0"].status).toBe("running");
  });

  it("restoreFromMessages 按 content 顺序恢复 thinking/text/tool_use 穿插", () => {
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [
          { type: "thinking", text: "thinking-1" },
          { type: "text", text: "part-1" },
          { type: "tool_use", id: "call-1", name: "read", args: {} },
          { type: "thinking", text: "thinking-2" },
          { type: "text", text: "part-2" },
        ],
      },
    ] as any);
    const tl = timelineOf(session);
    expect(tl.map((s) => s.kind)).toEqual(["thinking", "text", "tool", "thinking", "text"]);
    expect((tl[2] as any).callKey).toBe("call-1");
    const a = tabOf(session).items.find((i) => i.kind === "assistant") as any;
    expect(a.toolsMap["call-1"].tool).toBe("read");
    expect(a.toolsMap["call-1"].status).toBe("ok");
  });

  it("run:retry 清空半截 timeline 无残留", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [{ kind: "thinking", text: "半截思考" }], toolsMap: {}, streaming: true });
    });
    handlers["run:retry"]({ session, attempt: 1 });
    const items = tabOf(session).items;
    const last = items[items.length - 2] as any; // the assistant item before the notice
    expect(items[items.length - 1].kind).toBe("notice");
    expect(last.timeline).toEqual([]);
    expect(last.streaming).toBe(true);
  });

  it("多步 run 每步的思考紧邻该步的工具卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    // step1: thinking → tool result
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "step1-think" } as any);
    });
    handlers["tool:result"]({
      session,
      call_key: "b1:0",
      tool: "grep",
      args_preview: "",
      outcome: { ok: true, data: {} },
      duration_ms: 3,
    });
    // step2: new assistant item (run:start semantics), thinking → tool result
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "step2-think" } as any);
    });
    handlers["tool:result"]({
      session,
      call_key: "b2:0",
      tool: "edit",
      args_preview: "",
      outcome: { ok: true, data: {} },
      duration_ms: 5,
    });
    const items = tabOf(session).items.filter((i) => i.kind === "assistant");
    expect(items).toHaveLength(2);
    expect((items[0] as any).timeline.map((s: TimelineSeg) => s.kind)).toEqual(["thinking", "tool"]);
    expect((items[1] as any).timeline.map((s: TimelineSeg) => s.kind)).toEqual(["thinking", "tool"]);
    expect((items[0] as any).toolsMap["b1:0"].tool).toBe("grep");
    expect((items[1] as any).toolsMap["b2:0"].tool).toBe("edit");
  });

  it("结果落定时清掉流式进度尾部（残留 progressTail 不进已落定卡片）", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      // 运行中：进度帧把尾部填上
      applyFrameToTab(t, { type: "tool_progress", batch: "b7", index: 0, chunk: "partial output", name: "command" } as any);
    });
    const assistant = () => tabOf(session).items.find((i) => i.kind === "assistant") as any;
    expect(assistant().toolsMap["b7:0"].progressTail).toBe("partial output");
    expect(assistant().toolsMap["b7:0"].status).toBe("running");

    handlers["tool:result"]({
      session,
      call_key: "b7:0",
      tool: "command",
      args_preview: "",
      outcome: { ok: true, data: {} },
      duration_ms: 12,
    });
    expect(assistant().toolsMap["b7:0"].status).toBe("ok");
    expect(assistant().toolsMap["b7:0"].progressTail).toBe("");
  });

  it("run:retry 后旧 attempt 的半截帧按代次丢弃（审查 C2）", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "旧半截" } as any);
    });
    // backend reset precedes the event: new generation = 1
    handlers["run:retry"]({ session, attempt: 1, gen: 1 });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "-迟到的旧帧" } as any); // should be dropped
      applyFrameToTab(t, { type: "delta_text", gen: 1, text: "新内容" } as any); // should be kept
    });
    const tl = timelineOf(session);
    expect(tl).toHaveLength(1);
    expect(tl[0]).toMatchObject({ kind: "text", text: "新内容" });
  });

  it("run 结束后的迟到帧不重建流式条目（审查 C1）", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "正文" } as any);
    });
    handlers["run:done"]({ session });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "-迟到帧" } as any);
    });
    const items = tabOf(session).items;
    expect(items).toHaveLength(1);
    expect((items[0] as any).timeline[0].text).toBe("正文");
    expect((items[0] as any).streaming).toBe(false);
  });

  it("sub:spawn 落锚点到当前流式条目 timeline（与工具卡同机制）", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "委派前文本" } as any);
    });
    handlers["sub:spawn"]({ session, sub_id: "sub_1", role: "tester", description: "d", max_steps: 10 });
    handlers["sub:step"]({ session, sub_id: "sub_1", step: 3, tool: "grep", detail: "内部摘录" });
    handlers["sub:report"]({ session, sub_id: "sub_1", report: "最终汇报" });
    handlers["sub:done"]({ session, sub_id: "sub_1" });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "委派后文本" } as any);
    });
    const a = tabOf(session).items.find((i) => i.kind === "assistant") as any;
    expect(a.timeline.map((s: TimelineSeg) => s.kind)).toEqual(["text", "sub", "text"]);
    expect(a.timeline[1].subId).toBe("sub_1");
    const sub = tabOf(session).subs.find((x) => x.subId === "sub_1");
    expect(sub).toMatchObject({ status: "done", step: 3, detail: "内部摘录", report: "最终汇报" });
  });
});
