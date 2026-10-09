// Run queue ([docs/steer-run-inject](../../../docs/steer-run-inject.md)): submit while running enqueues (with attachments) / run:done auto-dequeues / error pauses /
// "Run now" steers into the running run (no cancel_run, no interrupt) / edit backfills the draft
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import QueuePanel from "../features/chat/QueuePanel";
import { applyRetainedContent, retainTabContent, reset as resetUiState } from "../utils/uiState";

const calls: { cmd: string; args: any }[] = [];
let injectReply: (() => Promise<unknown>) | undefined;
let startReply: (() => Promise<unknown>) | undefined;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "inject_run_message" && injectReply) return injectReply();
    if (cmd === "start_chat" && startReply) return startReply();
    return null;
  }),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const prefs = { approval_mode: "auto_edit" as const, model_id: null, reasoning_effort: null };

function seed() {
  useSessions.setState({
    tabs: [{ key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs }],
    activeKey: "s1",
    projects: [],
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: true, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
      queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

function handlers() {
  return useRun.getState().bindGlobalHandlers();
}

beforeEach(() => {
  calls.length = 0;
  injectReply = undefined;
  startReply = undefined;
  resetUiState();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
});
afterEach(cleanup);

describe("运行队列", () => {
  it("新 run 启动失败后，旧 done 的迟到确认不得恢复其队列", async () => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("旧注入B");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    handlers()["run:done"]({ session: "s1", run_id: "old-run" });
    let rejectStart!: (e: Error) => void;
    startReply = () => new Promise<void>((_, r) => { rejectStart = r; });
    const manual = useRun.getState().send("手动新任务");
    await useRun.getState().send("新任务排队C");
    rejectStart(new Error("启动失败"));
    await manual;
    resolve();
    // 若回归，会新开一个尚未确认的 start_chat；不用 await 掩盖调用次数错误。
    const settled = pending;
    await Promise.resolve();
    await Promise.resolve();
    expect(calls.filter((c) => c.cmd === "start_chat")).toHaveLength(1);
    await settled;
    expect(useRun.getState().tabs.s1.queue.map((q) => q.text)).toEqual(["新任务排队C"]);
  });

  it.each(["run:cancelled", "run:error"])("%s 先于成功注入确认时，剩余队列仍暂停", async (event) => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("已消费B");
    await useRun.getState().send("待执行C");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    handlers()[event]({ session: "s1", error: "已停止" });
    resolve();
    await pending;
    expect(calls.filter((c) => c.cmd === "start_chat")).toHaveLength(0);
    expect(useRun.getState().tabs.s1.queue.map((q) => q.text)).toEqual(["待执行C"]);
  });

  it("关闭但保留队列时，注入确认仍结算驻留项，重开不会重复发送", async () => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    retainTabContent("s1");
    useRun.getState().dispose("s1");
    resolve();
    await pending;
    useRun.getState().initTab("s1");
    applyRetainedContent("s1");
    expect(useRun.getState().tabs.s1.queue).toHaveLength(0);
  });

  it("注入期间 done 只出队未占用项，失败后原条目保留且解除占用", async () => {
    seed();
    let reject!: (e: Error) => void;
    injectReply = () => new Promise<void>((_, r) => { reject = r; });
    await useRun.getState().send("任务B");
    await useRun.getState().send("任务C");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    handlers()["run:done"]({ session: "s1", run_id: "r1" });
    await waitFor(() => expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务C")).toBe(true));
    expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(false);
    reject(new Error("本 run 已收尾"));
    await pending;
    expect(useRun.getState().tabs.s1.queue.map((q) => q.text)).toEqual(["任务B"]);
    expect(useRun.getState().tabs.s1.queue[0].injecting).toBe(false);
  });

  it("done 先于注入确认时不再发同一项，成功后继续剩余队列", async () => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    handlers()["run:done"]({ session: "s1", run_id: "r1" });
    expect(calls.some((c) => c.cmd === "start_chat")).toBe(false);
    // 确认仍在途时用户继续排队；解除占用后必须有机会继续执行。
    useRun.setState((s) => { s.tabs.s1.queue.push({ id: "next", text: "任务C" }); });
    resolve();
    await pending;
    expect(calls.filter((c) => c.cmd === "start_chat").map((c) => c.args.text)).toEqual(["任务C"]);
    expect(useRun.getState().tabs.s1.queue).toHaveLength(0);
  });

  it("注入确认前不能编辑或删除同一项", async () => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    useRun.getState().editQueueItem("s1", id);
    useRun.getState().removeQueueItem("s1", id);
    expect(useRun.getState().tabs.s1.queue).toHaveLength(1);
    expect(useRun.getState().tabs.s1.draftFromQueue).toBeNull();
    resolve();
    await pending;
  });

  it("注入确认前连续点同项立即只发送一次，且按钮显示占用", async () => {
    seed();
    let resolve!: () => void;
    injectReply = () => new Promise<void>((r) => { resolve = r; });
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs.s1.queue[0].id;
    const pending = useRun.getState().runNow("s1", id);
    const repeated = useRun.getState().runNow("s1", id);
    expect(calls.filter((c) => c.cmd === "inject_run_message")).toHaveLength(1);
    render(createElement(QueuePanel));
    expect((screen.getByRole("button", { name: /立即/ }) as HTMLButtonElement).disabled).toBe(true);
    resolve();
    await Promise.all([pending, repeated]);
    expect(useRun.getState().tabs.s1.queue).toHaveLength(0);
  });

  it("运行中提交进入队列（含附件），不发起 start_chat", async () => {
    seed();
    const ok = await useRun.getState().send("任务B", [{ mime: "image/png", data: "xx" }]);
    expect(ok).toBe(true);
    const q = useRun.getState().tabs["s1"]!.queue;
    expect(q).toHaveLength(1);
    expect(q[0].text).toBe("任务B");
    expect(q[0].images?.[0].data).toBe("xx");
    expect(calls.filter((c) => c.cmd === "start_chat")).toHaveLength(0);
  });

  it("run:done 后自动出队执行下一条，附件随发", async () => {
    seed();
    await useRun.getState().send("任务B", [{ mime: "image/png", data: "xx" }]);
    handlers()["run:done"]({ session: "s1" });
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(true),
    );
    // attachments passed along with send (base64 form, same shape as the normal send path)
    const startCall = calls.find((c) => c.cmd === "start_chat");
    expect(startCall?.args?.images?.[0]?.data).toBe("xx");
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(0);
  });

  it("run:error 后队列暂停保留，不出队", async () => {
    seed();
    await useRun.getState().send("任务B");
    handlers()["run:error"]({ session: "s1", error: "boom" });
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.filter((c) => c.cmd === "start_chat")).toHaveLength(0);
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(1);
    // "Continue" manually resumes dequeuing
    await useRun.getState().runQueueNext("s1");
    await waitFor(() => expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(true));
  });

  it("「立即」steer 进正在跑的 run：不打断、不调 cancel_run，条目出队", async () => {
    seed();
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs["s1"]!.queue[0].id;
    await useRun.getState().runNow("s1", id);
    // steer 语义的核心断言：**不得**调 cancel_run（那会级联停掉在跑的子代理）
    expect(calls.some((c) => c.cmd === "cancel_run")).toBe(false);
    // 改为走 inject 通道把消息并入当前 run
    expect(calls.some((c) => c.cmd === "inject_run_message" && c.args.text === "任务B")).toBe(true);
    // 注入成功后才出队
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(0);
    // 不经 start_chat 开新 run（当前 run 继续跑）
    expect(calls.some((c) => c.cmd === "start_chat")).toBe(false);
  });

  it("「立即」steer 不重排队列：消息进后端 history，剩余条目保持原序", async () => {
    seed();
    await useRun.getState().send("任务A");
    await useRun.getState().send("任务B");
    await useRun.getState().send("任务C");
    const last = useRun.getState().tabs["s1"]!.queue[2].id;
    await useRun.getState().runNow("s1", last);
    const q = useRun.getState().tabs["s1"]!.queue;
    expect(q.map((x) => x.text)).toEqual(["任务A", "任务B"]);
  });

  it("「立即」遇带图条目：运行中不入队也不 steer，条目留在队列", async () => {
    seed();
    await useRun.getState().send("任务B", [{ mime: "image/png", data: "xx" }]);
    const id = useRun.getState().tabs["s1"]!.queue[0].id;
    await useRun.getState().runNow("s1", id);
    // inject 通道只收纯文本 → 不调
    expect(calls.some((c) => c.cmd === "inject_run_message")).toBe(false);
    // 条目留在队列里等 run:done 自然出队（不丢消息）
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(1);
    handlers()["run:done"]({ session: "s1" });
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(true),
    );
  });

  it("带图条目的「立即」按钮在运行中禁用（review 🟡-2：避免静默 no-op）", async () => {
    seed();
    await useRun.getState().send("纯文本");
    await useRun.getState().send("带图", [{ mime: "image/png", data: "xx" }]);
    render(createElement(QueuePanel));
    await waitFor(() => expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(2));
    const btns = screen.getAllByRole("button", { name: /立即/ });
    expect(btns).toHaveLength(2);
    // 第一条（纯文本）可 steer，第二条（带图）不可 → 按钮禁用
    // 不用 jest-dom 的 toBeDisabled（本项目未装该扩展），直接读原生 disabled 属性
    expect((btns[0] as HTMLButtonElement).disabled).toBe(false);
    expect((btns[1] as HTMLButtonElement).disabled).toBe(true);
  });

  it("编辑队列条目：文本与附件回填草稿并移除条目", async () => {
    seed();
    await useRun.getState().send("任务B", [{ mime: "image/png", data: "AAAA" }]);
    const id = useRun.getState().tabs["s1"]!.queue[0].id;
    useRun.getState().editQueueItem("s1", id);
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(0);
    const draft = useRun.getState().tabs["s1"]!.draftFromQueue;
    expect(draft?.text).toBe("任务B");
    expect(draft?.images).toEqual([{ mime: "image/png", data: "AAAA" }]); // editing does not lose attachments
    useRun.getState().consumeDraftFromQueue();
    expect(useRun.getState().tabs["s1"]!.draftFromQueue).toBeNull();
  });

  it("拖拽排序：reorderQueue 把条目移到目标位置（grip 修复的 store 层）", async () => {
    seed();
    await useRun.getState().send("任务B");
    await useRun.getState().send("任务C");
    await useRun.getState().send("任务D");
    const ids = useRun.getState().tabs["s1"]!.queue.map((q) => q.id);
    // drag the third item (D) to the first item's (B) position → D B C
    useRun.getState().reorderQueue("s1", ids[2], ids[0]);
    expect(useRun.getState().tabs["s1"]!.queue.map((q) => q.text)).toEqual(["任务D", "任务B", "任务C"]);
    // same id: no-op; unknown id: no-op
    useRun.getState().reorderQueue("s1", ids[0], ids[0]);
    useRun.getState().reorderQueue("s1", "ghost", ids[0]);
    expect(useRun.getState().tabs["s1"]!.queue.map((q) => q.text)).toEqual(["任务D", "任务B", "任务C"]);
  });
});

describe("run:done 双发去重（docs/run-queue-and-ask-revamp 回归）", () => {
  it("suggest 双发 done 时队列只出队一次，running 不被二次复位", async () => {
    seed();
    await useRun.getState().send("任务B");
    await useRun.getState().send("任务C");
    const h = handlers();
    // done1: real finish (with run_id) → dequeues TaskB and starts a new run (running optimistically set to true)
    h["run:done"]({ session: "s1", run_id: "r1" });
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(true),
    );
    expect(useRun.getState().tabs["s1"]!.running).toBe(true);
    // done2: duplicate done for the same run via suggest → must be blocked by the run_id dedup,
    // otherwise running would be wrongly reset and TaskC would also be dequeued (backend running guard rejects → item silently lost)
    h["run:done"]({ session: "s1", run_id: "r1" });
    await new Promise((r) => setTimeout(r, 20));
    expect(useRun.getState().tabs["s1"]!.running).toBe(true);
    expect(useRun.getState().tabs["s1"]!.queue.map((q) => q.text)).toEqual(["任务C"]);
    expect(calls.filter((c) => c.cmd === "start_chat")).toHaveLength(1);
  });
});
