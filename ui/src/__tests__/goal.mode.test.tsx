// 目标模式（`ApprovalMode::Goal`）前端守护：
// ① `goal:update` 载荷落进 run store（runHandlers 路径）；② `resumeGoal` 的语义守卫；
// ③ 右栏「目标」段渲染（正文 / 达成标准 / 账本摘要 / 状态徽标 / 轮次）与「继续推进」；
// ④ `resumeGoal` 的本地用户气泡（文案与后端 `RESUME_GOAL_TEXT` 逐字一致）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { App as AntApp } from "antd";
import { invoke, Channel } from "@tauri-apps/api/core";
import "../i18n";
import Composer from "../features/chat/Composer";
import RightBar from "../features/shell/RightBar";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import type { GoalState, SessionPrefs } from "../ipc/types";

/** 后端桩（vi.hoisted：mock 工厂在模块体之前执行，这里只能在工厂里读的只能是提升过的引用）：
 *  `goal` / `prefs` 就是 `get_session_goal` / `get_session_prefs` 的返回值，用例按需改写。 */
const backend = vi.hoisted(() => ({
  goal: null as unknown,
  prefs: { approval_mode: "goal", model_id: null, reasoning_effort: null } as Record<string, unknown>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_skills" || cmd === "list_editors" || cmd === "list_agents" || cmd === "quota_snapshots") {
      return [];
    }
    if (cmd === "get_session_goal") return backend.goal;
    if (cmd === "get_session_prefs") return backend.prefs;
    // resume_goal 与 start_chat 同签名：返回本轮 run_id
    if (cmd === "resume_goal") return "run-resume-1";
    return null;
  }),
  // 事件通道（形态与真实包一致：onmessage 由调用方挂）
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

/** 目标状态夹具（只覆盖断言用到的字段，其余给中性默认） */
function goal(over: Partial<GoalState> = {}): GoalState {
  return {
    text: "把登录改成 OAuth",
    criteria: [
      { title: "单测全绿", done: true },
      { title: "文档更新", done: false },
    ],
    ledger: { paths: ["src/auth.rs", "src/login.rs"], programs: ["cargo", "pnpm"] },
    status: "paused",
    decisions: [],
    pending: [],
    blocked: [],
    rounds: 3,
    stall_streak: 0,
    ledger_denials: 0,
    ...over,
  };
}

/** 目标模式的会话 Tab（prefs.approval_mode = "goal"） */
function seed() {
  useSessions.setState({
    tabs: [
      {
        key: "s1",
        sessionId: "s1",
        workspace: "D:/demo/project",
        title: "目标会话",
        projectId: "p1",
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "goal", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: [
      {
        id: "p1",
        name: "demo",
        directory: "D:/demo/project",
        data_dir: null,
        created_at: "2026-09-08T00:00:00Z",
      },
    ],
  });
  useUi.setState({ rightBarOpen: true, rbTab: "info" });
}

afterEach(() => {
  cleanup();
  localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState({ tabs: {}, drafts: {} } as any);
  backend.goal = null;
  backend.prefs = { approval_mode: "goal", model_id: null, reasoning_effort: null };
  vi.mocked(invoke).mockClear();
});

/** 服务端 prefs（整体替换写的数据源） */
function serverPrefs(p: Partial<SessionPrefs>): void {
  backend.prefs = { approval_mode: "goal", model_id: null, reasoning_effort: null, ...p };
}

/** 后端 prefs 真值（档位胶囊断言用） */
function tabPrefs(): SessionPrefs {
  return useSessions.getState().tabs[0].prefs;
}

describe("goal:update → run store", () => {
  it("载荷写进 tabs[key].goal；goal=null 整体清空；未开 Tab 不建桶（M-1）", () => {
    useRun.setState({ tabs: { s1: { goal: null, todos: [] } } } as any);
    const h = useRun.getState().bindGlobalHandlers();
    h["goal:update"]({ session: "s1", goal: goal({ status: "executing" }) });
    expect(useRun.getState().tabs["s1"].goal?.status).toBe("executing");
    expect(useRun.getState().tabs["s1"].goal?.criteria).toHaveLength(2);
    expect(useRun.getState().tabs["s1"].goal?.ledger.paths).toHaveLength(2);
    // null = 目标已清除 / 已回落前档：整体覆盖，不留旧目标
    h["goal:update"]({ session: "s1", goal: null });
    expect(useRun.getState().tabs["s1"].goal).toBeNull();
    // 已关 Tab 的迟到事件不重建状态桶
    h["goal:update"]({ session: "closed", goal: goal() });
    expect(useRun.getState().tabs["closed"]).toBeUndefined();
  });

  it("resumeGoal 只对「已暂停」的目标发 resume_goal（执行中不重复续跑）", async () => {
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);    await useRun.getState().resumeGoal("s1");
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "resume_goal")).toBe(true);

    vi.mocked(invoke).mockClear();
    useRun.setState({ tabs: { s1: { goal: goal({ status: "executing" }), todos: [], items: [] } } } as any);    await useRun.getState().resumeGoal("s1");
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "resume_goal")).toBe(false);
  });

  it("resume_goal 与 start_chat 同构：带事件 channel、run_id 落运行态、本地置运行中", async () => {
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);    await useRun.getState().resumeGoal("s1");
    const call = vi.mocked(invoke).mock.calls.find((c) => c[0] === "resume_goal");
    const args = (call as any)[1];
    // 后端 resume_goal(session_id, on_event: Channel) -> run_id：调用形态与 start_chat 逐字同形
    expect(args.sessionId).toBe("s1");
    expect(args.onEvent).toBeInstanceOf(Channel);
    expect(typeof args.onEvent.onmessage).toBe("function");
    // 返回值落运行态（运行中标记 + run_id）
    expect(useRun.getState().tabs["s1"].runId).toBe("run-resume-1");
    expect(useRun.getState().tabs["s1"].running).toBe(true);
  });

  it("resume_goal 起跑失败：落错误项并退回未运行（目标回滚由后端 goal:update 下发）", async () => {
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);
    vi.mocked(invoke).mockImplementationOnce((() => Promise.reject(new Error("E_NO_MODEL"))) as any);
    await useRun.getState().resumeGoal("s1");
    expect(useRun.getState().tabs["s1"].running).toBe(false);
    expect(useRun.getState().tabs["s1"].items.at(-1)).toMatchObject({ kind: "error" });
  });
});

