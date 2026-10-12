// 计划任务 store（任务列表单一数据源）的单测：
// 核心不变量是「读盘失败绝不当成空列表」——失败只写 error，旧 items 原地保留；
// 另外守住 runningIds 的清理由 scheduled:fired / scheduled:done 驱动、收尾后重新拉一次列表补齐 runs/next_run。
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ScheduledTask } from "../ipc/types";

// vi.mock 工厂被提升到 import 之上：mock 句柄必须先用 vi.hoisted 构造
const invokeMock = vi.hoisted(() =>
  vi.fn(async (_cmd: string, _args?: unknown): Promise<unknown> => []),
);

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { statusKind, useTasks } from "../stores/tasks";

function task(over: Partial<ScheduledTask> & { id: string }): ScheduledTask {
  return {
    name: over.id,
    instruction: "do it",
    schedule: "every:30 m",
    next_run: null,
    last_status: null,
    last_summary: null,
    project_id: null,
    enabled: true,
    runs: [],
    ...over,
  };
}

const listCall = { sessionId: "" };

beforeEach(() => {
  invokeMock.mockReset().mockResolvedValue([]);
  useTasks.setState({ items: [], loading: false, error: null, runningIds: [], runningRuns: {}, terminalRuns: [], deletedIds: [], diagnostics: null });
});

describe("tasks store · load", () => {
  it("成功：items 覆盖 + error 清空 + loading 复位（命令名与参数键名固定）", async () => {
    invokeMock.mockResolvedValueOnce([task({ id: "t1" })]);
    useTasks.setState({ items: [task({ id: "old" })], error: "上一轮的错误" });

    const p = useTasks.getState().load();
    expect(useTasks.getState().loading).toBe(true);
    await p;

    expect(useTasks.getState().items.map((t) => t.id)).toEqual(["t1"]);
    expect(useTasks.getState().error).toBeNull();
    expect(useTasks.getState().loading).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("list_scheduled_tasks", listCall);
  });

  it("失败：旧 items 保留不变、error 为错误串、loading 复位（不把失败伪装成空列表）", async () => {
    const keep = task({ id: "t1", last_status: "ok" });
    useTasks.setState({ items: [keep] });
    invokeMock.mockRejectedValueOnce(new Error("boom"));

    await useTasks.getState().load();

    expect(useTasks.getState().items).toEqual([keep]);
    expect(useTasks.getState().error).toContain("boom");
    expect(useTasks.getState().loading).toBe(false);
  });
});

describe("tasks store · 运行态事件", () => {
  it("applyFired 去重：同一 id 连续两次仍只有一项", () => {
    useTasks.getState().applyFired({ id: "t1", name: "daily" });
    useTasks.getState().applyFired({ id: "t1", name: "daily" });
    expect(useTasks.getState().runningIds).toEqual(["t1"]);

    useTasks.getState().applyFired({ id: "t2", name: "hourly" });
    expect(useTasks.getState().runningIds).toEqual(["t1", "t2"]);
  });

  it("applyFired 无 id 时不动状态（异常载荷不留幽灵运行标记）", () => {
    useTasks.getState().applyFired({ name: "daily" });
    expect(useTasks.getState().runningIds).toEqual([]);
  });

  it("applyDone：按 id 更新状态/摘要、清 running，并触发一次重新 load", async () => {
    useTasks.setState({
      items: [task({ id: "t1" }), task({ id: "t2" })],
      runningIds: ["t1", "t2"],
    });

    useTasks.getState().applyDone({ id: "t1", name: "t1", status: "error", summary: "指令失败" });

    const [t1, t2] = useTasks.getState().items;
    expect(t1).toMatchObject({ last_status: "error", last_summary: "指令失败" });
    expect(t2).toMatchObject({ last_status: null, last_summary: null }); // 未命中项不动
    expect(useTasks.getState().runningIds).toEqual(["t2"]);

    // 收尾后重新拉列表（runs/next_run 只有后端落盘后才有）
    await Promise.resolve();
    expect(invokeMock).toHaveBeenCalledWith("list_scheduled_tasks", listCall);
  });

  it("applyDone 缺 id 时按 name 匹配，并按命中项的 id 清掉运行标记", async () => {
    useTasks.setState({ items: [task({ id: "real-id", name: "daily" })], runningIds: ["real-id"] });

    useTasks.getState().applyDone({ name: "daily", status: "ok", summary: "完成 3 步" });

    expect(useTasks.getState().items[0]).toMatchObject({ last_status: "ok", last_summary: "完成 3 步" });
    expect(useTasks.getState().runningIds).toEqual([]);
    await Promise.resolve();
  });

  it("applyDone 载荷缺 status/summary 时保留旧值（不把已有状态抹成 null）", async () => {
    useTasks.setState({ items: [task({ id: "t1", last_status: "ok", last_summary: "上次成功" })] });

    useTasks.getState().applyDone({ id: "t1" });

    expect(useTasks.getState().items[0]).toMatchObject({ last_status: "ok", last_summary: "上次成功" });
    await Promise.resolve();
  });
});

