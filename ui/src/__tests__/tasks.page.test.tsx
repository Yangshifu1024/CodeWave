// 计划任务独立页（TasksPage）· 取代原弹框的守门用例（[docs/tasks-module-polish]）：
//   ① 列表：名称 + 人类可读周期描述 + 项目归属 + 状态（中性/红）+ 下次触发四态；
//   ② 整行展开执行历史（runs 新的在前、最多 20 条、来源定时/手动）；
//   ③ 暂停 / 立即运行 / 删除三条命令的参数与失败处理（失败不动列表）；
//   ④ 加载失败：错误行常驻 + 旧列表保留；
//   ⑤「必须归属项目会话」才能新建（其余操作不受限）；
//   ⑥ 周期选择器 → 表达式（保存时下发 cron 形态，用户不再手写）；
//   ⑦ 状态文案本地化（statusLabel：ok/error/skipped → 正常/失败/已跳过，未知状态原样回显）；
//   ⑧ 删除失败后重新拉一次列表对齐后端真相；暂停开关失败不乐观改本地。
// 挂载方式与 settings.mcp.test.tsx 同源（standalone + store 种子）。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n"; // 直接挂载组件需显式初始化 i18next
import TasksPage from "../features/panels/TasksPage";
import { useSessions } from "../stores/sessions";
import { useTasks } from "../stores/tasks";
import { useUi } from "../stores/ui";
import type { ScheduledTask, TaskRun } from "../ipc/types";

let tasks: ScheduledTask[] = [];
let calls: { cmd: string; args: any }[] = [];
let listFails = false;
let deleteFails = false;
let runFails = false;
let runGate: Promise<void> | null = null;
let saveFails = false;
let enableFails = false;

async function baseInvoke(cmd: string, args?: any) {
  calls.push({ cmd, args });
  switch (cmd) {
    case "list_scheduled_tasks":
      if (listFails) throw new Error("boom-load");
      return JSON.parse(JSON.stringify(tasks));
    case "set_scheduled_task_enabled": {
      if (enableFails) throw new Error("boom-enable");
      const hit = tasks.find((x) => x.id === args.id) ?? { id: args.id };
      return JSON.parse(JSON.stringify({ ...hit, enabled: args.enabled }));
    }
    case "run_scheduled_task_now":
      if (runGate) await runGate;
      if (runFails) throw new Error("已有任务正在运行，请稍后再试");
      return null;
    case "delete_scheduled_task":
      if (deleteFails) throw new Error("boom-delete");
      return null;
    case "create_scheduled_task":
    case "update_scheduled_task":
      // 后端拒绝的常见形态（参数非法 / 被占）：保存失败必须就近展示错误原文
      if (saveFails) throw new Error("invalid schedule");
      return JSON.parse(JSON.stringify(task({
        id: args.id ?? "new1", name: args.name, instruction: args.instruction, schedule: args.schedule,
      })));
    default:
      throw new Error(`unmocked command: ${cmd}`);
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class {
    onmessage: any = null;
  },
}));

function task(over: Partial<ScheduledTask> & { id: string }): ScheduledTask {
  return {
    name: over.id, instruction: "写日报", schedule: "cron:0 9 * * *",
    next_run: null, last_status: null, last_summary: null, project_id: null, enabled: true, runs: [], ...over,
  };
}

function run(over: Partial<TaskRun> = {}): TaskRun {
  return { at: "2026-10-02T01:00:00Z", status: "ok", summary: "", source: "schedule", out_tokens: 0, ...over };
}

