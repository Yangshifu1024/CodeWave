// run 终态兜底链（[docs/run-terminal-event-fallback](../../../docs/run-terminal-event-fallback.md)）store 级测试。
//
// 覆盖四件事：① 共享收敛体 settleRun（含新增的 t.ask 一格）② run:done 幂等早退时仍收敛
// ③ reconcileRun 双向对账 ④ hasUnsettledRunState / hasRunningTools 的判据（看门狗的正确性依赖它们）。
import { beforeEach, describe, expect, it, vi } from "vitest";
import { blank, settleRunningTools } from "../stores/runFrames";
import { hasUnsettledRunState, settleRun } from "../stores/runHandlers";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { SessionRunSnapshot } from "../ipc/types";

const sessionRunning = vi.fn(async () => false);
const sessionRunSnapshot = vi.fn(async (): Promise<SessionRunSnapshot> => ({ running: false, run_id: null, terminal: null }));
const startChat = vi.fn(async () => "run-1");

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    startChat: (...args: any[]) => startChat(...(args as [])),
    sessionRunSnapshot: (...args: any[]) => sessionRunSnapshot(...(args as [])),
    gitStatus: vi.fn(async () => ({ repo: false, entries: [] })),
    loadToolOutcomes: vi.fn(async (): Promise<any[]> => []),
    loadSubagentHistory: vi.fn(async () => []),
    sessionRunning: (...args: any[]) => sessionRunning(...(args as [])),
  },
}));

const handlers = () => useRun.getState().bindGlobalHandlers();

/** 建一个空桶 + 若干「未收敛」痕迹：running 的 ask、在途工具卡、running 的子代理卡 */
function seedStuck(over: { running?: boolean } = {}) {
  useRun.setState((s) => {
    const t = blank();
    t.running = over.running ?? true;
    t.ask = {
      askId: "ask-1",
      kind: "approval",
      title: "批准？",
      detail: "",
      questions: [],
      switchToAutoEdit: false,
      allowAlways: false,
      planFile: null,
      planBody: null,
      approval: null,
      approveId: null,
    } as any;
    t.items.push({
      kind: "assistant",
      timeline: [],
      toolsMap: { "c1": { callKey: "c1", tool: "bash", status: "running", progressTail: "" } },
      streaming: true,
    } as any);
    t.subs.push({
      subId: "sub_1",
      role: "explore",
      name: "explore",
      description: "d",
      step: 3,
      maxSteps: 25,
      tokens: 100,
      lastTools: [],
      status: "running",
    });
    t.subStreams.sub_1 = { timeline: [], toolsMap: {}, status: "running", gen: 0, loaded: false };
    s.tabs[SESSION] = t;
  });
  useSessions.setState({ activeKey: SESSION });
}

const SESSION = "s-term";

function tabOf() {
  return useRun.getState().tabs[SESSION]!;
}

/** settleRun 是「对 immer 草稿原地变异」的函数，必须经 set() 对 draft 调用；
 *  直接对 getState() 的冻结快照调用会抛 read only property。 */
function settleNow() {
  useRun.setState((s) => {
    const t = s.tabs[SESSION];
    if (t) settleRun(t);
  });
}

