// Subagent interaction batch ([docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)) store-level tests:
// envelope frame routing / first subagent auto-opens the drawer / archive preserved across runs / subagent tool result backfill / restore recognition and process history rebuild.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { applyFrameToTab } from "../stores/runFrames";
import { useRun, type TimelineSeg } from "../stores/run";
import { useSessions } from "../stores/sessions";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    startChat: vi.fn(async () => "run-1"),
    loadSubagentHistory: vi.fn(async (_sid: string, subId: string) =>
      subId === "sub_9"
        ? [
            { role: "user", content: [{ type: "text", text: "<subagent-task>任务</subagent-task>" }] },
            {
              role: "assistant",
              content: [
                { type: "thinking", text: "子代理思考" },
                { type: "tool_use", id: "t-1", name: "grep", args: { pattern: "x" } },
                { type: "text", text: "子代理结论" },
              ],
            },
            {
              role: "tool",
              content: [{ type: "tool_result", tool_use_id: "t-1", content: '{"matches":[]}', is_error: false }],
            },
          ]
        : [],
    ),
  },
}));

const handlers = () => useRun.getState().bindGlobalHandlers();

function seedTab(session: string, running = true) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [],
      running,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
  useSessions.setState({ activeKey: session });
}

function tabOf(session: string) {
  return useRun.getState().tabs[session]!;
}