/** 挂载（projectId = null 模拟临时会话；activeKey = null 模拟没有会话） */
function mount(opts: { projectId?: string | null; session?: boolean } = {}) {
  const session = opts.session ?? true;
  const projectId = opts.projectId === undefined ? "p1" : opts.projectId;
  useSessions.setState({
    tabs: session
      ? [{
        key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "任务", projectId,
        createdAt: "2026-09-01T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      }]
      : [],
    activeKey: session ? "s1" : null,
    projects: projectId ? [{ id: "p1", name: "演示项目", directory: "/tmp/proj", created_at: "2026-09-01T00:00:00Z" }] : [],
  });
  useUi.setState({ tasksOpen: true });
  return render(
    <AntApp>
      <TasksPage />
    </AntApp>,
  );
}

/** 两字按钮会插空格（「编 辑」）：按去空白文本匹配 */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function allButtonsByText(text: string): HTMLButtonElement[] {
  return Array.from(document.querySelectorAll("button")).filter(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  ) as HTMLButtonElement[];
}

function rows(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".task-row"));
}

function tags(row: HTMLElement): HTMLElement[] {
  return Array.from(row.querySelectorAll<HTMLElement>(".ant-tag"));
}

/** Popconfirm：点触发器后确认（与 providers.panel.test.tsx 同一手法） */
async function confirmPopconfirm(trigger: HTMLElement) {
  fireEvent.click(trigger);
  await waitFor(() => expect(document.querySelector(".ant-popover .ant-btn-primary")).toBeTruthy());
  fireEvent.click(document.querySelector(".ant-popover .ant-btn-primary") as HTMLButtonElement);
}

beforeEach(() => {
  useTasks.setState({ items: [], loading: false, error: null, runningIds: [] });
  tasks = [];
  calls = [];
  listFails = false;
  deleteFails = false;
  runFails = false;
  runGate = null;
  saveFails = false;
  enableFails = false;
});

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, projects: [], projectsLoadFailed: false });
  useUi.setState({ tasksOpen: false });
});

describe("任务页：列表渲染", () => {
  it("返回按钮占满侧栏，任务卡片把信息和操作分层", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    expect(document.querySelector(".tasks-nav-back")?.classList.contains("ant-btn-block")).toBe(true);
    expect(rows()[0].classList.contains("ant-card")).toBe(true);
    expect(rows()[0].querySelector(".ant-card-head")?.textContent).toContain("日报");
    expect(rows()[0].querySelector(".task-row-meta")?.textContent).toContain("下次触发");
    expect(rows()[0].querySelectorAll(".ant-card-actions > li")).toHaveLength(3);
  });

  it("名称 + 周期描述 + 项目归属 + 状态标签（本地化文案：正常/失败/已跳过）+ 下次触发", async () => {
    tasks = [
      task({ id: "t1", name: "日报", last_status: "ok", next_run: "2026-10-01T01:00:00Z" }),
      task({ id: "t2", name: "周报", schedule: "cron:0 9 * * 1,3,5", last_status: "error" }),
      task({ id: "t3", name: "巡检", schedule: "every:30 m" }),
      task({ id: "t4", name: "归档", last_status: "skipped" }),
    ];
    mount();

    await waitFor(() => expect(rows()).toHaveLength(4));
    const [r1, r2, r3, r4] = rows();

    expect(r1.textContent).toContain("日报");
    expect(r1.textContent).toContain("每天 09:00");
    // 下次触发：本地 YYYY-MM-DD HH:mm（不写死具体时区，免得 CI 与本机时区不同时红）
    expect(r1.textContent).toMatch(/\d{4}-\d{2}-\d{2} \d{2}:\d{2}/);
    // 项目归属：接口不带 project_id → 显示「未归属项目」（不硬造字段）
    expect(r1.textContent).toContain("未归属项目");

    // 状态文案走 statusLabel 本地化（不再裸显 ok/error）；色彩只映射风险等级：ok/skipped 中性、error 红
    expect(tags(r1)[0].textContent).toBe("正常");
    expect(tags(r1)[0].className).not.toContain("ant-tag-error");
    expect(tags(r2)[0].textContent).toBe("失败");
    expect(tags(r2)[0].className).toContain("ant-tag-error");
    expect(tags(r4)[0].textContent).toBe("已跳过");
    expect(tags(r4)[0].className).not.toContain("ant-tag-error");

    expect(r2.textContent).toContain("每周一、三、五 09:00");
    expect(r3.textContent).toContain("每 30 分钟");
    // 从未跑过（last_status == null）：无状态标签，下次触发位显示「从未运行」
    expect(tags(r3)).toHaveLength(0);
    expect(r3.textContent).toContain("从未运行");
  });

  it("暂停与「不再触发」：enabled=false 优先显示已暂停；once 且无 next_run 显示不再触发", async () => {
    tasks = [
      task({ id: "t1", name: "甲", enabled: false, next_run: "2026-10-01T01:00:00Z" }),
      task({ id: "t2", name: "乙", schedule: "once:2026-10-01T01:00:00Z", last_status: "ok" }),
    ];
    mount();

    await waitFor(() => expect(rows()).toHaveLength(2));
    expect(rows()[0].textContent).toContain("已暂停");
    expect(rows()[1].textContent).toContain("不再触发");
    expect(rows()[1].textContent).toContain("仅一次");
  });

  it("运行中的任务带「运行中」标记（runningIds 由事件驱动）", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    useTasks.setState({ runningIds: ["t1"] });
    mount();

    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0].textContent).toContain("运行中");
  });

  it("空列表：Empty + 创建引导", async () => {
    mount();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("暂无计划任务"));
  });
});

