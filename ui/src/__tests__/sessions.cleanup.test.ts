// 会话保留期与清理 · 前端状态层（[docs/session-cleanup](../../docs/session-cleanup.md) §3 第 15/28 条）：
//   一、applyCleanup 的清理链路与顺序纪律（丢弃驻留内容 → 清运行态 → 关 Tab → 刷新列表，返回关掉几个 Tab）
//   二、restoreTabs 的「列表不可信」守卫（refresh 失败时不剔除快照 Tab；列表确实为空时照旧剔除）
//   三、refresh 的失败语义（不置空列表、只标记不可信、不抛错）
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）。
// 本文件另外包一层 utils/uiState 的 dropTabContent（真实实现 + 记序）：顺序纪律是这次改动的命门
// ——「先丢驻留内容、再关 Tab、最后刷新」一旦倒过来，已删会话的草稿就会写进快照并在下次启动复活（审查 E14）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_PREFS } from "../ipc/types";
import type { ProjectEntry, SessionMeta } from "../ipc/types";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { RestoredTab, Tab } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { applyRetainedContent, reset as resetUiState, retainTabContent } from "../utils/uiState";

const ipcMock = vi.hoisted(() => ({
  listSessions: vi.fn(async (): Promise<SessionMeta[]> => []),
  listProjects: vi.fn(async (): Promise<ProjectEntry[]> => []),
  setUiState: vi.fn(async (_state: unknown): Promise<void> => {}),
  getUiState: vi.fn(async (): Promise<unknown> => null),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

/** 清理链路的调用顺序记录（drop 由下面的模块包装写入，dispose/refresh 由各个用例写入） */
const seq = vi.hoisted(() => ({ order: [] as string[] }));

vi.mock("../utils/uiState", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../utils/uiState")>();
  return {
    ...actual,
    dropTabContent: (sessionId: string) => {
      seq.order.push(`drop:${sessionId}`);
      actual.dropTabContent(sessionId);
    },
  };
});

function meta(id: string, projectId: string | null = null): SessionMeta {
  return {
    id,
    title: id,
    workspace: `/tmp/${id}`,
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    last_opened_at: null,
    message_count: 0,
    project_id: projectId,
    roots: [`/tmp/${id}`],
    running: false,
    interrupted: null,
  };
}

function tab(id: string): Tab {
  return {
    key: id,
    sessionId: id,
    workspace: `/tmp/${id}`,
    title: id,
    projectId: null,
    createdAt: "2026-09-01T00:00:00Z",
    prefs: { ...DEFAULT_PREFS },
    loaded: true,
  };
}

/** 项目条目（只用到 id，其余字段占位） */
function proj(id: string): ProjectEntry {
  return { id, name: id, directory: `/tmp/${id}`, data_dir: null, created_at: "2026-09-01T00:00:00Z" };
}

/** 快照 Tab（projectId 可带：项目维度的失效引用剔除就靠它） */
function restored(id: string, projectId: string | null = null): RestoredTab {
  return {
    key: id,
    workspace: `/tmp/${id}`,
    title: id,
    projectId,
    createdAt: "2026-09-01T00:00:00Z",
    prefs: { ...DEFAULT_PREFS },
  };
}

function cleanStores(): void {
  useSessions.setState({
    tabs: [],
    activeKey: null,
    sessions: [],
    projects: [],
    unread: {},
    loading: false,
    sessionsLoadFailed: false,
    projectsLoadFailed: false,
  });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
  useUi.setState({ notifications: [], closeTabRequest: null });
}

beforeEach(() => {
  cleanStores();
  resetUiState();
  seq.order.length = 0;
  ipcMock.listSessions.mockReset().mockResolvedValue([]);
  ipcMock.listProjects.mockReset().mockResolvedValue([]);
  ipcMock.setUiState.mockReset().mockResolvedValue(undefined);
  ipcMock.getUiState.mockReset().mockResolvedValue(null);
});

afterEach(() => {
  cleanStores();
  // reset 放最后：把上面几次 setState 触发的（订阅链路）落盘防抖计时器一并清掉，测试间不串味
  resetUiState();
});

describe("applyCleanup：被清理会话的前端现场收尾", () => {
  it("关掉其 Tab、清运行态、刷新列表，并返回关闭的 Tab 数", async () => {
    useSessions.setState({
      sessions: [meta("s1"), meta("s2")],
      tabs: [tab("s1"), tab("s2")],
      activeKey: "s2",
    });
    useRun.getState().initTab("s1");
    useRun.getState().initTab("s2");
    ipcMock.listSessions.mockResolvedValue([meta("s2")]);

    const closed = useSessions.getState().applyCleanup(["s1"]);

    expect(closed).toBe(1);
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s2"]);
    // 运行态桶被清（迟到的检查点写入不会把已删会话在前端复活），未删会话的运行态原样保留
    expect(useRun.getState().tabs.s1).toBeUndefined();
    expect(useRun.getState().tabs.s2).toBeDefined();
    // 活跃 Tab 不在被清理之列 ⇒ 焦点不动
    expect(useSessions.getState().activeKey).toBe("s2");

    // 刷新列表（applyCleanup 按契约同步返回关闭数，refresh 不阻塞它）
    expect(ipcMock.listSessions).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(useSessions.getState().sessions.map((m) => m.id)).toEqual(["s2"]));
  });

  it("活跃 Tab 被清理时回落到相邻 Tab；一个不剩则空态", () => {
    useSessions.setState({
      sessions: [meta("s1"), meta("s2"), meta("s3")],
      tabs: [tab("s1"), tab("s2"), tab("s3")],
      activeKey: "s1",
    });
    expect(useSessions.getState().applyCleanup(["s1"])).toBe(1);
    expect(useSessions.getState().activeKey).toBe("s2"); // 相邻（剩下的第一个）

    expect(useSessions.getState().applyCleanup(["s2", "s3"])).toBe(2);
    expect(useSessions.getState().tabs).toEqual([]);
    expect(useSessions.getState().activeKey).toBeNull();
  });

  it("空列表直接返回 0，不做任何事（不误刷列表）", () => {
    useSessions.setState({ sessions: [meta("s1")], tabs: [tab("s1")], activeKey: "s1" });
    expect(useSessions.getState().applyCleanup([])).toBe(0);
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]);
    expect(ipcMock.listSessions).not.toHaveBeenCalled();
  });

  it("驻留内容随会话清掉：关 Tab 时选「保留」的草稿不会在重开时回填（未开 Tab 的会话同样清）", () => {
    useRun.getState().initTab("s1");
    useRun.getState().setDraftText("别留我", "s1");
    retainTabContent("s1"); // 关 Tab 时选「保留」：内容搬进 uiState 驻留表
    useRun.getState().dispose("s1"); // 运行态桶随关 Tab 删除，内容只剩驻留表
    useSessions.setState({
      sessions: [meta("s1"), meta("s2")],
      tabs: [tab("s2")],
      activeKey: "s2",
    });

    expect(useSessions.getState().applyCleanup(["s1"])).toBe(0); // 没开 Tab ⇒ 不关 Tab
    expect(seq.order).toContain("drop:s1");

    applyRetainedContent("s1"); // 驻留表已清 ⇒ 不回填（否则已删会话的草稿又回到输入框）
    expect(useRun.getState().drafts.s1).toBeUndefined();
    expect(useRun.getState().tabs.s1).toBeUndefined();
  });

  it("顺序纪律：丢驻留内容 → 清运行态 → 关 Tab → 最后才刷新列表", async () => {
    useSessions.setState({
      sessions: [meta("s1"), meta("s2")],
      tabs: [tab("s1"), tab("s2")],
      activeKey: "s2",
    });
    const realDispose = useRun.getState().dispose;
    useRun.getState().initTab("s1");
    const spy = vi
      .spyOn(useRun.getState(), "dispose")
      .mockImplementation((id: string) => {
        seq.order.push(`dispose:${id}`);
        realDispose(id);
      });
    // 刷新发生在最后：进 listSessions 时 Tab 条必须已经是不含被删会话的状态
    ipcMock.listSessions.mockImplementation(async () => {
      seq.order.push(`refresh:tabs=${useSessions.getState().tabs.length}`);
      return [meta("s2")];
    });

    expect(useSessions.getState().applyCleanup(["s1"])).toBe(1);
    expect(seq.order).toEqual(["drop:s1", "dispose:s1", "refresh:tabs=1"]);

    spy.mockRestore();
    await vi.waitFor(() => expect(ipcMock.listSessions).toHaveBeenCalled());
  });
});