describe("tasks store · 本地列表维护", () => {
  it("upsertLocal 已知 id 就地合并、未知 id 追加（不整表刷新）", () => {
    useTasks.setState({ items: [task({ id: "t1", name: "旧名" })] });

    useTasks.getState().upsertLocal(task({ id: "t1", name: "新名" }));
    expect(useTasks.getState().items).toHaveLength(1);
    expect(useTasks.getState().items[0].name).toBe("新名");

    useTasks.getState().upsertLocal(task({ id: "t2" }));
    expect(useTasks.getState().items.map((t) => t.id)).toEqual(["t1", "t2"]);
  });

  it("removeLocal 就地摘除该条", () => {
    useTasks.setState({ items: [task({ id: "t1" }), task({ id: "t2" })] });
    useTasks.getState().removeLocal("t1");
    expect(useTasks.getState().items.map((t) => t.id)).toEqual(["t2"]);
  });
});

describe("statusKind 判定表", () => {
  it("ok/skipped 中性，其余非空为 error，空值为 none", () => {
    expect(statusKind("ok")).toBe("neutral");
    expect(statusKind("skipped")).toBe("neutral");
    expect(statusKind("error")).toBe("error");
    expect(statusKind("running")).toBe("error");
    expect(statusKind("")).toBe("none");
    expect(statusKind(null)).toBe("none");
    expect(statusKind(undefined)).toBe("none");
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

function active(run_id: string) {
  return { run_id, source: "manual", started_at: "2026-10-01T00:00:00Z", scheduled_for: null };
}

describe("task lifecycle reconciliation", () => {
  it("rejects non-array data without erasing known tasks", async () => {
    const keep = task({ id: "keep" }); useTasks.setState({ items: [keep] });
    invokeMock.mockResolvedValueOnce({ tasks: [] });
    await useTasks.getState().load();
    expect(useTasks.getState().items).toEqual([keep]);
    expect(useTasks.getState().error).toContain("Invalid scheduled task list");
  });

  it("rejects a list that arrived after a newer edit or delete", async () => {
    for (const operation of ["edit", "delete"]) {
      const response = deferred<ScheduledTask[]>();
      invokeMock.mockReturnValueOnce(response.promise);
      useTasks.setState({ items: [task({ id: "t", name: "old" })] });
      const loading = useTasks.getState().load();
      if (operation === "edit") useTasks.getState().upsertLocal(task({ id: "t", name: "new" }));
      else useTasks.getState().removeLocal("t");
      response.resolve([task({ id: "t", name: "old" })]); await loading;
      expect(useTasks.getState().items.map((t) => t.name)).toEqual(operation === "edit" ? ["new"] : []);
    }
  });

  it("the latest request wins even if older requests resolve later", async () => {
    const first = deferred<ScheduledTask[]>(); const second = deferred<ScheduledTask[]>();
    invokeMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const a = useTasks.getState().load(); const b = useTasks.getState().load();
    second.resolve([task({ id: "new" })]); await b;
    first.resolve([task({ id: "old" })]); await a;
    expect(useTasks.getState().items.map((t) => t.id)).toEqual(["new"]);
  });

  it("authoritative snapshots recover missing fired and done events", async () => {
    invokeMock.mockResolvedValueOnce([task({ id: "t", active_run: active("run1") })]);
    await useTasks.getState().load();
    expect(useTasks.getState().runningIds).toEqual(["t"]);
    expect(useTasks.getState().runningRuns.t).toBe("run1");
    invokeMock.mockResolvedValueOnce([task({ id: "t", active_run: null, last_status: "ok" })]);
    await useTasks.getState().load();
    expect(useTasks.getState().runningIds).toEqual([]);
  });

  it("old or identity-less done events cannot settle a newer active instance", () => {
    useTasks.setState({ items: [task({ id: "t", active_run: active("new") })], runningIds: ["t"], runningRuns: { t: "new" } });
    useTasks.getState().applyDone({ id: "t", run_id: "old", status: "error" });
    useTasks.getState().applyDone({ id: "t", status: "ok" });
    expect(useTasks.getState().runningIds).toEqual(["t"]);
    expect(useTasks.getState().items[0].last_status).toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("a fired event invalidates an older empty snapshot", async () => {
    const response = deferred<ScheduledTask[]>(); invokeMock.mockReturnValueOnce(response.promise);
    const loading = useTasks.getState().load();
    useTasks.getState().applyFired({ id: "t", run_id: "new" });
    response.resolve([]); await loading;
    expect(useTasks.getState().runningIds).toEqual(["t"]);
  });

  it("completed-instance fired events cannot resurrect that instance", () => {
    const run = { run_id: "old", at: "", status: "ok", summary: "", source: "schedule", out_tokens: 0 };
    useTasks.setState({ items: [task({ id: "t", runs: [run] })] });
    useTasks.getState().applyFired({ id: "t", run_id: "old" });
    expect(useTasks.getState().runningIds).toEqual([]);
  });

  it("retains recovery diagnostics and unsaved results", async () => {
    const diagnostics = { state: "degraded", issues: [{ kind: "task_json", path: "/project/tasks/t.json", project_id: "p", task_id: "t", message: "Invalid JSON" }] };
    invokeMock.mockResolvedValueOnce(diagnostics);
    await useTasks.getState().loadDiagnostics();
    expect(useTasks.getState().diagnostics).toEqual(diagnostics);
    invokeMock.mockResolvedValueOnce([task({ id: "t", last_status: "ok", persistence_error: "Disk full" })]);
    await useTasks.getState().load();
    expect(useTasks.getState().items[0].persistence_error).toBe("Disk full");
  });
});


it("done tombstones a run before its deferred history reload, so late fired cannot revive it", async () => {
  const response = deferred<ScheduledTask[]>(); invokeMock.mockReturnValueOnce(response.promise);
  useTasks.setState({ items: [task({ id: "t", active_run: active("finished") })], runningIds: ["t"], runningRuns: { t: "finished" } });
  useTasks.getState().applyDone({ id: "t", run_id: "finished", status: "ok" });
  useTasks.getState().applyFired({ id: "t", run_id: "finished" });
  expect(useTasks.getState().runningIds).toEqual([]);
  response.resolve([task({ id: "t", active_run: null, last_status: "ok" })]);
  await response.promise; await Promise.resolve();
  expect(useTasks.getState().runningIds).toEqual([]);
  expect(useTasks.getState().loading).toBe(false);
});

it("deleted tasks reject delayed fired and done until explicitly re-created", () => {
  useTasks.setState({ items: [task({ id: "t", active_run: active("removed") })], runningIds: ["t"], runningRuns: { t: "removed" } });
  useTasks.getState().removeLocal("t");
  useTasks.getState().applyFired({ id: "t", run_id: "removed" });
  useTasks.getState().applyDone({ id: "t", run_id: "removed", status: "ok" });
  expect(useTasks.getState().items).toEqual([]); expect(useTasks.getState().runningIds).toEqual([]);
  expect(invokeMock).not.toHaveBeenCalled();
  useTasks.getState().upsertLocal(task({ id: "t" }));
  useTasks.getState().applyFired({ id: "t", run_id: "fresh" });
  expect(useTasks.getState().runningRuns.t).toBe("fresh");
});


it("editing one task preserves another task's event-backed running identity before its snapshot arrives", () => {
  useTasks.setState({ items: [task({ id: "a", active_run: null }), task({ id: "b", active_run: null })] });
  useTasks.getState().applyFired({ id: "a", run_id: "run1" });
  useTasks.getState().upsertLocal(task({ id: "b", name: "edited", active_run: null }));
  expect(useTasks.getState().runningIds).toEqual(["a"]);
  expect(useTasks.getState().runningRuns).toEqual({ a: "run1" });
  expect(useTasks.getState().items.find((item) => item.id === "b")?.name).toBe("edited");
  // The target's own projection can still be updated without disturbing A.
  useTasks.getState().upsertLocal(task({ id: "b", active_run: active("run2") }));
  expect(useTasks.getState().runningRuns).toEqual({ a: "run1", b: "run2" });
  useTasks.getState().upsertLocal(task({ id: "b", active_run: null }));
  expect(useTasks.getState().runningRuns).toEqual({ a: "run1" });
});


it("a deferred inactive mutation response cannot clear a run fired after that command started", async () => {
  const mutation = deferred<ScheduledTask>();
  const snapshot = deferred<ScheduledTask[]>(); invokeMock.mockReturnValueOnce(snapshot.promise);
  useTasks.setState({ items: [task({ id: "a", active_run: null })] });
  const requestRevision = useTasks.getState().captureRevision();
  const applying = mutation.promise.then((saved) => useTasks.getState().upsertLocal(saved, requestRevision));
  useTasks.getState().applyFired({ id: "a", run_id: "run1" });
  mutation.resolve(task({ id: "a", name: "edited", active_run: null })); await applying;
  expect(useTasks.getState().runningRuns).toEqual({ a: "run1" });
  expect(useTasks.getState().runningIds).toEqual(["a"]);
  expect(useTasks.getState().items[0].name).toBe("edited");
  expect(invokeMock).toHaveBeenCalledWith("list_scheduled_tasks", listCall);
  snapshot.resolve([task({ id: "a", name: "edited", active_run: active("run1") })]); await snapshot.promise; await Promise.resolve();
  expect(useTasks.getState().items[0].active_run?.run_id).toBe("run1");
});

it("a deferred old-run mutation response cannot replace a newer fired run or revive an old snapshot", async () => {
  const oldSnapshot = deferred<ScheduledTask[]>(); const freshSnapshot = deferred<ScheduledTask[]>();
  invokeMock.mockReturnValueOnce(oldSnapshot.promise).mockReturnValueOnce(freshSnapshot.promise);
  useTasks.setState({ items: [task({ id: "a", active_run: active("old") })], runningIds: ["a"], runningRuns: { a: "old" } });
  const requestRevision = useTasks.getState().captureRevision();
  useTasks.getState().applyDone({ id: "a", run_id: "old", status: "ok" });
  useTasks.getState().applyFired({ id: "a", run_id: "new" });
  useTasks.getState().upsertLocal(task({ id: "a", name: "edited", active_run: active("old") }), requestRevision);
  expect(useTasks.getState().runningRuns).toEqual({ a: "new" });
  expect(useTasks.getState().items[0].name).toBe("edited");
  expect(useTasks.getState().items[0].active_run).toBeNull();
  oldSnapshot.resolve([task({ id: "a", active_run: active("old") })]); await oldSnapshot.promise; await Promise.resolve();
  expect(useTasks.getState().runningRuns).toEqual({ a: "new" });
  freshSnapshot.resolve([task({ id: "a", name: "edited", active_run: active("new") })]); await freshSnapshot.promise; await Promise.resolve();
  expect(useTasks.getState().runningRuns).toEqual({ a: "new" });
});


it("a newer authoritative active snapshot also invalidates a delayed mutation lifecycle", async () => {
  useTasks.setState({ items: [task({ id: "a", active_run: null })] });
  const requestRevision = useTasks.getState().captureRevision();
  invokeMock.mockResolvedValueOnce([task({ id: "a", active_run: active("fresh") })]);
  await useTasks.getState().load();
  const refreshed = deferred<ScheduledTask[]>(); invokeMock.mockReturnValueOnce(refreshed.promise);
  useTasks.getState().upsertLocal(task({ id: "a", name: "edited", active_run: null }), requestRevision);
  expect(useTasks.getState().runningRuns).toEqual({ a: "fresh" });
  expect(useTasks.getState().items[0].active_run?.run_id).toBe("fresh");
  refreshed.resolve([task({ id: "a", name: "edited", active_run: active("fresh") })]);
  await refreshed.promise; await Promise.resolve();
});