describe("resumeGoal 的本地用户气泡", () => {
  // 后端 `resume_goal` 会把 `RESUME_GOAL_TEXT`（"继续推进"）写进会话历史再起 run；
  // 前端必须同步补一条本地用户项，否则实时视图里只有助手输出、找不到对应的用户消息。
  it("成功后 items 里出现文案为「继续推进」的用户项（与后端常量逐字一致）", async () => {
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);
    await useRun.getState().resumeGoal("s1");
    const items = useRun.getState().tabs["s1"].items;
    expect(items).toHaveLength(1);
    // 断言字面量而非引用常量：该文案是与后端 RESUME_GOAL_TEXT 绑定的持久化标记，改字即漂移
    expect(items[0]).toMatchObject({ kind: "user", text: "继续推进" });
    // UiItem 是判别联合（user / assistant / sub），createdAt 只存在于前两支：
    // toMatchObject 是运行时断言、不做类型收窄，故先显式收窄再取字段（tsc --noEmit 门禁）。
    const first = items[0];
    if (first.kind !== "user") throw new Error(`期望 user 项，实际 ${first.kind}`);
    expect(first.createdAt).toBeTruthy();
  });

  it("起跑失败：不留下该用户项（后端未落历史，只落错误项）", async () => {
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);
    vi.mocked(invoke).mockImplementationOnce((() => Promise.reject(new Error("E_NO_MODEL"))) as any);
    await useRun.getState().resumeGoal("s1");
    const items = useRun.getState().tabs["s1"].items;
    expect(items.some((i) => i.kind === "user")).toBe(false);
    expect(items.at(-1)).toMatchObject({ kind: "error" });
  });
});