describe("任务页：执行历史", () => {
  it("整行可点展开（含 Enter/Space），runs 按后端顺序（新的在前）渲染，来源区分定时/手动", async () => {
    tasks = [task({
      id: "t1", name: "日报",
      runs: [
        run({ at: "2026-10-02T01:00:00Z", status: "ok", summary: "最新一次", source: "schedule", out_tokens: 12 }),
        run({ at: "2026-10-01T01:00:00Z", status: "error", summary: "上一次失败", source: "manual", out_tokens: 3 }),
      ],
    })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    const main = document.querySelector(".task-row-main") as HTMLElement;
    expect(main.getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelectorAll(".tasks-history-row")).toHaveLength(0);

    fireEvent.click(main);
    await waitFor(() => expect(main.getAttribute("aria-expanded")).toBe("true"));
    const runs = Array.from(document.querySelectorAll<HTMLElement>(".tasks-history-row"));
    expect(runs).toHaveLength(2);
    expect(runs[0].textContent).toContain("最新一次");
    expect(runs[0].textContent).toContain("定时");
    expect(runs[0].textContent).toContain("12 tok");
    expect(runs[1].textContent).toContain("上一次失败");
    expect(runs[1].textContent).toContain("手动");
    // 失败记录走红（风险等级映射），成功记录不带红；文案同样本地化
    expect((runs[0].querySelector(".tasks-history-status") as HTMLElement).textContent).toBe("正常");
    expect((runs[1].querySelector(".tasks-history-status") as HTMLElement).textContent).toBe("失败");
    expect((runs[1].querySelector(".tasks-history-status") as HTMLElement).className).toContain("tasks-history-status-error");
    expect((runs[0].querySelector(".tasks-history-status") as HTMLElement).className).not.toContain("tasks-history-status-error");

    // 键盘：Enter 收起、Space 再展开
    fireEvent.keyDown(main, { key: "Enter" });
    await waitFor(() => expect(main.getAttribute("aria-expanded")).toBe("false"));
    fireEvent.keyDown(main, { key: " " });
    await waitFor(() => expect(main.getAttribute("aria-expanded")).toBe("true"));
  });

  it("runs 最多渲染 20 条（防旧数据异常膨胀）", async () => {
    tasks = [task({
      id: "t1", name: "日报",
      runs: Array.from({ length: 21 }, (_, i) => run({ at: `2026-10-${String(i + 1).padStart(2, "0")}T01:00:00Z`, summary: `第 ${i} 条` })),
    })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));
    fireEvent.click(document.querySelector(".task-row-main") as HTMLElement);

    await waitFor(() => expect(document.querySelectorAll(".tasks-history-row")).toHaveLength(20));
    // 截断保留的是**最新**的 20 条（后端已是新的在前）
    expect(document.body.textContent ?? "").toContain("第 0 条");
    expect(document.body.textContent ?? "").not.toContain("第 20 条");
  });

  it("没有执行记录时空态文案", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));
    fireEvent.click(document.querySelector(".task-row-main") as HTMLElement);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("还没有执行记录"));
  });
});

