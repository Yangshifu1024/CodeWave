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
  useTasks.setState({ items: [], loading: false, error: null, runningIds: [] });
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