describe("目标状态回读（get_session_goal）", () => {
  it("回读写入 tabs[key].goal；服务端 null 写干净 null；已关 Tab 不建桶", async () => {
    backend.goal = goal({ status: "executing" });
    useRun.setState({ tabs: { s1: { goal: null, todos: [], goalRev: 0 } } } as any);
    await useRun.getState().syncGoal("s1");
    expect(useRun.getState().tabs["s1"].goal?.status).toBe("executing");

    // 服务端无目标 → 写 null（不是 undefined、也不留旧目标）
    backend.goal = null;
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], goalRev: 0 } } } as any);
    await useRun.getState().syncGoal("s1");
    expect(useRun.getState().tabs["s1"].goal).toBeNull();

    // 已关 Tab：不重建状态桶（M-1）
    await useRun.getState().syncGoal("closed");
    expect(useRun.getState().tabs["closed"]).toBeUndefined();
  });

  it("推送优先：回读期间到达的 goal:update 不被更旧的回读结果覆盖", async () => {
    let release: ((v: any) => void) | null = null;
    vi.mocked(invoke).mockImplementationOnce(
      (() => new Promise((res) => { release = res; })) as any,
    );
    useRun.setState({ tabs: { s1: { goal: null, todos: [], goalRev: 0 } } } as any);
    const pending = useRun.getState().syncGoal("s1");
    // 回读在途时推送先到（后端已推进到「执行中」）
    useRun.getState().bindGlobalHandlers()["goal:update"]({
      session: "s1",
      goal: goal({ status: "executing" }),
    });
    release!(goal({ status: "clarify" })); // 回读返回的是更旧的快照
    await pending;
    expect(useRun.getState().tabs["s1"].goal?.status).toBe("executing");
  });

  it("回读被拒（IPC 失败）：不写脏值、不抛未捕获异常，既有 goal 原样保留", async () => {
    const before = goal({ status: "executing" });
    useRun.setState({ tabs: { s1: { goal: before, todos: [], goalRev: 0 } } } as any);
    vi.mocked(invoke).mockImplementationOnce(
      (() => Promise.reject(new Error("E_SESSION_GONE"))) as any,
    );
    // 不抛未捕获异常：调用方多为 `void syncGoal(...)`，reject 逃逸即 unhandled rejection
    await expect(useRun.getState().syncGoal("s1")).resolves.toBeUndefined();
    // 防空洞通过：确认拒绝真的发生在回读链路上（而不是压根没发请求）
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "get_session_goal")).toBe(true);
    // 引用不变 = 一个字段都没被写脏（写成 null / 只写部分字段都算脏）
    expect(useRun.getState().tabs["s1"].goal).toBe(before);
    expect(useRun.getState().tabs["s1"].goal?.status).toBe("executing");
    expect(useRun.getState().tabs["s1"].goalRev).toBe(0);
  });

  it("回读被拒：无目标 Tab 保持 null（失败不得伪造目标、也不得建桶）", async () => {
    useRun.setState({ tabs: { s1: { goal: null, todos: [], goalRev: 0 } } } as any);
    vi.mocked(invoke).mockImplementationOnce(
      (() => Promise.reject(new Error("E_SESSION_GONE"))) as any,
    );
    await useRun.getState().syncGoal("s1");
    expect(useRun.getState().tabs["s1"].goal).toBeNull();
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "get_session_goal")).toBe(true);
    // 失败路径同样受 M-1 约束：不因为一次失败就建桶
    vi.mocked(invoke).mockImplementationOnce(
      (() => Promise.reject(new Error("E_SESSION_GONE"))) as any,
    );
    await useRun.getState().syncGoal("closed");
    expect(useRun.getState().tabs["closed"]).toBeUndefined();
  });
});

describe("目标达成的档位回落同步", () => {
  it("goal:update(status=done) → 回读服务端 prefs（整体替换写，不丢其他字段）", async () => {
    seed();
    serverPrefs({ approval_mode: "auto_edit", model_id: "m9", reasoning_effort: "high" });
    useRun.setState({ tabs: { s1: { goal: goal({ status: "executing" }), todos: [], goalRev: 0 } } } as any);
    act(() => {
      useRun.getState().bindGlobalHandlers()["goal:update"]({
        session: "s1",
        goal: goal({ status: "done" }),
      });
    });
    await waitFor(() => expect(tabPrefs().approval_mode).toBe("auto_edit"));
    expect(tabPrefs()).toEqual({ approval_mode: "auto_edit", model_id: "m9", reasoning_effort: "high" });
  });

  it("run:done（目标档收尾）→ 同样回读，权限胶囊随 store 更新", async () => {
    seed();
    serverPrefs({ approval_mode: "confirm_each" });
    useRun.setState({
      tabs: {
        s1: {
          goal: goal({ status: "done" }), todos: [], goalRev: 0, running: true, items: [],
          subs: [], subStreams: {}, queue: [],
        },
      },
    } as any);
    act(() => {
      useRun.getState().bindGlobalHandlers()["run:done"]({ session: "s1", run_id: "r1" });
    });
    await waitFor(() => expect(tabPrefs().approval_mode).toBe("confirm_each"));
  });

  it("用户切档与回读不打架：回读期间本地切档 → 保留本地新值", async () => {
    seed();
    serverPrefs({ approval_mode: "auto_edit" });
    let release: ((v: any) => void) | null = null;
    vi.mocked(invoke).mockImplementationOnce(
      (() => new Promise((res) => { release = res; })) as any,
    );
    const pending = useSessions.getState().syncPrefs("s1");
    await useSessions.getState().updatePrefs("s1", { approval_mode: "full_access" });
    release!(backend.prefs); // 回读结果比用户操作更旧
    await pending;
    expect(tabPrefs().approval_mode).toBe("full_access");
  });

  it("回读被拒（IPC 失败）：Tab.prefs 原样保留（不清空、不写 undefined）", async () => {
    seed();
    const before = tabPrefs();
    vi.mocked(invoke).mockImplementationOnce(
      (() => Promise.reject(new Error("E_SESSION_GONE"))) as any,
    );
    // 不抛未捕获异常（handler 里的 `void syncPrefs(...)` 全靠内部 catch 兵底）
    await expect(useSessions.getState().syncPrefs("s1")).resolves.toBeUndefined();
    // 防空洞通过：拒绝真的发生在回读链路上（否则 prefs 不变可能只是没发请求）
    expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "get_session_prefs")).toBe(true);
    expect(tabPrefs()).toBeDefined();
    expect(tabPrefs()).toEqual(before);
    expect(tabPrefs().approval_mode).toBe("goal"); // 胶囊不回退到空值
  });

  it("Composer 权限胶囊随档位回落切换（目标模式 → 前档）", async () => {
    seed();
    serverPrefs({ approval_mode: "auto_edit" });
    useRun.setState({
      tabs: {
        s1: {
          goal: goal({ status: "executing" }), todos: [], goalRev: 0, items: [],
          subs: [], subStreams: {},
        },
      },
    } as any);
    render(
      <AntApp>
        <Composer />
      </AntApp>,
    );
    await waitFor(() => expect(document.querySelector(".approval-goal")).toBeTruthy());
    act(() => {
      useRun.getState().bindGlobalHandlers()["goal:update"]({
        session: "s1",
        goal: goal({ status: "done" }),
      });
    });
    await waitFor(() => expect(document.querySelector(".approval-auto")).toBeTruthy());
    expect(document.querySelector(".approval-goal")).toBeNull();
  });
});