describe("restoreTabs：列表不可信守卫（一次 IPC 抖动不清空工作现场）", () => {
  it("list_projects 失败 → 快照里带项目的 Tab 不被剔除（项目维度与会话维度同一口径）", async () => {
    // 会话列表本身是好的：剔不剔除只看项目列表这一路
    ipcMock.listSessions.mockResolvedValue([meta("s1", "p1")]);
    await useSessions.getState().refresh();
    // 项目列表从没成功加载过（手里这份是空的）且本次失败 → 不可信，不能按「项目已删」剔 Tab
    ipcMock.listProjects.mockRejectedValueOnce(new Error("ipc 抖动"));
    await expect(useSessions.getState().loadProjects()).resolves.toBeUndefined();
    expect(useSessions.getState().projectsLoadFailed).toBe(true);

    const kept = useSessions.getState().restoreTabs({
      tabs: [restored("s1", "p1")],
      activeKey: "s1",
      activeProject: "p1",
      unread: {},
    });

    expect(kept).toEqual(["s1"]);
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]);
  });

  it("list_projects 失败不置空列表，加载成功即复位「不可信」标记", async () => {
    ipcMock.listProjects.mockResolvedValueOnce([proj("p1")]);
    await useSessions.getState().loadProjects();
    expect(useSessions.getState().projects.map((p) => p.id)).toEqual(["p1"]);

    ipcMock.listProjects.mockRejectedValueOnce(new Error("ipc 抖动"));
    await useSessions.getState().loadProjects();
    expect(useSessions.getState().projectsLoadFailed).toBe(true);
    expect(useSessions.getState().projects.map((p) => p.id)).toEqual(["p1"]); // 旧写法会在这里变成空数组

    ipcMock.listProjects.mockResolvedValue([]);
    await useSessions.getState().loadProjects();
    expect(useSessions.getState().projectsLoadFailed).toBe(false);
  });

  it("项目列表确实为空（加载成功）→ 带已删项目的 Tab 照旧剔除，守卫不放大保活", async () => {
    ipcMock.listSessions.mockResolvedValue([meta("s1", "p1")]);
    await useSessions.getState().refresh();
    await useSessions.getState().loadProjects(); // 成功且为空 = 用户真的没有项目
    expect(useSessions.getState().projectsLoadFailed).toBe(false);

    const kept = useSessions.getState().restoreTabs({
      tabs: [restored("s1", "p1")],
      activeKey: "s1",
      activeProject: "p1",
      unread: {},
    });

    expect(kept).toEqual([]);
    expect(useSessions.getState().tabs).toEqual([]);
    expect(useSessions.getState().activeKey).toBeNull();
  });
  it("list_sessions 失败 → 不剔除快照 Tab，未读集合照旧保留", async () => {
    // 先有一次成功的加载：列表里有 s1
    ipcMock.listSessions.mockResolvedValueOnce([meta("s1")]);
    await useSessions.getState().refresh();
    expect(useSessions.getState().sessionsLoadFailed).toBe(false);

    // IPC 抖动：失败即标记不可信，且列表保持原值（旧写法会覆盖成空数组）
    ipcMock.listSessions.mockRejectedValueOnce(new Error("ipc 抖动"));
    await expect(useSessions.getState().refresh()).resolves.toBeUndefined();
    expect(useSessions.getState().sessionsLoadFailed).toBe(true);
    expect(useSessions.getState().sessions.map((m) => m.id)).toEqual(["s1"]);

    const kept = useSessions.getState().restoreTabs({
      tabs: [restored("s1"), restored("s2")],
      activeKey: "s2",
      activeProject: null,
      // ghost 不在（不可信的）列表里：守卫下未读集合也照旧保留（剔除了就丢）
      unread: { ghost: true, s2: true },
    });

    // 守卫：s2 虽不在（不可信的）列表里也必须留下；活跃 Tab 与未读都不动
    expect(kept).toEqual(["s1", "s2"]);
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1", "s2"]);
    expect(useSessions.getState().activeKey).toBe("s2");
    expect(useSessions.getState().unread.ghost).toBe(true);
    expect(useSessions.getState().unread.s2).toBeFalsy(); // 活跃会话的未读由既有不变式清零
  });

  it("列表成功加载且确实不含某会话 → 照旧剔除该项", async () => {
    ipcMock.listSessions.mockResolvedValue([meta("s1")]);
    await useSessions.getState().refresh();

    const kept = useSessions.getState().restoreTabs({
      tabs: [restored("s1"), restored("gone")],
      activeKey: "gone",
      activeProject: null,
      unread: { gone: true },
    });

    expect(kept).toEqual(["s1"]);
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]);
    expect(useSessions.getState().activeKey).toBe("s1"); // 失效的活跃 Tab 回落邻位
    expect(useSessions.getState().unread.gone).toBeUndefined();
  });

  it("列表确实为空（用户真的没有会话）→ 仍然剔除全部，守卫不放大保活", () => {
    // 没有任何失败记录（默认 = 可信）：空列表就是「确实没有会话」
    useSessions.setState({ sessions: [] });

    const kept = useSessions.getState().restoreTabs({
      tabs: [restored("s1"), restored("s2")],
      activeKey: "s1",
      activeProject: null,
      unread: { s1: true },
    });

    expect(kept).toEqual([]);
    expect(useSessions.getState().tabs).toEqual([]);
    expect(useSessions.getState().activeKey).toBeNull();
  });

  it("刷新成功后复位「不可信」标记：下一次恢复照旧做失效引用剔除", async () => {
    ipcMock.listSessions.mockRejectedValueOnce(new Error("抖动"));
    await useSessions.getState().refresh();
    expect(useSessions.getState().sessionsLoadFailed).toBe(true);

    ipcMock.listSessions.mockResolvedValue([meta("s1")]);
    await useSessions.getState().refresh();
    expect(useSessions.getState().sessionsLoadFailed).toBe(false);

    const kept = useSessions
      .getState()
      .restoreTabs({ tabs: [restored("s1"), restored("gone")], activeKey: "s1", activeProject: null, unread: {} });
    expect(kept).toEqual(["s1"]);
  });
});

describe("refresh 的失败语义", () => {
  it("失败不抛错、不置空列表（调用方行为不变）", async () => {
    useSessions.setState({ sessions: [meta("s1"), meta("s2")] });
    ipcMock.listSessions.mockRejectedValue(new Error("后端还没起"));
    await expect(useSessions.getState().refresh()).resolves.toBeUndefined();
    expect(useSessions.getState().sessions.map((m) => m.id)).toEqual(["s1", "s2"]);
    expect(useSessions.getState().sessionsLoadFailed).toBe(true);

    // 失败后左栏仍能用上一份列表渲染，恢复也不剔除 Tab
    const kept = useSessions
      .getState()
      .restoreTabs({ tabs: [restored("s1"), restored("s2")], activeKey: "s1", activeProject: null, unread: {} });
    expect(kept).toEqual(["s1", "s2"]);
  });
});