describe("run 终态兜底链（docs/run-terminal-event-fallback）", () => {
  beforeEach(() => {
    sessionRunning.mockReset();
    sessionRunning.mockResolvedValue(false);
    sessionRunSnapshot.mockReset();
    sessionRunSnapshot.mockImplementation(async () => ({ running: await sessionRunning(), run_id: null, terminal: null }));
    startChat.mockReset();
    startChat.mockResolvedValue("run-1");
    useRun.setState({ tabs: {} });
  });

  describe("① 共享收敛体 settleRun", () => {
    it("一次收敛四类未收敛状态：ask / 在途工具卡 / 流式项 / running 子代理卡", () => {
      seedStuck();
      handlers()["run:done"]({ session: SESSION, run_id: "r1" });
      const t = tabOf();
      expect(t.running).toBe(false);
      expect(t.ask).toBeNull(); // 此前只有 ask:closed 一个清理入口 → 丢失即输入锁死
      expect(t.items[0]).toMatchObject({ streaming: false });
      expect((t.items[0] as any).toolsMap.c1.status).toBe("error"); // 落「已中断」
      expect(t.subs[0].status).toBe("done");
      expect(t.subs[0].ended).toBe("no_report");
      expect(t.subStreams.sub_1.status).toBe("done");
    });

    it("run:error 与 run:cancelled 走同一收敛体（ask 同样被清）", () => {
      for (const key of ["run:error", "run:cancelled"] as const) {
        useRun.setState({ tabs: {} });
        seedStuck();
        handlers()[key]({ session: SESSION, error: "boom" });
        expect(tabOf().ask, key).toBeNull();
        expect(tabOf().subs[0].status, key).toBe("done");
      }
    });

    it("幂等：对已收尾的桶重复调用无副作用", () => {
      seedStuck();
      settleNow();
      const snap = JSON.stringify(tabOf());
      settleNow();
      expect(JSON.stringify(tabOf())).toBe(snap);
    });
  });

  describe("② run:done 幂等早退时仍收敛（证据 5 的修复）", () => {
    it("run:start 丢失（t.running 恒 false）时，迟到的 done 不再跳过收敛", () => {
      // 关键前提：桶建出来时就是 running=false（run:start 丢帧），但子代理卡已由 sub:spawn 建好
      seedStuck({ running: false });
      expect(tabOf().running).toBe(false);
      expect(tabOf().subs[0].status).toBe("running");

      // 旧实现在这里 `if (!before?.running) return` 直接返回 → 子代理卡永久转圈
      handlers()["run:done"]({ session: SESSION, run_id: "r1" });

      expect(tabOf().subs[0].status).toBe("done");
      expect(tabOf().ask).toBeNull();
      expect((tabOf().items[0] as any).toolsMap.c1.status).toBe("error");
    });

    it("桶已完全收敛时早退仍是纯 no-op（不产生多余变更）", () => {
      seedStuck();
      settleNow();
      const snap = JSON.stringify(tabOf());
      handlers()["run:done"]({ session: SESSION, run_id: "r1" });
      expect(JSON.stringify(tabOf())).toBe(snap);
    });

    it("桶已删（Tab 已关）时早退不抛错——before 为 undefined 必须短路", () => {
      // 回归：早退分支直接 `hasUnsettledRunState(before)` 而漏判 undefined 会在
      // 「迟到 done 且 Tab 已关」时抛 TypeError，把整条 run:done 处理打断。
      // （该路径由 projectnav.row-states 的未读点用例间接触发——它断言迟到 done 仍要标未读。）
      seedStuck();
      useRun.getState().dispose(SESSION);
      expect(() => handlers()["run:done"]({ session: SESSION, run_id: "r1" })).not.toThrow();
      expect(useRun.getState().tabs[SESSION]).toBeUndefined();
    });

    it("lastDoneRunId 去重那道守卫不加收敛：新 run 已乐观置回 running，早退会误杀它", () => {
      seedStuck();
      handlers()["run:done"]({ session: SESSION, run_id: "r1" });
      // done1 之后立刻起了新 run（队列自动续跑：running 被乐观置回 true）
      useRun.setState((s) => {
        const t = s.tabs[SESSION]!;
        t.running = true;
        t.subs[0].status = "running";
        t.lastDoneRunId = "r1";
      });
      // 重复的 done2（同一 run_id）必须被守卫拦下，且**不得**收敛掉正在跑的新 run
      handlers()["run:done"]({ session: SESSION, run_id: "r1" });
      expect(tabOf().running).toBe(true);
      expect(tabOf().subs[0].status).toBe("running");
    });
  });

  describe("③ reconcileRun 双向对账", () => {
    it("A/B收尾后idle迟到Astart不得改身份，Bidle快照保持已收敛", async () => {
      seedStuck();
      useRun.setState((s) => { s.tabs[SESSION].runId = "A"; });
      handlers()["run:done"]({ session: SESSION, run_id: "A" });
      await useRun.getState().send("B任务", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "run-1" });
      sessionRunSnapshot.mockResolvedValue({ running: false, run_id: "run-1", terminal: { run_id: "run-1", kind: "done" } });
      handlers()["run:start"]({ session: SESSION, run_id: "A" });
      await useRun.getState().reconcileRun(SESSION);
      expect(tabOf().running).toBe(false);
      expect(tabOf().runId).toBe("run-1");
    });

    it("idle期间不同身份的真实外部run由权威running快照认领", async () => {
      seedStuck();
      useRun.setState((s) => { s.tabs[SESSION].runId = "old"; });
      handlers()["run:done"]({ session: SESSION, run_id: "old" });
      sessionRunSnapshot.mockResolvedValue({ running: true, run_id: "external", terminal: null });
      handlers()["run:start"]({ session: SESSION, run_id: "external" });
      await Promise.resolve();
      await Promise.resolve();
      expect(sessionRunSnapshot).toHaveBeenCalled();
      expect(tabOf().running).toBe(true);
      expect(tabOf().runId).toBe("external");
    });
    it.each(["start", "error", "cancelled"])("A/B 已收尾后 C 回执未到，迟到 A 的 %s 不得劫持 C", async (kind) => {
      seedStuck();
      useRun.setState((s) => { s.tabs[SESSION].runId = "A"; });
      handlers()["run:done"]({ session: SESSION, run_id: "A" });
      await useRun.getState().send("B任务", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "run-1" });
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("C任务", [], SESSION);
      const epoch = tabOf().runEpoch;
      handlers()[`run:${kind}`]({ session: SESSION, run_id: "A", error: "old error" });
      expect(tabOf().running).toBe(true);
      expect(tabOf().startPending).toBe(true);
      expect(tabOf().runId).toBeNull();
      expect(tabOf().runEpoch).toBe(epoch);
      started("C");
      await send;
      expect(tabOf().running).toBe(true);
      expect(tabOf().runId).toBe("C");
    });

    it.each(["done", "error", "cancelled"])("当前 %s 早于 startChat 回执，身份确认后仍处理终态", async (kind) => {
      seedStuck();
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("快速任务", [], SESSION);
      handlers()["run:start"]({ session: SESSION, run_id: "current" });
      handlers()[`run:${kind}`]({ session: SESSION, run_id: "current", error: "current error" });
      started("current");
      await send;
      expect(tabOf().running).toBe(false);
      expect(tabOf().runId).toBe("current");
      expect(tabOf().startPending).toBe(false);
    });

    it("启动终态暂存最多16身份，超界淘汰当前终态后由snapshot补偿", async () => {
      seedStuck();
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("快速任务", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "current" });
      for (let n = 0; n < 20; n++) {
        handlers()["run:error"]({ session: SESSION, run_id: `old-${n}`, error: "old error" });
      }
      expect(Object.keys(tabOf().pendingRunTerminals ?? {})).toHaveLength(16);
      expect(tabOf().running).toBe(true);
      sessionRunSnapshot.mockResolvedValue({ running: false, run_id: "current", terminal: { run_id: "current", kind: "done" } });
      started("current");
      await send;
      expect(sessionRunSnapshot).toHaveBeenCalledTimes(1);
      expect(tabOf().running).toBe(false);
      expect(tabOf().lastDoneRunId).toBe("current");
      expect(tabOf().pendingRunTerminals).toBeUndefined();
    });

    it("关闭重开后旧启动回执与暂存终态不得写新桶", async () => {
      seedStuck();
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("旧任务", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "old" });
      useRun.getState().dispose(SESSION);
      seedStuck();
      const before = JSON.stringify(tabOf());
      started("old");
      await send;
      expect(JSON.stringify(tabOf())).toBe(before);
    });

    it("自动续队已开始后旧空闲查询返回，不得收掉新 ask", async () => {
      seedStuck();
      let reply!: (v: SessionRunSnapshot) => void;
      sessionRunSnapshot.mockImplementationOnce(() => new Promise((r) => { reply = r; }));
      const stale = useRun.getState().reconcileRun(SESSION);
      await useRun.getState().send("队列下一轮", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "old" });
      useRun.setState((s) => { s.tabs[SESSION].ask = { askId: "new-ask", kind: "ask" }; });
      reply({ running: false, run_id: "old", terminal: { run_id: "old", kind: "done" } });
      await stale;
      expect(tabOf().running).toBe(true);
      expect(tabOf().ask?.askId).toBe("new-ask");
    });

    it("收尾事件先于启动回执：回执后正常续队，旧 channel 不得写入下一轮", async () => {
      seedStuck();
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const oldSend = useRun.getState().send("快速旧任务", [], SESSION);
      const channel = (startChat.mock.calls as any)[0][3];
      handlers()["run:start"]({ session: SESSION, run_id: "old" });
      await useRun.getState().send("下一轮", [], SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "old" });
      started("old");
      await oldSend;
      await Promise.resolve();
      const before = JSON.stringify(tabOf());
      channel.onmessage({ type: "delta_text", text: "旧半帧" });
      expect(JSON.stringify(tabOf())).toBe(before);
      expect(tabOf().runId).toBe("run-1");
    });

    it("已在运行新 run，旧 start/error/cancelled 都不得改动", () => {
      seedStuck();
      useRun.setState((s) => { s.tabs[SESSION].runId = "new"; });
      const before = JSON.stringify(tabOf());
      for (const kind of ["start", "error", "cancelled", "done"]) {
        handlers()[`run:${kind}`]({ session: SESSION, run_id: "old", error: "old error" });
      }
      expect(JSON.stringify(tabOf())).toBe(before);
    });

    it("空闲快照身份不符，不得收尾或续队当前 run", async () => {
      seedStuck();
      useRun.setState((s) => {
        s.tabs[SESSION].runId = "new";
        s.tabs[SESSION].queue = [{ id: "q", text: "keep" }];
      });
      sessionRunSnapshot.mockResolvedValue({ running: false, run_id: "old", terminal: { run_id: "old", kind: "done" } });
      await useRun.getState().reconcileRun(SESSION);
      expect(tabOf().running).toBe(true);
      expect(startChat).not.toHaveBeenCalled();
    });
    it("查询旧空闲状态后 send 尚未返回时不得结束乐观启动的新 run", async () => {
      seedStuck();
      let reply!: (v: SessionRunSnapshot) => void;
      sessionRunSnapshot.mockImplementationOnce(() => new Promise((r) => { reply = r; }));
      const stale = useRun.getState().reconcileRun(SESSION);
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("下一轮", [], SESSION);
      reply({ running: false, run_id: "old", terminal: null });
      await stale;
      expect(tabOf().running).toBe(true);
      started("new");
      await send;
    });

    it("查询开始时已在等待 startChat 的回执，空闲快照也不得收尾", async () => {
      seedStuck();
      settleNow();
      let started!: (v: string) => void;
      startChat.mockImplementationOnce(() => new Promise((r) => { started = r; }));
      const send = useRun.getState().send("新任务", [], SESSION);
      await useRun.getState().reconcileRun(SESSION);
      expect(tabOf().running).toBe(true);
      started("new");
      await send;
    });

    it("查询旧 running 后收到 done，不得由旧 true 响应复活", async () => {
      seedStuck();
      let reply!: (v: SessionRunSnapshot) => void;
      sessionRunSnapshot.mockImplementationOnce(() => new Promise((r) => { reply = r; }));
      const stale = useRun.getState().reconcileRun(SESSION);
      handlers()["run:done"]({ session: SESSION, run_id: "old" });
      reply({ running: true, run_id: "old", terminal: null });
      await stale;
      expect(tabOf().running).toBe(false);
    });

    it("关闭重开同会话 id，不得套用关闭前的响应", async () => {
      seedStuck();
      let reply!: (v: SessionRunSnapshot) => void;
      sessionRunSnapshot.mockImplementationOnce(() => new Promise((r) => { reply = r; }));
      const stale = useRun.getState().reconcileRun(SESSION);
      useRun.getState().dispose(SESSION);
      seedStuck();
      reply({ running: false, run_id: "old", terminal: null });
      await stale;
      expect(tabOf().running).toBe(true);
    });

    it("只有当前 run 的 authoritative done 可以续队，重复查询只消费一次", async () => {
      seedStuck();
      useRun.setState((s) => {
        s.tabs[SESSION].runId = "old";
        s.tabs[SESSION].queue = [{ id: "next", text: "续队" }, { id: "later", text: "后续" }];
      });
      sessionRunSnapshot.mockResolvedValue({ running: false, run_id: "old", terminal: { run_id: "old", kind: "done" } });
      await Promise.all([useRun.getState().reconcileRun(SESSION), useRun.getState().reconcileRun(SESSION)]);
      expect(startChat).toHaveBeenCalledTimes(1);
      expect(tabOf().queue.map((q) => q.id)).toEqual(["later"]);
      expect(tabOf().running).toBe(true);
    });

    it.each(["error", "cancelled"] as const)("权威 %s 收尾保持队列暂停且清理本轮标记", async (kind) => {
      seedStuck();
      useRun.setState((s) => {
        const t = s.tabs[SESSION];
        t.runId = "old";
        t.queue = [{ id: "next", text: "续队" }];
        t.textRecovered = "<ask>";
        t.widgetAutoOpen = { armed: true };
      });
      sessionRunSnapshot.mockResolvedValue({ running: false, run_id: "old", terminal: { run_id: "old", kind } });
      await useRun.getState().reconcileRun(SESSION);
      expect(startChat).not.toHaveBeenCalled();
      expect(tabOf().queue).toHaveLength(1);
      expect(tabOf().textRecovered).toBeNull();
      expect(tabOf().widgetAutoOpen).toBeNull();
    });
    it("后端已空闲 → 收敛（原先只有 true 方向，陈旧的 running 永远不被纠正）", async () => {
      seedStuck({ running: false });
      sessionRunning.mockResolvedValue(false);
      await useRun.getState().reconcileRun(SESSION);
      const t = tabOf();
      expect(t.subs[0].status).toBe("done");
      expect(t.ask).toBeNull();
      expect(t.running).toBe(false);
    });

    it("后端仍在跑 → 不收敛，并把本地漏掉的 running 补上（run:start 丢帧）", async () => {
      seedStuck({ running: false });
      sessionRunning.mockResolvedValue(true);
      await useRun.getState().reconcileRun(SESSION);
      expect(tabOf().running).toBe(true);
      expect(tabOf().subs[0].status).toBe("running"); // 绝不能误杀在跑的子代理
    });

    it("IPC 失败 → 维持现状，绝不因探测失败反推成已结束", async () => {
      seedStuck();
      sessionRunning.mockRejectedValue(new Error("ipc down"));
      await useRun.getState().reconcileRun(SESSION);
      expect(tabOf().running).toBe(true);
      expect(tabOf().subs[0].status).toBe("running");
    });

    it("本地已收敛时对账不产生任何多余变更（幂等）", async () => {
      seedStuck();
      settleNow();
      const snap = JSON.stringify(tabOf());
      sessionRunning.mockResolvedValue(false);
      await useRun.getState().reconcileRun(SESSION);
      expect(JSON.stringify(tabOf())).toBe(snap);
    });
  });

  describe("④ 看门狗判据", () => {
    it("hasUnsettledRunState 不只看 t.running：run:start 丢失但子代理卡在跑也算", () => {
      // 只按 t.running 判会让看门狗在这种场景下根本不启动 —— 这正是判据必须放宽的原因
      seedStuck({ running: false });
      expect(hasUnsettledRunState(tabOf() as any)).toBe(true);
    });

    it("ask 残留也算未收敛（哪怕 running 已是 false）", () => {
      seedStuck({ running: false });
      useRun.setState((s) => {
        const t = s.tabs[SESSION]!;
        t.subs[0].status = "done";
        t.items = [];
      });
      expect(hasUnsettledRunState(tabOf() as any)).toBe(true);
    });

    it("完全收敛的桶返回 false（看门狗据此不发 IPC，正常运行零开销）", () => {
      seedStuck();
      settleNow();
      expect(hasUnsettledRunState(tabOf() as any)).toBe(false);
    });

    it("hasRunningTools 与 settleRunningTools 判据同源：waiting 也算在途", () => {
      const map = {
        a: { callKey: "a", tool: "bash", status: "waiting" as const, progressTail: "" },
        b: { callKey: "b", tool: "bash", status: "ok" as const, progressTail: "" },
      };
      const t = blank();
      t.items.push({ kind: "assistant", timeline: [], toolsMap: map, streaming: false } as any);
      expect(hasUnsettledRunState(t as any)).toBe(true);
      settleRunningTools((t.items[0] as any).toolsMap);
      // waiting 被 settleRunningTools 落定后，判据必须同步转为 false（否则看门狗会永远空转）
      expect(hasUnsettledRunState(t as any)).toBe(false);
    });

    it("子流内的在途工具卡同样被扫到（扫描范围与 closeRunningTools 无 subId 分支一致）", () => {
      const t = blank();
      t.subStreams.s = {
        timeline: [],
        toolsMap: { x: { callKey: "x", tool: "bash", status: "running", progressTail: "" } },
        status: "done",
        gen: 0,
        loaded: false,
      };
      expect(hasUnsettledRunState(t as any)).toBe(true);
    });
  });
});
