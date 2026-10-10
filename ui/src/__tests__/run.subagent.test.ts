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
    // 恢复路径的完整出参回填（[docs/session-restore-fidelity]）：默认可空，具体用例用 mockResolvedValueOnce 注入
    loadToolOutcomes: vi.fn(async (): Promise<any[]> => []),
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

  // 恢复路径曾不读 ended / steps_used → 重开历史会话时所有子代理卡恒显绿勾（撞顶、提前退出都看不出来）。
  it("恢复路径回填 ended / step：出参带收尾形态时不再恒显绿勾", () => {
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [
          { type: "tool_use", id: "call-p", name: "subagent", args: { task: "T", role: "explore", maxSteps: 60 } },
          { type: "tool_use", id: "call-r", name: "subagent", args: { task: "T2", role: "explore", maxSteps: 60 } },
          { type: "tool_use", id: "call-n", name: "subagent", args: { task: "T3", role: "explore", maxSteps: 60 } },
        ],
      },
      {
        role: "tool",
        content: [
          // partial = 跑满预算才交汇报：恢复后必须原样保留，卡片据此显橙警示
          { type: "tool_result", tool_use_id: "call-p", content: JSON.stringify({ sub_id: "sub_p", steps_budget: 60, steps_used: 60, ended: "partial", report: "半成品" }), is_error: false },
          { type: "tool_result", tool_use_id: "call-r", content: JSON.stringify({ sub_id: "sub_r", steps_budget: 60, steps_used: 12, ended: "report", report: "完成" }), is_error: false },
          // 旧会话出参不带 ended / steps_used → 保持缺省 / 0（绿勾，向后兼容不得破坏）
          { type: "tool_result", tool_use_id: "call-n", content: JSON.stringify({ sub_id: "sub_n", report: "老数据" }), is_error: false },
        ],
      },
    ] as any);
    const subs = tabOf(session).subs;
    const p = subs.find((x) => x.subId === "sub_p")!;
    expect(p.ended).toBe("partial");
    expect(p.step).toBe(60);
    expect(p.report).toBe("半成品");
    const r = subs.find((x) => x.subId === "sub_r")!;
    expect(r.ended).toBe("report");
    expect(r.step).toBe(12);
    // 向后兼容：更早的会话出参无收尾字段 → ended 缺省、step 保持 0
    const n = subs.find((x) => x.subId === "sub_n")!;
    expect(n.ended).toBeUndefined();
    expect(n.step).toBe(0);
  });

  it("恢复路径：未知 ended 值 / 非法 steps_used 不写入（不得强转放行）", () => {
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-x", name: "subagent", args: { task: "T", role: "explore", maxSteps: 10 } }],
      },
      {
        role: "tool",
        content: [
          { type: "tool_result", tool_use_id: "call-x", content: JSON.stringify({ sub_id: "sub_x", ended: "brand_new", steps_used: "12" }), is_error: false },
        ],
      },
    ] as any);
    const sv = tabOf(session).subs[0];
    // 白名单校验：未知值当缺省（绿勾），步数只认 number
    expect(sv.ended).toBeUndefined();
    expect(sv.step).toBe(0);
  });

  // sidecar 回填（历史出参被截断 → 合成 restored key）走 renameRestoredSubs，共享同一套回填函数
  it("sidecar 回填路径：改名同时补回 ended / step（合成 key → 真实 sub_id）", async () => {
    const { ipc } = await import("../ipc/client");
    (ipc.loadToolOutcomes as any).mockResolvedValueOnce([
      {
        call_id: "call-cut",
        outcome: { ok: true, data: { sub_id: "sub_cut", steps_used: 60, ended: "budget", report: "半截报告" } },
      },
    ]);
    useRun.getState().restoreFromMessages(session, [
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "call-cut", name: "subagent", args: { task: "T", role: "explore", maxSteps: 60 } }],
      },
      {
        role: "tool",
        // 出参被截断：既无 sub_id 也无 ended（白名单校验路径的前提）
        content: [{ type: "tool_result", tool_use_id: "call-cut", content: `{"sub_id":"sub_cu`, is_error: false }],
      },
    ] as any);
    expect(tabOf(session).subs[0].subId).toBe("restored:call-cut");
    // backfillToolOutcomes 是异步 IPC（.then），推进微任务队列后生效
    await vi.waitFor(() => {
      const sv = tabOf(session).subs.find((x) => x.subId === "sub_cut");
      expect(sv).toBeTruthy();
      expect(sv!.ended).toBe("budget");
      expect(sv!.step).toBe(60);
      expect(sv!.report).toBe("半截报告");
    });
    expect((tabOf(session).items[0] as any).timeline[0].subId).toBe("sub_cut");
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

  // ===== [docs/subagent-terminal-event-loss](../../../docs/subagent-terminal-event-loss.md) =====
  // 子代理终态事件一旦丢失，卡片永久转圈且 composer 计数不归零。本组钉死兜底与次生修复。

  describe("子代理卡收尾兜底（subagent-terminal-event-loss）", () => {
    function seedRunningSub(subId: string) {
      useRun.setState((s) => {
        const t = s.tabs[session]!;
        t.subs.push({
          subId, role: "explore", name: null, description: "d",
          step: 3, maxSteps: 25, tokens: 100, lastTools: [],
          status: "running",
        });
        t.subStreams[subId] = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
      });
    }

    it("run:done 兜底：残留 running 的子代理卡收敛为 done（终态事件丢了也不永久转圈）", () => {
      const h = handlers();
      seedRunningSub("sub_lost");
      expect(tabOf(session).subs[0].status).toBe("running");
      // 终态事件从未到达（模拟丢帧）：直接投喂主 run 收尾
      h["run:done"]({ session, run_id: "r1" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      expect(tabOf(session).subStreams.sub_lost.status).toBe("done");
      // 兜底标记为「未按约定汇报」而非伪装成干净完成（卡片按橙色警示口径展示）
      expect(sub.ended).toBe("no_report");
    });

    it("run:done 兜底幂等：已终态的子代理不被改写（不污染正常 sub:done 路径）", () => {
      const h = handlers();
      h["sub:spawn"]({ session, sub_id: "sub_ok", role: "explore", description: "d", max_steps: 5 });
      h["sub:done"]({ session, sub_id: "sub_ok", steps_used: 4, ended: "report" });
      h["run:done"]({ session, run_id: "r2" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      // 正常路径的 ended 不被兜底覆写
      expect(sub.ended).toBe("report");
      expect(sub.step).toBe(4);
    });

    it("run:error 与 run:cancelled 同样收敛残留 running 的子代理卡", () => {
      const h = handlers();
      seedRunningSub("sub_e");
      h["run:error"]({ session, error: "boom" });
      expect(tabOf(session).subs[0].status).toBe("done");

      useRun.setState((s) => {
        const t = s.tabs[session]!;
        t.running = true;
        t.subs.push({
          subId: "sub_c", role: "explore", name: null, description: "d",
          step: 1, maxSteps: 25, tokens: 0, lastTools: [], status: "running",
        });
        t.subStreams.sub_c = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
      });
      h["run:cancelled"]({ session });
      expect(tabOf(session).subs.find((s) => s.subId === "sub_c")!.status).toBe("done");
    });

    it("sub:step 次生：已收尾的子代理不再被迟到 tick 覆写步数", () => {
      const h = handlers();
      h["sub:spawn"]({ session, sub_id: "sub_s", role: "explore", description: "d", max_steps: 40 });
      h["sub:done"]({ session, sub_id: "sub_s", steps_used: 34, ended: "report" });
      expect(tabOf(session).subs[0].step).toBe(34);
      // 后端 sub:done 先于 progress.abort() 发射，窗口内的迟到 tick 不得覆写最终步数
      h["sub:step"]({ session, sub_id: "sub_s", step: 12, tool: "read" });
      expect(tabOf(session).subs[0].step).toBe(34);
    });

    it("sub:step 仍正常更新 running 状态子代理的步数（守卫不误伤）", () => {
      const h = handlers();
      h["sub:spawn"]({ session, sub_id: "sub_r", role: "explore", description: "d", max_steps: 40 });
      h["sub:step"]({ session, sub_id: "sub_r", step: 7, tool: "grep", detail: "d1" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("running");
      expect(sub.step).toBe(7);
      expect(sub.detail).toBe("d1");
      expect(sub.lastTools).toEqual(["grep"]);
    });
  });

  // ===== [docs/subagent-terminal-event-loss](../../../docs/subagent-terminal-event-loss.md)：tool:result 二道兜底 =====
  // run 收尾兜底只在主 run 结束时收敛；「子代理早已返回、主 run 还在跑」的那段窗口里卡片照样一直转圈
  // （单个 run 实测可达数分钟）。tool:result 与 sub:done 出自同一个 task 的相邻位置，是现存第二可靠的收尾信号。

  describe("子代理终态二道兜底 · tool:result（subagent-terminal-event-loss）", () => {
    function seedRunningSub(subId: string) {
      useRun.setState((s) => {
        const t = s.tabs[session]!;
        t.subs.push({
          subId, role: "explore", name: null, description: "d",
          step: 3, maxSteps: 25, tokens: 100, lastTools: [], status: "running",
        });
        t.subStreams[subId] = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
      });
    }

    /** 投喂一条 subagent 工具结果（后端 ToolOutcome::ok 的 data 首键即 sub_id，见 subagent.rs） */
    function subagentResult(ok: boolean, data: any, tool = "subagent") {
      useRun.getState().onToolResult(
        session,
        {
          session, run_id: "r", batch_id: "b", call_index: 0, call_key: "b:0",
          tool, args_preview: "", outcome: { ok, data }, duration_ms: 1200,
        } as any,
        ok,
      );
    }

    it("sub:done 丢失时按 outcome 的 sub_id 就地收敛，并回填 ended / steps_used / report", () => {
      seedRunningSub("sub_tr");
      expect(tabOf(session).subs[0].status).toBe("running");
      // 终态三帧（report / usage / done）全部未到达，只剩 tool:result 到达
      subagentResult(true, { sub_id: "sub_tr", role: "explore", steps_budget: 25, steps_used: 18, ended: "report", report: "子代理结论" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      expect(sub.ended).toBe("report");
      expect(sub.step).toBe(18);
      expect(sub.report).toBe("子代理结论");
      expect(tabOf(session).subStreams.sub_tr.status).toBe("done");
    });

    it("幂等：sub:done 已收尾的卡不被兜底改写（不覆盖正常路径带回的终态字段）", () => {
      const h = handlers();
      h["sub:spawn"]({ session, sub_id: "sub_idem", role: "explore", description: "d", max_steps: 40 });
      h["sub:report"]({ session, sub_id: "sub_idem", report: "事件带的报告" });
      h["sub:done"]({ session, sub_id: "sub_idem", steps_used: 34, ended: "report" });
      subagentResult(true, { sub_id: "sub_idem", steps_used: 99, ended: "budget", report: "兜底报告" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      expect(sub.ended).toBe("report");
      expect(sub.step).toBe(34);
      expect(sub.report).toBe("事件带的报告");
    });

    it("守卫不误伤：非 subagent 工具的结果即使带 sub_id 也不改动子代理状态", () => {
      seedRunningSub("sub_x");
      subagentResult(true, { sub_id: "sub_x" }, "read");
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("running");
      expect(sub.ended).toBeUndefined();
    });

    it("失败路径不由兜底收敛：outcome 不带 sub_id 时保持 running（仍交 sub:error / run 收尾兜底）", () => {
      seedRunningSub("sub_fail");
      subagentResult(false, null);
      expect(tabOf(session).subs[0].status).toBe("running");
    });

    it("outcome 缺 ended 时保守落 no_report，不伪装成干净完成", () => {
      seedRunningSub("sub_noended");
      subagentResult(true, { sub_id: "sub_noended" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      expect(sub.ended).toBe("no_report");
    });

    // 钉死「ended 不按枚举白名单过滤」：白名单写法在后端扩展 ended 时会把新值静默漏判成
    // no_report —— docs/subagent-budget-and-ended.md 就 `partial` 明确警告过这一点。
    // 本分支的 SubView["ended"] 仍是三值（PR #126 未合），partial 走 as-any 载荷进，
    // 断言的是**运行时行为**：值原样保留，不被改写。
    it("ended 原样保留后端送来的值，不按枚举白名单过滤（后端新增 ended 时不静默漏判）", () => {
      seedRunningSub("sub_partial");
      subagentResult(true, { sub_id: "sub_partial", steps_used: 60, ended: "partial" });
      const sub = tabOf(session).subs[0];
      expect(sub.status).toBe("done");
      expect(sub.ended).toBe("partial");
      expect(sub.step).toBe(60);
    });

    it("ended 非字符串时落 no_report（脏值不被放行）", () => {
      seedRunningSub("sub_dirty");
      subagentResult(true, { sub_id: "sub_dirty", ended: 42 });
      expect(tabOf(session).subs[0].ended).toBe("no_report");
    });
  });
});