describe("子代理交互（docs/subagent-interaction-drawer）", () => {
  const session = "s-sub";

  beforeEach(() => {
    // zustand store is a module-level singleton: reset the state bucket between tests (pitfall list)
    seedTab(session);
  });

  it("sub:spawn 首个子代理自动弹出抽屉并初始化消息流；多个不再切换", () => {
    const h = handlers();
    useRun.setState((s) => {
      s.tabs[session]!.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
    });
    h["sub:spawn"]({ session, sub_id: "sub_1", role: "explore", name: "explore", task: "调研任务", description: "探索代码", max_steps: 10 });
    let t = tabOf(session);
    expect(t.subDrawer).toEqual({ open: true, subId: "sub_1" });
    expect(t.subs[0]).toMatchObject({ name: "explore", task: "调研任务", status: "running" });
    expect(t.subStreams.sub_1.status).toBe("running");

    // second subagent starts: drawer keeps the current view, no interruption
    h["sub:spawn"]({ session, sub_id: "sub_2", role: "backend-dev", description: "实现", max_steps: 5 });
    t = tabOf(session);
    expect(t.subDrawer).toEqual({ open: true, subId: "sub_1" });
    expect(t.subs.filter((x) => x.status === "running")).toHaveLength(2);
  });

  it("信封帧路由进独立消息流，不污染主流；收口后迟到帧不入流", () => {
    const h = handlers();
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [], toolsMap: {}, streaming: true });
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "主流文本" } as any);
    });
    h["sub:spawn"]({ session, sub_id: "sub_1", role: "explore", description: "d", max_steps: 10 });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "sub", sub_id: "sub_1", frame: { type: "delta_text", gen: 0, text: "子代理文本" } } as any);
      applyFrameToTab(t, { type: "sub", sub_id: "sub_1", frame: { type: "delta_thinking", gen: 0, text: "子代理思考" } } as any);
    });
    let t = tabOf(session);
    // main stream has main text + spawn anchor (subagent text only goes into the subagent stream)
    const mainTl = (t.items[0] as any).timeline as TimelineSeg[];
    expect(mainTl.map((s) => s.kind)).toEqual(["text", "sub"]);
    expect(JSON.stringify(mainTl)).not.toContain("子代理文本");
    expect(t.subStreams.sub_1.timeline.map((s) => s.kind)).toEqual(["text", "thinking"]);

    h["sub:done"]({ session, sub_id: "sub_1" });
    t = tabOf(session);
    expect(t.subs[0].status).toBe("done");
    expect(t.subStreams.sub_1.status).toBe("done");
    useRun.setState((s) => {
      const tt = s.tabs[session]!;
      applyFrameToTab(tt, { type: "sub", sub_id: "sub_1", frame: { type: "delta_text", gen: 0, text: "-迟到" } } as any);
    });
    expect(tabOf(session).subStreams.sub_1.timeline).toHaveLength(2);
  });

  it("sub:done 携带 steps_used/ended：刷新最终步数并标记收尾原因；无新字段的旧 payload 仍照常收尾", () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_early", role: "frontend-dev", description: "d", max_steps: 80 });
    // 轮询采样值（22）在收尾时会滞后于真实已启动步数（37）
    h["sub:step"]({ session, sub_id: "sub_early", step: 22 });
    expect(tabOf(session).subs[0].step).toBe(22);

    h["sub:done"]({ session, sub_id: "sub_early", steps_used: 37, ended: "no_report" });
    const sub = tabOf(session).subs[0];
    expect(sub.step).toBe(37);
    expect(sub.ended).toBe("no_report");
    expect(sub.status).toBe("done");
    expect(tabOf(session).subStreams.sub_early.status).toBe("done");

    // 旧会话 / 旧后端：payload 不带新字段 → 收尾照常，ended 保持缺省（前端按正常收尾展示）
    h["sub:spawn"]({ session, sub_id: "sub_legacy", role: "explore", description: "d", max_steps: 10 });
    h["sub:done"]({ session, sub_id: "sub_legacy" });
    const legacy = tabOf(session).subs.find((x) => x.subId === "sub_legacy")!;
    expect(legacy.status).toBe("done");
    expect(legacy.ended).toBeUndefined();
  });

  it("sub:step 上报 approval_mode：写入子代理档位；旧载荷无该字段不覆盖", () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_mode", role: "backend-dev", description: "d", max_steps: 5 });
    expect(tabOf(session).subs[0].approvalMode).toBeUndefined();
    h["sub:step"]({ session, sub_id: "sub_mode", step: 2, approval_mode: "full_access" });
    expect(tabOf(session).subs[0].approvalMode).toBe("full_access");
    // 归档 / 旧后端：payload 不带 approval_mode → 保持原值（不写成 undefined，抽屉不会突然丢档位行）
    h["sub:step"]({ session, sub_id: "sub_mode", step: 3 });
    expect(tabOf(session).subs[0].approvalMode).toBe("full_access");
  });

  it("子代理流内 tool_progress 落锚点，tool:result（session=sub_id）路由回填工具卡", () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_1", role: "backend-dev", description: "d", max_steps: 10 });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "sub", sub_id: "sub_1", frame: { type: "tool_progress", batch: "b1", index: 0, chunk: "..." } } as any);
    });
    h["tool:result"]({ session: "sub_1", call_key: "b1:0", tool: "grep", args_preview: "{}", outcome: { ok: true, data: {} }, duration_ms: 3 });
    const st = tabOf(session).subStreams.sub_1;
    expect(st.timeline.map((s) => s.kind)).toEqual(["tool"]);
    // 结果落定：进度尾部一并清掉（与主会话同步）
    expect(st.toolsMap["b1:0"]).toMatchObject({ tool: "grep", status: "ok", progressTail: "" });

    // subagent file writes also bump the main session writeTick ([docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md) attribution semantics)
    const before = tabOf(session).writeTick;
    h["tool:result"]({ session: "sub_1", call_key: "b1:1", tool: "edit", args_preview: "{}", outcome: { ok: true, data: {} }, duration_ms: 3 });
    expect(tabOf(session).writeTick).toBe(before + 1);
  });

  it("归档化：新一轮 run 不再清空子代理卡与消息流（跨 run 可回看）", async () => {
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      t.items.push({ kind: "assistant", timeline: [{ kind: "sub", subId: "sub_1" }], toolsMap: {}, streaming: false });
      t.subs.push({ subId: "sub_1", role: "explore", name: "explore", description: "d", step: 3, maxSteps: 10, tokens: 100, lastTools: [], status: "done" });
      t.subStreams.sub_1 = { timeline: [{ kind: "text", text: "历史过程" }], toolsMap: {}, status: "done", gen: 0, loaded: true };
      t.running = false;
    });
    await useRun.getState().send("新任务");
    const t = tabOf(session);
    expect(t.subs).toHaveLength(1);
    expect(t.subStreams.sub_1.timeline[0]).toMatchObject({ text: "历史过程" });
    // send accepted normally (running set to true, user item pushed)
    expect(t.running).toBe(true);
    expect(t.items.some((i) => i.kind === "user")).toBe(true);
  });

  it("restoreFromMessages 识别 subagent 工具卡：锚点替换 + 归档注册", () => {
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [
          { type: "text", text: "委派调研" },
          { type: "tool_use", id: "call-9", name: "subagent", args: { task: "任务全文", role: "explore", maxSteps: 8, description: "探索代码" } },
        ],
      },
      {
        role: "tool",
        content: [
          { type: "tool_result", tool_use_id: "call-9", content: JSON.stringify({ sub_id: "sub_9", role: "explore", steps_budget: 8, report: "最终报告" }), is_error: false },
        ],
      },
    ] as any);
    const t = tabOf(session);
    const a = t.items.find((i) => i.kind === "assistant") as any;
    expect(a.timeline.map((s: TimelineSeg) => s.kind)).toEqual(["text", "sub"]);
    expect(a.timeline[1].subId).toBe("sub_9");
    expect(t.subs[0]).toMatchObject({
      subId: "sub_9", role: "explore", description: "探索代码", task: "任务全文", report: "最终报告", status: "done",
    });
    expect(t.subStreams.sub_9).toMatchObject({ status: "done", loaded: false });
  });

  it("旧会话 outcome 无 sub_id 时合成 restored key 降级", () => {
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-old", name: "subagent", args: { task: "T", role: "pm" } }],
      },
      {
        role: "tool",
        content: [{ type: "tool_result", tool_use_id: "call-old", content: "{}", is_error: false }],
      },
    ] as any);
    const t = tabOf(session);
    expect((t.items[0] as any).timeline[0].subId).toBe("restored:call-old");
    expect(t.subs[0].subId).toBe("restored:call-old");
  });

  it("openSubDrawer 按需拉取过程历史并重建消息流（运行中不拉取）", async () => {
    const { ipc } = await import("../ipc/client");
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-9", name: "subagent", args: { task: "T", role: "explore" } }],
      },
      {
        role: "tool",
        content: [{ type: "tool_result", tool_use_id: "call-9", content: JSON.stringify({ sub_id: "sub_9", report: "R" }), is_error: false }],
      },
    ] as any);

    await useRun.getState().openSubDrawer(session, "sub_9");
    expect(ipc.loadSubagentHistory).toHaveBeenCalledWith(session, "sub_9");
    const st = tabOf(session).subStreams.sub_9;
    expect(st.loaded).toBe(true);
    // message stream rebuild: text/thinking interleave + tool card backfill (user task message stays out of the stream, shown separately in the drawer header)
    expect(st.timeline.map((s) => s.kind)).toEqual(["thinking", "tool", "text"]);
    expect(st.toolsMap["t-1"]).toMatchObject({ tool: "grep", status: "ok" });

    // reopening: loaded=true, no repeated fetch
    (ipc.loadSubagentHistory as any).mockClear();
    await useRun.getState().openSubDrawer(session, "sub_9");
    expect(ipc.loadSubagentHistory).not.toHaveBeenCalled();

    // running subagent (live stream) does not fetch history
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_live", role: "backend-dev", description: "d", max_steps: 5 });
    (ipc.loadSubagentHistory as any).mockClear();
    await useRun.getState().openSubDrawer(session, "sub_live");
    expect(ipc.loadSubagentHistory).not.toHaveBeenCalled();
    expect(tabOf(session).subDrawer).toEqual({ open: true, subId: "sub_live" });
  });

  it("closeSubDrawer 只关不销毁：流数据保留可再次进入", async () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_1", role: "explore", description: "d", max_steps: 5 });
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "sub", sub_id: "sub_1", frame: { type: "delta_text", gen: 0, text: "过程" } } as any);
    });
    useRun.getState().closeSubDrawer(session);
    expect(tabOf(session).subDrawer.open).toBe(false);
    // reopening: live stream data still present
    await useRun.getState().openSubDrawer(session, "sub_1");
    expect(tabOf(session).subDrawer).toEqual({ open: true, subId: "sub_1" });
    expect(tabOf(session).subStreams.sub_1.timeline[0]).toMatchObject({ text: "过程" });
  });

  it("sub:done 只收尾该子流在途工具卡，不误伤主会话在途卡", () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_1", role: "explore", description: "d", max_steps: 5 });
    // 子流内两个在途卡（一 running 一 waiting）；session = sub_id 路由进子流
    h["tool:start"]({ session: "sub_1", call_key: "s1:0", tool: "read", args_preview: "{}", phase: "running" });
    h["tool:start"]({ session: "sub_1", call_key: "s1:1", tool: "edit", args_preview: "{}", phase: "waiting" });
    // 主会话在途卡（子代理结束时主会话可能仍在跑）
    h["tool:start"]({ session, call_key: "m1:0", tool: "command", args_preview: "{}", phase: "running" });

    h["sub:done"]({ session, sub_id: "sub_1" });

    const t = tabOf(session);
    for (const k of ["s1:0", "s1:1"]) {
      expect(t.subStreams.sub_1.toolsMap[k].status).toBe("error");
      expect(t.subStreams.sub_1.toolsMap[k].outcome.error.code).toBe("E_INTERRUPTED");
    }
    const a = t.items.find((i) => i.kind === "assistant") as any;
    expect(a.toolsMap["m1:0"].status).toBe("running"); // 主会话卡不受子代理收尾影响
  });

  it("冻结态 produce 路径：同类 delta 连帧合并 + 主流水同步无恙（P4-3 回归：排除 immer 冻结抛错致 set 回滚）", () => {
    const h = handlers();
    h["sub:spawn"]({ session, sub_id: "sub_fz", role: "explore", description: "d", max_steps: 5 });
    // first set(): simulates real frame arrival (the new state returned by produce is then frozen by immer)
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "sub", sub_id: "sub_fz", frame: { type: "delta_text", gen: 0, text: "第一段" } } as any);
    });
    // second set(): run the full reduction again on the frozen new state (last.text += text hits the frozen object)
    useRun.setState((s) => {
      const t = s.tabs[session]!;
      applyFrameToTab(t, { type: "sub", sub_id: "sub_fz", frame: { type: "delta_text", gen: 0, text: "第二段" } } as any);
      // main stream appends within the same batch, verifying the coupled rollback surface if the subagent reduction throws
      applyFrameToTab(t, { type: "delta_text", gen: 0, text: "主流" } as any);
    });
    const t = tabOf(session);
    expect(t.subStreams.sub_fz.timeline).toEqual([{ kind: "text", text: "第一段第二段" }]);
    const mainTl = (t.items[0] as any).timeline as TimelineSeg[];
    expect(mainTl.map((s) => s.kind)).toEqual(["sub", "text"]);
    expect((mainTl[1] as any).text).toBe("主流");
  });
});