describe("任务页：行内操作", () => {
  it("立即运行请求未结束时显示 loading 并阻止重复提交", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    let release = () => {};
    runGate = new Promise<void>((resolve) => { release = resolve; });
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    fireEvent.click(buttonByText("立即运行"));
    expect(buttonByText("立即运行").disabled).toBe(true);
    fireEvent.click(buttonByText("立即运行"));
    expect(calls.filter((call) => call.cmd === "run_scheduled_task_now")).toHaveLength(1);

    release();
    await waitFor(() => expect(buttonByText("立即运行").disabled).toBe(false));
  });

  it("暂停开关：调 set_scheduled_task_enabled 并把返回的任务就地合并", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    const sw = document.querySelector('.task-row [role="switch"]') as HTMLElement;
    fireEvent.click(sw);

    await waitFor(() => expect(calls.find((c) => c.cmd === "set_scheduled_task_enabled")?.args).toEqual({
      id: "t1", enabled: false,
    }));
    await waitFor(() => expect(document.querySelector('.task-row [role="switch"]')?.getAttribute("aria-checked")).toBe("false"));
  });

  it("立即运行：调 run_scheduled_task_now；被占（reject）时把后端原文显示出来", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    let release = () => {};
    runGate = new Promise<void>((resolve) => { release = resolve; });
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    fireEvent.click(buttonByText("立即运行"));
    await waitFor(() => expect(calls.find((c) => c.cmd === "run_scheduled_task_now")?.args).toEqual({ id: "t1" }));
    expect(buttonByText("立即运行").disabled).toBe(true);
    release();
    await waitFor(() => expect(buttonByText("立即运行").disabled).toBe(false));

    runGate = null;
    runFails = true;
    fireEvent.click(buttonByText("立即运行"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已有任务正在运行"));
  });

  it("删除：Popconfirm 确认后调 delete_scheduled_task 并移除该行", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    await confirmPopconfirm(buttonByText("删除"));

    await waitFor(() => expect(calls.find((c) => c.cmd === "delete_scheduled_task")?.args).toEqual({
      sessionId: "s1", id: "t1",
    }));
    await waitFor(() => expect(rows()).toHaveLength(0));
  });

  it("删除失败：列表不消失且给出错误提示", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    deleteFails = true;
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    await confirmPopconfirm(buttonByText("删除"));

    await waitFor(() => expect(document.body.textContent ?? "").toContain("boom-delete"));
    expect(rows()).toHaveLength(1); // 任务还在：用户可原样重试
    // 失败后重新拉一次列表对齐后端真相（失败原因可能是「已被别处删掉」的幽灵行）
    await waitFor(() => expect(calls.filter((c) => c.cmd === "list_scheduled_tasks").length).toBeGreaterThan(1));
  });

  it("暂停失败：给出错误提示且开关不乐观改本地", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    enableFails = true;
    fireEvent.click(document.querySelector('.task-row [role="switch"]') as HTMLElement);

    await waitFor(() => expect(document.body.textContent ?? "").toContain("boom-enable"));
    // 失败不写本地：开关仍是「开」（否则界面会说谎，用户以为已暂停）
    expect(document.querySelector('.task-row [role="switch"]')?.getAttribute("aria-checked")).toBe("true");
  });
});

