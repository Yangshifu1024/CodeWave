// Run queue ([docs/run-queue-and-ask-revamp](../../../docs/run-queue-and-ask-revamp.md)): submit while running enqueues (with attachments) / run:done auto-dequeues / error pauses /
// "Run now" interrupts the current run, then executes after cancelled / edit backfills the draft
import { describe, it, expect, vi, beforeEach } from "vitest";
import { waitFor } from "@testing-library/react";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

const calls: { cmd: string; args: any }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
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
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState((s) => {
    s.tabs = {};
  });
});

describe("运行队列", () => {
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

  it("「立即」运行中打断当前任务，cancelled 后执行该条", async () => {
    seed();
    await useRun.getState().send("任务B");
    const id = useRun.getState().tabs["s1"]!.queue[0].id;
    await useRun.getState().runNow("s1", id);
    expect(calls.some((c) => c.cmd === "cancel_run")).toBe(true);
    expect(useRun.getState().tabs["s1"]!.pendingItemId).toBe(id);
    // current task cancelled → this item runs immediately (does not wait for the rest of the queue; the queue only has this item here)
    handlers()["run:cancelled"]({ session: "s1" });
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "start_chat" && c.args.text === "任务B")).toBe(true),
    );
    expect(useRun.getState().tabs["s1"]!.queue).toHaveLength(0);
    expect(useRun.getState().tabs["s1"]!.pendingItemId).toBeNull();
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
