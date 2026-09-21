// 任务中心（TaskCenterPanel）· 版式审计批次新增行为的守门
// （[docs/ui-affordance-visibility-audit](../../../docs/ui-affordance-visibility-audit.md)）：
// ① 计划表达式的语法说明**常驻**在输入框下方（原先只当 placeholder，敲第一个字符即消失）；
// ② 创建按钮禁用时就地给出原因（与设置页「禁用原因写在按钮旁」同一范式，不藏在 Tooltip 里）；
// ③ 状态标签不再用预设绿（无色彩=默认、色彩只映射风险等级），「待触发」文案走 i18n（此前硬编码中文）。
// 挂载方式与 settings.mcp.test.tsx 同源（standalone + store 种子）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup, screen } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n"; // 直接挂载组件需显式初始化 i18next
import TaskCenterPanel from "../features/panels/TaskCenterPanel";
import { useSessions } from "../stores/sessions";
import type { ScheduledTask } from "../ipc/types";

let tasks: ScheduledTask[] = [];
let calls: { cmd: string; args: any }[] = [];

async function baseInvoke(cmd: string, args?: any) {
  calls.push({ cmd, args });
  switch (cmd) {
    case "list_scheduled_tasks": return JSON.parse(JSON.stringify(tasks));
    case "create_scheduled_task": return null;
    case "delete_scheduled_task": return null;
    default: throw new Error(`unmocked command: ${cmd}`);
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class {
    onmessage: any = null;
  },
}));

/** 任务中心按当前会话取数（useActiveId = sessions.activeKey） */
function mount() {
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "任务", projectId: null,
      createdAt: "2026-09-01T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
    projects: [],
  });
  return render(
    <AntApp>
      <TaskCenterPanel />
    </AntApp>,
  );
}

/** 两字按钮会插空格（“创 建”）：按去空白文本匹配 */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

function rows(): HTMLElement[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".task"));
}

function task(over: Partial<ScheduledTask> & { id: string }): ScheduledTask {
  return {
    name: over.id, instruction: "写日报", schedule: "every:30m",
    next_run: null, last_status: null, last_summary: null, ...over,
  };
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  calls = [];
  tasks = [];
});

describe("任务中心：新建区版式与交互", () => {
  it("语法说明常驻在计划输入框下方；placeholder 只是示例表达式", () => {
    mount();

    const field = document.querySelector(".task-sched-field") as HTMLElement;
    expect(field).toBeTruthy();
    // 说明常驻（此前只当 placeholder，敲第一个字符就看不见了）
    expect(field.querySelector(".hint")?.textContent).toContain("计划：");

    const sched = screen.getByPlaceholderText("cron:0 9 * * *") as HTMLInputElement;
    expect(sched.value).toBe("");
    expect(sched.placeholder).not.toContain("计划：");
  });

  it("三字段未齐时创建按钮禁用并就地给出原因；填齐后启用且原因消失", async () => {
    mount();

    expect(buttonByText("创建").disabled).toBe(true);
    expect(document.body.textContent ?? "").toContain("填写任务名、计划与指令后可创建");

    // 只填名字还不够（原先禁用状态没有任何原因可看）
    fireEvent.change(screen.getByPlaceholderText("任务名"), { target: { value: "日报" } });
    expect(buttonByText("创建").disabled).toBe(true);
    expect(document.body.textContent ?? "").toContain("填写任务名、计划与指令后可创建");

    fireEvent.change(screen.getByPlaceholderText("cron:0 9 * * *"), { target: { value: "cron:0 9 * * *" } });
    fireEvent.change(screen.getByPlaceholderText(/任务指令/), { target: { value: "写日报" } });
    await waitFor(() => expect(buttonByText("创建").disabled).toBe(false));
    expect(document.body.textContent ?? "").not.toContain("填写任务名、计划与指令后可创建");
  });

  it("创建：参数按 name/instruction/schedule 下发，成功后清空输入并重拉列表", async () => {
    mount();
    fireEvent.change(screen.getByPlaceholderText("任务名"), { target: { value: "日报" } });
    fireEvent.change(screen.getByPlaceholderText("cron:0 9 * * *"), { target: { value: "cron:0 9 * * *" } });
    fireEvent.change(screen.getByPlaceholderText(/任务指令/), { target: { value: "写日报" } });
    tasks = [task({ id: "t1", name: "日报" })];

    fireEvent.click(buttonByText("创建"));

    await waitFor(() => expect(rows()).toHaveLength(1));
    expect(calls.find((c) => c.cmd === "create_scheduled_task")?.args).toEqual({
      sessionId: "s1", name: "日报", instruction: "写日报", schedule: "cron:0 9 * * *",
    });
    expect((screen.getByPlaceholderText("任务名") as HTMLInputElement).value).toBe("");
  });

  it("空列表显示空态（暂无计划任务）", async () => {
    mount();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("暂无计划任务"));
  });
});

describe("任务中心：状态标签色彩与文案", () => {
  it("无状态显示 i18n「待触发」；ok 走中性标签；非 ok 才是红", async () => {
    tasks = [
      task({ id: "t1", name: "甲" }),
      task({ id: "t2", name: "乙", last_status: "ok" }),
      task({ id: "t3", name: "丙", last_status: "error" }),
    ];
    mount();

    await waitFor(() => expect(rows()).toHaveLength(3));
    const tags = rows().map((r) => r.querySelector(".ant-tag") as HTMLElement);

    // 「待触发」不再硬编码在组件里（英文界面会露中文），走 tasks.pending
    expect(tags[0].textContent).toBe("待触发");
    expect(tags[0].className).not.toContain("ant-tag-success");

    // 成功态中性：预设绿违反「无色彩=默认、色彩只映射风险等级」
    expect(tags[1].textContent).toBe("ok");
    expect(tags[1].className).not.toContain("ant-tag-success");
    expect(tags[1].className).not.toContain("ant-tag-error");

    // 失败态保持红（风险等级映射不变）
    expect(tags[2].className).toContain("ant-tag-error");
  });
});