describe("任务页：三态与归属", () => {
  it("加载失败：顶部错误行可见且旧列表仍在", async () => {
    listFails = true;
    useTasks.setState({ items: [task({ id: "t1", name: "旧任务" })] });
    mount();

    await waitFor(() => expect(document.querySelector(".tasks-error")?.textContent).toContain("加载失败"));
    expect(rows()).toHaveLength(1);
    expect(document.body.textContent ?? "").toContain("旧任务");
  });

  it("没有会话：新建禁用并就地给出原因，其余操作不受限", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount({ session: false });

    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(buttonByText("新建任务").disabled).toBe(true);
    expect(document.body.textContent ?? "").toContain("先打开一个项目会话");
    // 查看/编辑/立即运行/删除仍然可用（只有「新建」需要项目归属）
    expect(buttonByText("编辑").disabled).toBe(false);
    expect(buttonByText("立即运行").disabled).toBe(false);
    expect(buttonByText("删除").disabled).toBe(false);
  });

  it("临时会话（Tab 无 projectId）：原因文案换成「不会落盘」", async () => {
    tasks = [task({ id: "t1", name: "日报" })];
    mount({ projectId: null });

    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(buttonByText("新建任务").disabled).toBe(true);
    expect(document.body.textContent ?? "").toContain("当前是临时会话，任务不会落盘");
  });

  it("归属项目会话：新建可用（禁用原因消失）", async () => {
    mount();
    await waitFor(() => expect(allButtonsByText("新建任务").length).toBeGreaterThan(0));
    for (const btn of allButtonsByText("新建任务")) expect(btn.disabled).toBe(false);
    expect(document.body.textContent ?? "").not.toContain("先打开一个项目会话");
  });

  it("返回工作区与页内 Esc 都关闭页面", async () => {
    mount();
    fireEvent.click(buttonByText("返回工作区"));
    expect(useUi.getState().tasksOpen).toBe(false);

    useUi.setState({ tasksOpen: true });
    fireEvent.keyDown(window, { key: "Escape" });
    expect(useUi.getState().tasksOpen).toBe(false);
  });
});

