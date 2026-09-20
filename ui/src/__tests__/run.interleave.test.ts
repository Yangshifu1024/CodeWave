import { beforeEach, describe, expect, it } from "vitest";
import { applyFrameToTab } from "../stores/runFrames";
import { useRun, type TimelineSeg } from "../stores/run";
import type { ToolView } from "../stores/run.types";

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

/** 跨所有 assistant 项收集某 call_key 的工具卡：同一次调用重复建卡时返回 >1 张 */
function toolCardsOf(session: string, callKey: string): ToolView[] {
  const items = tabOf(session).items.filter((i) => i.kind === "assistant") as any[];
  return items.map((i) => i.toolsMap[callKey]).filter(Boolean) as ToolView[];
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

  // 工具卡重复 + 卡死修复：帧/事件同键（call_key = batch:index）时必须落在同一张卡上
  it("tool:start → tool_progress → tool:result 同 key：同一张卡由 running 翻 ok，items 不增", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b7:0", tool: "command", args_preview: '{"command":"ls"}', phase: "running" });
    const before = tabOf(session).items.length;
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "tool_progress", batch: "b7", index: 0, chunk: "partial", name: "command" } as any);
    });
    handlers["tool:result"]({
      session, call_key: "b7:0", tool: "command", args_preview: '{"command":"ls"}',
      outcome: { ok: true, data: { output: "ok" } }, duration_ms: 12,
    });
    const items = tabOf(session).items.filter((i) => i.kind === "assistant");
    expect(items).toHaveLength(1);
    expect(tabOf(session).items.length).toBe(before);
    const cards = items.flatMap((i: any) => Object.values(i.toolsMap) as any[]);
    expect(cards).toHaveLength(1); // 不是两张卡
    expect(cards[0]).toMatchObject({ tool: "command", status: "ok", progressTail: "" });
  });

  it("锚点落在非当前 assistant 项（notice 插队后另建末项）：结果仍回原卡，不新建卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b7:0", tool: "read", args_preview: "{}", phase: "running" });
    // run:inject 类通知插到 items 末尾，随后 delta 帧让 currentAssistantIm 另建 assistant 项
    handlers["run:inject"]({ session, count: 1 });
    useRun.setState((s) => {
      applyFrameToTab(s.tabs[session]!, { type: "delta_text", gen: 0, text: "注入后正文" } as any);
    });
    expect(tabOf(session).items).toHaveLength(3);
    handlers["tool:result"]({
      session, call_key: "b7:0", tool: "read", args_preview: "{}",
      outcome: { ok: true, data: { files: [] } }, duration_ms: 4,
    });
    const t = tabOf(session);
    expect(t.items).toHaveLength(3); // 没多出第二张卡
    expect((t.items[0] as any).toolsMap["b7:0"]).toMatchObject({ status: "ok", progressTail: "" });
    expect(Object.keys((t.items[2] as any).toolsMap)).toEqual([]);
  });

  it("run:done 后仍 running/waiting 的工具卡落定「已中断」", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b8:0", tool: "command", args_preview: "{}", phase: "running" });
    handlers["tool:start"]({ session, call_key: "b8:1", tool: "edit", args_preview: "{}", phase: "waiting" });
    handlers["run:done"]({ session });
    const a = tabOf(session).items.find((i) => i.kind === "assistant") as any;
    for (const k of ["b8:0", "b8:1"]) {
      expect(a.toolsMap[k].status).toBe("error");
      expect(a.toolsMap[k].outcome.error.code).toBe("E_INTERRUPTED");
    }
  });

  it("run 结束后迟到的 tool:start 不建卡（幽灵转圈守卫）", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["run:done"]({ session });
    handlers["tool:start"]({ session, call_key: "b9:0", tool: "read", args_preview: "{}", phase: "running" });
    const t = tabOf(session);
    expect(t.items).toHaveLength(0);
  });

  // waiting → running 两相 + tool_progress 帧都必须跨 assistant 项命中已有卡：
  // 审批门期间 notice（run:inject / sub:error）插到 items 末尾后，currentAssistantIm 会另建 assistant 项，
  // 只查末项的实现会为同一次调用建出第二张卡（旧卡永久停在 waiting/running，直到 run 收尾才被标「已中断」）。
  it("waiting → notice 插队 → running：同一次调用仍只有一张卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b1:0", tool: "edit", args_preview: '{"files":["a.ts"]}', phase: "waiting" });
    expect(toolCardsOf(session, "b1:0")).toHaveLength(1);
    // 审批门等待期间的第三方通知插队：流式项不再是末项
    handlers["run:inject"]({ session, count: 1 });
    expect(tabOf(session).items.map((i) => i.kind)).toEqual(["assistant", "notice"]);
    // 门通过后的 running 相：必须翻原卡，不得另建项/另建卡
    handlers["tool:start"]({ session, call_key: "b1:0", tool: "edit", args_preview: "", phase: "running" });
    const cards = toolCardsOf(session, "b1:0");
    expect(cards).toHaveLength(1);
    expect(cards[0].status).toBe("running");
    expect(cards[0].tool).toBe("edit");
    expect(cards[0].argsPreview).toBe('{"files":["a.ts"]}'); // 已有值不被空值覆盖
    expect(tabOf(session).items).toHaveLength(2); // 两次相变没有多建 assistant 项
  });

  it("tool:start → notice 插队 → tool_progress：分片帧不另建卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b1:0", tool: "read", args_preview: "{}", phase: "running" });
    handlers["run:inject"]({ session, count: 2 });
    useRun.setState((s) => {
      applyFrameToTab(s.tabs[session]!, { type: "tool_progress", batch: "b1", index: 0, chunk: "hello" } as any);
    });
    const cards = toolCardsOf(session, "b1:0");
    expect(cards).toHaveLength(1);
    expect(cards[0].progressTail).toBe("hello");
    expect(cards[0].status).toBe("running");
    expect(tabOf(session).items.map((i) => i.kind)).toEqual(["assistant", "notice"]); // 帧不得新开 assistant 项
  });

  it("tool_progress 先到 → notice 插队 → tool:result 仍回原卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    useRun.setState((s) => {
      applyFrameToTab(s.tabs[session]!, { type: "tool_progress", batch: "b1", index: 0, chunk: "partial", name: "command" } as any);
    });
    handlers["run:inject"]({ session, count: 1 });
    handlers["tool:result"]({
      session, call_key: "b1:0", tool: "command", args_preview: "{}",
      outcome: { ok: true, data: { output: "ok" } }, duration_ms: 9,
    });
    const cards = toolCardsOf(session, "b1:0");
    expect(cards).toHaveLength(1);
    expect(cards[0].status).toBe("ok");
    expect(cards[0].progressTail).toBe("");
    // 结果事件同样不得新开项：仍是「帧建的流式项 + notice」
    expect(tabOf(session).items.map((i) => i.kind)).toEqual(["assistant", "notice"]);
  });

  it("waiting 相 → sub:error notice 插队 → tool_progress：进度落回原卡，不另建卡", () => {
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["tool:start"]({ session, call_key: "b3:0", tool: "edit", args_preview: "{}", phase: "waiting" });
    // 审批门期间的子代理失败通知：另一条 notice 插队路径（runFrames 不变量注释列出的三处之一）
    handlers["sub:error"]({ session, sub_id: "sub_x", error: "boom" });
    useRun.setState((s) => {
      applyFrameToTab(s.tabs[session]!, { type: "tool_progress", batch: "b3", index: 0, chunk: "hi", name: "edit" } as any);
    });
    const cards = toolCardsOf(session, "b3:0");
    expect(cards).toHaveLength(1);
    expect(cards[0].progressTail).toBe("hi");
    expect(cards[0].status).toBe("waiting"); // 相变由 tool:start 负责，进度帧只填尾部
    expect(tabOf(session).items.map((i) => i.kind)).toEqual(["assistant", "notice"]);
  });
});