describe("右栏「目标」段", () => {
  const renderBar = () =>
    render(
      <AntApp>
        <RightBar />
      </AntApp>,
    );

  it("渲染目标正文 / 达成标准勾选 / 账本摘要 / 状态徽标 / 轮次", async () => {
    seed();
    useRun.setState({ tabs: { s1: { goal: goal(), todos: [] } } } as any);
    renderBar();
    await screen.findByText("目标");
    expect(document.querySelector(".rb-goal-text")?.textContent).toContain("把登录改成 OAuth");
    expect(document.querySelector(".rb-goal-badge")?.textContent).toBe("已暂停");
    expect(document.querySelector(".rb-goal-head")?.textContent).toContain("已推进 3 轮");
    // 达成标准：已完成项划线、未完成项不划线（只读展示）
    expect(document.querySelector(".rb-todo-done")?.textContent).toBe("单测全绿");
    expect(screen.getByText("文档更新").className).not.toContain("rb-todo-done");
    // 账本摘要：路径数 + 程序列表
    expect(screen.getByText("路径 2 个")).toBeTruthy();
    expect(document.querySelector(".rb-goal-programs")?.textContent).toContain("cargo");
  });

  it("与「当前计划」段并列：两段同时存在、互不覆盖", async () => {
    seed();
    useRun.setState({
      tabs: { s1: { goal: goal(), todos: [{ title: "接入额度段", status: "in_progress" }] } },
    } as any);
    renderBar();
    await screen.findByText("目标");
    expect(screen.getByText("当前计划")).toBeTruthy();
    expect(screen.getByText("接入额度段")).toBeTruthy();
    expect(screen.getByText("文档更新")).toBeTruthy();
  });

  it("无目标时不渲染目标段（不做「折叠了不存在的段」）", async () => {
    seed();
    useRun.setState({ tabs: { s1: { goal: null, todos: [] } } } as any);
    renderBar();
    await screen.findByText("技能");
    expect(screen.queryByText("目标")).toBeNull();
  });

  it("暂停态出现「继续推进」，点击调 resume_goal", async () => {
    seed();
    useRun.setState({ tabs: { s1: { goal: goal({ status: "paused" }), todos: [], items: [] } } } as any);    renderBar();
    await screen.findByText("目标");
    fireEvent.click(screen.getByText("继续推进"));
    await waitFor(() =>
      expect(vi.mocked(invoke).mock.calls.some((c) => c[0] === "resume_goal")).toBe(true),
    );
  });

  it("执行中不出现「继续推进」，徽标为「执行中」", async () => {
    seed();
    useRun.setState({ tabs: { s1: { goal: goal({ status: "executing" }), todos: [] } } } as any);
    renderBar();
    await screen.findByText("目标");
    expect(screen.queryByText("继续推进")).toBeNull();
    expect(document.querySelector(".rb-goal-badge")?.textContent).toBe("执行中");
  });
});
