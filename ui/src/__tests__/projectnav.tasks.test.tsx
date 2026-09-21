// 左栏任务区（[docs/tasks-module-polish]）· 守门用例：
//   ① 列表来自 stores/tasks 单一数据源（组件不自己 invoke、不留第二份状态）；
//   ② 状态点只区分「失败 = 红」与「其余 = 中性」（与任务页共用 statusKind，禁各写一份）；
//   ③ tooltip 补回颜色之外的信息：本地化状态文案（不再裸显 ok/error）+ 下次触发 + 上次摘要；
//   ④ 点击任务行 = 打开任务页（tasksOpen）。
// 挂载方式与 projectnav.row-states.test.tsx 同源（standalone + store 种子）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent, waitFor } from "@testing-library/react";
import { App } from "antd";
import ProjectNav from "../features/shell/ProjectNav";
import { useSessions } from "../stores/sessions";
import { useTasks } from "../stores/tasks";
import { useUi } from "../stores/ui";
import { invoke } from "@tauri-apps/api/core";
import type { ScheduledTask } from "../ipc/types";

let tasks: ScheduledTask[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => (cmd === "list_scheduled_tasks" ? tasks : [])),
}));

function task(over: Partial<ScheduledTask> & { id: string }): ScheduledTask {
  return {
    name: over.id,
    instruction: "写日报",
    schedule: "cron:0 9 * * *",
    next_run: null,
    last_status: null,
    last_summary: null,
    project_id: null,
    enabled: true,
    runs: [],
    ...over,
  };
}

function mount() {
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  return render(
    <App>
      <ProjectNav />
    </App>,
  );
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  useTasks.setState({ items: [], loading: false, error: null, runningIds: [] });
  useUi.setState({ tasksOpen: false });
  tasks = [];
  vi.mocked(invoke).mockClear();
});

describe("左栏任务区", () => {
  it("列表来自 stores/tasks；状态点只对失败上色（其余中性）", async () => {
    tasks = [
      task({ id: "t1", name: "日报", last_status: "ok" }),
      task({ id: "t2", name: "周报", last_status: "error" }),
      task({ id: "t3", name: "巡检", last_status: "skipped" }),
      task({ id: "t4", name: "归档" }),
    ];
    useTasks.setState({ items: tasks });
    mount();

    await waitFor(() => expect(document.querySelectorAll(".task-nav-row")).toHaveLength(4));
    const dots = Array.from(document.querySelectorAll<HTMLElement>(".task-nav-row .task-dot"));
    // 中性 = 默认灰点（ok / skipped / 从未运行都不上色）；只有失败走 .err
    expect(dots.map((d) => d.className)).toEqual(["task-dot", "task-dot err", "task-dot", "task-dot"]);
    expect(Array.from(document.querySelectorAll(".task-nav-row .task-name")).map((x) => x.textContent)).toEqual([
      "日报", "周报", "巡检", "归档",
    ]);
  });

  it("tooltip：本地化状态文案 + 下次触发 + 上次摘要（不再裸显 ok）", async () => {
    tasks = [
      task({
        id: "t1", name: "日报", last_status: "ok", last_summary: "摘要甲",
        next_run: "2026-10-01T01:00:00Z",
      }),
    ];
    useTasks.setState({ items: tasks });
    mount();

    await waitFor(() => expect(document.querySelector(".task-nav-row")).toBeTruthy());
    const tip = document.querySelector(".task-nav-row")?.getAttribute("title") ?? "";
    expect(tip).toContain("上次状态: 正常");
    expect(tip).not.toContain("ok");
    expect(tip).toContain("下次触发:");
    expect(tip).toContain("摘要甲");
  });

  it("暂停的任务在 tooltip 里说明「已暂停」；点行打开任务页", async () => {
    tasks = [task({ id: "t1", name: "日报", enabled: false, last_status: null })];
    useTasks.setState({ items: tasks });
    mount();

    await waitFor(() => expect(document.querySelector(".task-nav-row")).toBeTruthy());
    const row = document.querySelector(".task-nav-row") as HTMLElement;
    expect(row.getAttribute("title")).toContain("已暂停");

    fireEvent.click(row);
    expect(useUi.getState().tasksOpen).toBe(true);
  });
});