describe("任务页：表单提示与项目归属兜底", () => {
  it("语法说明常驻且示例合法（every:30 m 带空白，不出现后端会拒的 every:30m）", async () => {
    mount();
    await waitFor(() => expect(allButtonsByText("新建任务").length).toBeGreaterThan(0));
    fireEvent.click(allButtonsByText("新建任务")[0]);
    await waitFor(() => expect(document.querySelector(".ant-modal")).toBeTruthy());

    const hint = document.querySelector(".ant-modal .hint")?.textContent ?? "";
    expect(hint).toContain("every:30 m");
    expect(hint).not.toContain("every:30m");
    // 填写不完整时就地给出「为何不能保存」（禁用无原因会让用户猜）
    expect(document.querySelector(".ant-modal")?.textContent ?? "").toContain("填写任务名、周期与指令后可保存");

    fireEvent.change(document.querySelector("#task-name") as HTMLInputElement, { target: { value: "日报" } });
    fireEvent.change(document.querySelector("#task-instruction") as HTMLTextAreaElement, { target: { value: "写日报" } });
    await waitFor(() =>
      expect(document.querySelector(".ant-modal")?.textContent ?? "").not.toContain("填写任务名、周期与指令后可保存"),
    );
  });

  it("项目注册表读盘失败时不宣称「项目已不存在」（改为信息未加载）", async () => {
    tasks = [task({ id: "t1", name: "日报", project_id: "gone" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0].textContent).toContain("项目已不存在");

    useSessions.setState({ projectsLoadFailed: true });
    await waitFor(() => expect(rows()[0].textContent).toContain("项目信息未加载"));
  });
});

describe("任务页：周期选择器 → 表达式", () => {
  it("新建：默认周期「每天 09:00」保存为 cron:0 9 * * *（用户不手写 cron）", async () => {
    mount();
    await waitFor(() => expect(allButtonsByText("新建任务").length).toBeGreaterThan(0));
    fireEvent.click(allButtonsByText("新建任务")[0]);

    await waitFor(() => expect(document.querySelector(".ant-modal")).toBeTruthy());
    fireEvent.change(document.querySelector("#task-name") as HTMLInputElement, { target: { value: "日报" } });
    fireEvent.change(document.querySelector("#task-instruction") as HTMLTextAreaElement, { target: { value: "写日报" } });
    // 时间保持默认 09:00（周期默认「每天」）
    expect((document.querySelector("#task-time") as HTMLInputElement).value).toBe("09:00");

    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement);

    await waitFor(() => expect(calls.find((c) => c.cmd === "create_scheduled_task")?.args).toEqual({
      sessionId: "s1", name: "日报", instruction: "写日报", schedule: "cron:0 9 * * *",
    }));
    // 成功后弹窗关闭、新任务就地入列（upsertLocal 免一次整表刷新）
    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(rows()[0].textContent).toContain("日报");
  });

  it("编辑：既有 cron 反解成「每天」并把改后的时间写成新表达式", async () => {
    tasks = [task({ id: "t1", name: "日报", schedule: "cron:0 9 * * *", instruction: "写日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    fireEvent.click(buttonByText("编辑"));
    await waitFor(() => expect(document.querySelector(".ant-modal")).toBeTruthy());
    expect((document.querySelector("#task-name") as HTMLInputElement).value).toBe("日报");
    expect((document.querySelector("#task-time") as HTMLInputElement).value).toBe("09:00");

    fireEvent.change(document.querySelector("#task-time") as HTMLInputElement, { target: { value: "10:30" } });
    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement);

    await waitFor(() => expect(calls.find((c) => c.cmd === "update_scheduled_task")?.args).toEqual({
      id: "t1", name: "日报", instruction: "写日报", schedule: "cron:30 10 * * *",
    }));
  });

  it("编辑：认不出的表达式回落到自定义输入，原样保留用户写法", async () => {
    tasks = [task({ id: "t1", name: "日报", schedule: "cron:0 9 * * 1-5", instruction: "写日报" })];
    mount();
    await waitFor(() => expect(rows()).toHaveLength(1));

    fireEvent.click(buttonByText("编辑"));
    await waitFor(() => expect(document.querySelector(".ant-modal")).toBeTruthy());
    expect((document.querySelector("#task-expr") as HTMLInputElement).value).toBe("cron:0 9 * * 1-5");

    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement);
    await waitFor(() => expect(calls.find((c) => c.cmd === "update_scheduled_task")?.args).toEqual({
      id: "t1", name: "日报", instruction: "写日报", schedule: "cron:0 9 * * 1-5",
    }));
  });

  it("保存失败：后端错误原文展示在弹窗内，弹窗不关", async () => {
    tasks = [];
    mount();
    await waitFor(() => expect(allButtonsByText("新建任务").length).toBeGreaterThan(0));
    fireEvent.click(allButtonsByText("新建任务")[0]);
    await waitFor(() => expect(document.querySelector(".ant-modal")).toBeTruthy());

    saveFails = true; // 后端拒绝（等价于参数非法或被占）
    fireEvent.change(document.querySelector("#task-name") as HTMLInputElement, { target: { value: "日报" } });
    fireEvent.change(document.querySelector("#task-instruction") as HTMLTextAreaElement, { target: { value: "写日报" } });
    fireEvent.click(document.querySelector(".ant-modal .ant-btn-primary") as HTMLButtonElement);

    await waitFor(() => expect(document.querySelector(".ant-modal")?.textContent ?? "").toContain("保存失败"));
    expect(document.querySelector(".ant-modal")?.textContent ?? "").toContain("invalid schedule");
  });
});
