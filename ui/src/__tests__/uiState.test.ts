// ui-state 现场态持久化（会话保存与恢复优化 · 批1）：
// 结构归一 / 落盘→读回往返 / 失效引用剔除 / 关 Tab 内容保留与丢弃 / 防抖与最长等待落盘 /
// 窗口几何单位换算 / 退出应答 / 订阅覆盖。
// 项目惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock（不直接 mock @tauri-apps/api/core）；
// 窗口 API 按既有测试的做法 mock @tauri-apps/api/window。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_PREFS } from "../ipc/types";
import type { ProjectEntry, SessionMeta } from "../ipc/types";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import {
  FLUSH_DEBOUNCE_MS,
  FLUSH_MAX_WAIT_MS,
  UI_STATE_SCHEMA,
  applyRetainedContent,
  applyUiStateToStores,
  buildSnapshot,
  dropTabContent,
  flushNow,
  getScrollAnchor,
  getTreeCollapsed,
  getTreeExpanded,
  initUiStatePersistence,
  loadUiState,
  normalizeUiState,
  reset,
  respondExitRequest,
  retainTabContent,
  scheduleAnchor,
  scheduleFlush,
  setScrollAnchor,
  setTreeCollapsed,
  setTreeExpanded,
  tabHasPendingContent,
} from "../utils/uiState";
import type { UiState, UiTabSnapshot } from "../utils/uiState";

const ipcMock = vi.hoisted(() => ({
  getUiState: vi.fn(async (): Promise<unknown> => null),
  setUiState: vi.fn(async (_state: unknown): Promise<void> => {}),
  resolveExitRequest: vi.fn(async (_action: string): Promise<void> => {}),
  // 会话删除链路（E14 用例：removeSession / deleteProject / refresh 都要过 ipc）
  listSessions: vi.fn(async (): Promise<SessionMeta[]> => []),
  deleteSession: vi.fn(async (_sessionId: string): Promise<void> => {}),
  deleteProject: vi.fn(async (_id: string): Promise<{ deleted_sessions: number }> => ({ deleted_sessions: 0 })),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

/** 窗口 API 的可调桩：available=false 模拟非 Tauri 运行时（纯浏览器/测试） */
const winApi = vi.hoisted(() => ({
  available: true,
  scale: 2,
  pos: { x: 100, y: 200 },
  size: { width: 1600, height: 1200 },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => {
    if (!winApi.available) throw new Error("not running in a Tauri window");
    return {
      scaleFactor: async () => winApi.scale,
      outerPosition: async () => winApi.pos,
      innerSize: async () => winApi.size,
    };
  },
}));

const prefs = { ...DEFAULT_PREFS };

function meta(id: string, projectId: string | null = null): SessionMeta {
  return {
    id,
    title: id,
    workspace: `/tmp/${id}`,
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    message_count: 0,
    project_id: projectId,
    roots: [`/tmp/${id}`],
    running: false,
    interrupted: null,
  };
}

function project(id: string): ProjectEntry {
  return { id, name: id, directory: `/tmp/${id}`, data_dir: null, created_at: "2026-09-01T00:00:00Z" };
}

/** 铺一个会话 + 已打开 Tab（key === sessionId） */
function seedTab(key: string, projectId: string | null = null): void {
  useSessions.setState((s) => ({
    sessions: [...s.sessions, meta(key, projectId)],
    tabs: [
      ...s.tabs,
      { key, sessionId: key, workspace: `/tmp/${key}`, title: key, projectId, createdAt: "2026-09-01T00:00:00Z", prefs },
    ],
  }));
}

function seedProject(id: string): void {
  useSessions.setState((s) => ({ projects: [...s.projects, project(id)] }));
}

/** 关 Tab（仅从 Tab 条移除，运行态桶由调用方决定是否 dispose） */
function closeTab(key: string): void {
  useSessions.setState((s) => ({
    tabs: s.tabs.filter((t) => t.key !== key),
    activeKey: s.activeKey === key ? null : s.activeKey,
  }));
}

/** 落盘链路上有若干 await（动态 import + invoke）：推进计时器后把微任务与零延迟计时器都走干净 */
async function advance(ms: number): Promise<void> {
  await vi.advanceTimersByTimeAsync(ms);
  for (let i = 0; i < 10; i++) await Promise.resolve();
  await vi.advanceTimersByTimeAsync(0);
  for (let i = 0; i < 10; i++) await Promise.resolve();
}

/** 反复让出微任务直到条件成立（避开对内部 await 次数做假设） */
async function until(cond: () => boolean, turns = 80): Promise<void> {
  for (let i = 0; i < turns && !cond(); i++) await Promise.resolve();
  if (!cond()) throw new Error("waitUntil: 条件未在预期微任务轮数内成立");
}

function cleanStores(): void {
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {}, loading: false });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
  useUi.setState({ notifications: [], exitRequest: null, closeTabRequest: null });
}

beforeEach(() => {
  reset();
  cleanStores();
  ipcMock.getUiState.mockReset().mockResolvedValue(null);
  ipcMock.setUiState.mockReset().mockResolvedValue(undefined);
  ipcMock.resolveExitRequest.mockReset().mockResolvedValue(undefined);
  ipcMock.listSessions.mockReset().mockResolvedValue([]);
  ipcMock.deleteSession.mockReset().mockResolvedValue(undefined);
  ipcMock.deleteProject.mockReset().mockResolvedValue({ deleted_sessions: 0 });
  winApi.available = true;
  winApi.scale = 2;
  winApi.pos = { x: 100, y: 200 };
  winApi.size = { width: 1600, height: 1200 };
});

afterEach(() => {
  vi.useRealTimers();
  // reset 放最后：把上面几次 setState 触发的（订阅链路）防抖计时器一并清掉，测试间不串味
  cleanStores();
  reset();
});

describe("normalizeUiState 结构归一", () => {
  it("非对象 / 数组 / 缺 schema / schema 版本不符 → null（无快照启动）", () => {
    expect(normalizeUiState(null)).toBeNull();
    expect(normalizeUiState("x")).toBeNull();
    expect(normalizeUiState([])).toBeNull();
    expect(normalizeUiState({})).toBeNull();
    expect(normalizeUiState({ schema: UI_STATE_SCHEMA + 1, tabs: { order: [], items: {} } })).toBeNull();
    expect(normalizeUiState({ schema: "1", tabs: { order: [], items: {} } })).toBeNull();
  });

  it("tabs 结构损坏（order 非数组 / items 缺失）→ null", () => {
    expect(normalizeUiState({ schema: UI_STATE_SCHEMA })).toBeNull();
    expect(normalizeUiState({ schema: UI_STATE_SCHEMA, tabs: { items: {} } })).toBeNull();
    expect(normalizeUiState({ schema: UI_STATE_SCHEMA, tabs: { order: [], items: [] } })).toBeNull();
  });

  it("未知多余字段不炸、也不透传（向前兼容靠归一兜）", () => {
    const st = normalizeUiState({
      schema: UI_STATE_SCHEMA,
      tabs: { order: [], items: {}, future: 1 },
      somethingNew: { deep: [1, 2] },
    });
    expect(st).not.toBeNull();
    expect(Object.keys(st!)).not.toContain("somethingNew");
    expect(st!.tabs.order).toEqual([]);
  });

  it("子结构缺失 → 安全默认（空表 + null 活跃项目 + 无窗口）", () => {
    const st = normalizeUiState({ schema: UI_STATE_SCHEMA, tabs: { order: [], items: {} } })!;
    expect(st.activeProject).toBeNull();
    expect(st.window).toBeNull();
    expect(st.scrollAnchors).toEqual({});
    expect(st.drafts).toEqual({});
    expect(st.queue).toEqual({});
    expect(st.panels).toEqual({});
    expect(st.tree).toEqual({ expanded: {}, collapsed: false, unread: {} });
  });

  it("item 类型不符（workspace 非字符串）→ 该 Tab 与 order 一并剔除；缺字段按默认补齐", () => {
    const st = normalizeUiState({
      schema: UI_STATE_SCHEMA,
      tabs: {
        order: ["ok", "bad", "missing"],
        items: { ok: { workspace: "/tmp/ok" }, bad: { workspace: 42 } },
        activeKey: "bad",
      },
    })!;
    expect(st.tabs.order).toEqual(["ok"]);
    expect(Object.keys(st.tabs.items)).toEqual(["ok"]);
    expect(st.tabs.items.ok).toEqual({
      workspace: "/tmp/ok",
      title: "",
      projectId: null,
      createdAt: "",
      prefs: DEFAULT_PREFS,
    });
  });

  it("prefs 类型不符 → 回落 DEFAULT_PREFS", () => {
    const st = normalizeUiState({
      schema: UI_STATE_SCHEMA,
      tabs: { order: ["ok"], items: { ok: { workspace: "/tmp/ok", prefs: "auto" } } },
    })!;
    expect(st.tabs.items.ok.prefs).toEqual(DEFAULT_PREFS);
  });

  it("window 非法数值（缺宽 / 类型错 / 0 / 负）→ null；位置不成对 → 只留尺寸", () => {
    const withWin = (w: unknown) =>
      normalizeUiState({ schema: UI_STATE_SCHEMA, tabs: { order: [], items: {} }, window: w })!.window;
    expect(withWin({ height: 800 })).toBeNull();
    expect(withWin({ width: "1200", height: 800 })).toBeNull();
    expect(withWin({ width: 0, height: 800 })).toBeNull();
    expect(withWin({ width: 1200, height: -1 })).toBeNull();
    expect(withWin({ width: 1200, height: 800 })).toEqual({ width: 1200, height: 800 });
    expect(withWin({ width: 1200, height: 800, x: 10, y: 20 })).toEqual({ width: 1200, height: 800, x: 10, y: 20 });
    expect(withWin({ width: 1200, height: 800, x: 10 })).toEqual({ width: 1200, height: 800 });
  });
});

describe("快照往返（buildSnapshot → 磁盘 → loadUiState → 落 store）", () => {
  it("Tab 骨架 / 活跃 Tab / 活跃项目 / 未读 / 锚点 / 树展开 / 面板态 全部来回一致", async () => {
    seedProject("p1");
    seedTab("s1", "p1");
    seedTab("s2", "p1");
    useSessions.setState({ activeKey: "s1", unread: { s2: true } });
    useRun.getState().initTab("s1");
    useRun.getState().initTab("s2");
    useRun.getState().setDraftText("半段草稿", "s2");
    useRun.setState((s) => {
      s.tabs.s2.queue = [{ id: "q1", text: "排队任务", images: [{ mime: "image/png", data: "AAA" }] }];
      s.tabs.s1.todos = [{ title: "跑测试", status: "in_progress" }];
      s.tabs.s1.subDrawer = { open: true, subId: "sub-1" };
    });
    setScrollAnchor("s1", { kind: "item", idx: 2, sig: "u:1:3:abc", offset: 12 });
    setTreeExpanded({ p1: true });

    const snap = await buildSnapshot();
    expect(snap.schema).toBe(UI_STATE_SCHEMA);
    expect(snap.tabs.order).toEqual(["s1", "s2"]);
    expect(snap.tabs.activeKey).toBe("s1");
    expect(snap.tabs.items.s2.projectId).toBe("p1");
    expect(snap.activeProject).toBe("p1");
    expect(snap.tree.unread).toEqual({ s2: true });
    expect(snap.drafts.s2).toEqual({ text: "半段草稿", images: [] });
    expect(snap.queue.s2[0]).toEqual({ id: "q1", text: "排队任务", images: [{ mime: "image/png", data: "AAA" }] });
    expect(snap.panels.s1.todos).toEqual([{ title: "跑测试", status: "in_progress" }]);
    expect(snap.panels.s1.subDrawer).toEqual({ open: true, subId: "sub-1" });
    expect(snap.scrollAnchors.s1).toEqual({ kind: "item", idx: 2, sig: "u:1:3:abc", offset: 12 });
    expect(snap.tree.expanded).toEqual({ p1: true });
    expect(snap.window).toEqual({ x: 50, y: 100, width: 800, height: 600 });

    // 磁盘往返：JSON 序列化后归一，结构必须原样回来（缺失/多余字段都由归一兜住）
    const onDisk: unknown = JSON.parse(JSON.stringify(snap));
    expect(normalizeUiState(onDisk)).toEqual(snap);

    // 重启：先清内存态与 store，再把读回的快照落到 store
    reset();
    cleanStores();
    useSessions.setState({ sessions: [meta("s1", "p1"), meta("s2", "p1")], projects: [project("p1")] });
    ipcMock.getUiState.mockResolvedValue(onDisk);
    const loaded = await loadUiState();
    expect(loaded).not.toBeNull();
    const applied = applyUiStateToStores();

    const sessions = useSessions.getState();
    expect(applied).toEqual({ activeKey: "s1", kept: ["s1", "s2"] });
    expect(sessions.tabs.map((t) => t.key)).toEqual(["s1", "s2"]);
    expect(sessions.tabs.every((t) => t.loaded === false)).toBe(true); // 骨架：消息按需拉
    expect(sessions.activeKey).toBe("s1");
    expect(sessions.unread).toEqual({ s2: true });
    expect(getTreeExpanded()).toEqual({ p1: true });
    expect(getScrollAnchor("s1")).toEqual({ kind: "item", idx: 2, sig: "u:1:3:abc", offset: 12 });
    // 草稿/队列/面板按 Tab 打开时机回填
    expect(useRun.getState().drafts.s2.text).toBe("半段草稿");
    expect(useRun.getState().tabs.s2.queue[0].text).toBe("排队任务");
    expect(useRun.getState().tabs.s1.todos[0].title).toBe("跑测试");
    expect(useRun.getState().tabs.s1.subDrawer).toEqual({ open: true, subId: "sub-1" });
    expect(Object.keys(useRun.getState().tabs).sort()).toEqual(["s1", "s2"]); // 不为未恢复的会话建桶
  });

  it("getUiState 读失败 → null（不阻断启动，不抛错）", async () => {
    ipcMock.getUiState.mockRejectedValue(new Error("disk gone"));
    await expect(loadUiState()).resolves.toBeNull();
    expect(applyUiStateToStores()).toEqual({ activeKey: null, kept: [] });
  });
});

describe("失效引用剔除（与 sessions.restoreTabs 协作）", () => {
  /** 造一份 raw 快照（patch 覆盖顶层键）。items 的项故意允许只给部分字段：
   *  磁盘上的快照可能来自更早/更粗的写入，把缺失字段补齐正是归一的职责（所以这里不能按 UiTabSnapshot 严检） */
  function loadState(patch: {
    tabs?: { order: string[]; activeKey: string | null; items: Record<string, Partial<UiTabSnapshot>> };
    activeProject?: string | null;
    tree?: { expanded: Record<string, boolean>; collapsed: boolean; unread: Record<string, boolean> };
  }): unknown {
    const base: UiState = {
      schema: UI_STATE_SCHEMA,
      tabs: { order: [], activeKey: null, items: {} },
      activeProject: null,
      window: null,
      scrollAnchors: {},
      drafts: {},
      queue: {},
      tree: { expanded: {}, collapsed: false, unread: {} },
      panels: {},
    };
    return { ...base, ...patch };
  }

  it("会话已删 → 静默剔除（不抛错、不建空 Tab）", async () => {
    seedTab("s1");
    ipcMock.getUiState.mockResolvedValue(
      loadState({
        tabs: {
          order: ["s1", "gone"],
          activeKey: "s1",
          items: { s1: { workspace: "/tmp/s1" }, gone: { workspace: "/tmp/gone" } },
        },
      }),
    );
    await loadUiState();
    const applied = applyUiStateToStores();
    expect(applied).toEqual({ activeKey: "s1", kept: ["s1"] });
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]);
  });

  it("项目已删 → 该项目下 Tab 一并剔除（会话还在也不留）", async () => {
    seedTab("s1", "pGone");
    ipcMock.getUiState.mockResolvedValue(
      loadState({
        tabs: { order: ["s1"], activeKey: "s1", items: { s1: { workspace: "/tmp/s1", projectId: "pGone" } } },
        activeProject: "pGone",
      }),
    );
    await loadUiState();
    const applied = applyUiStateToStores();
    expect(applied).toEqual({ activeKey: null, kept: [] });
    expect(useSessions.getState().tabs).toHaveLength(0);
    expect(useRun.getState().tabs).toEqual({}); // 全被剔除 ⇒ 不建运行态桶
  });

  it("活跃 Tab 被剔除 → 回落相邻 Tab", async () => {
    seedTab("s1");
    seedTab("s3");
    ipcMock.getUiState.mockResolvedValue(
      loadState({
        tabs: {
          order: ["s1", "gone", "s3"],
          activeKey: "gone",
          items: { s1: { workspace: "/tmp/s1" }, gone: { workspace: "/tmp/gone" }, s3: { workspace: "/tmp/s3" } },
        },
      }),
    );
    await loadUiState();
    const applied = applyUiStateToStores();
    expect(applied.kept).toEqual(["s1", "s3"]);
    // 回落邻位，且返回值必须与 store 实际值一致（否则调用方会急加载错的 Tab）：
    // restoreTabs 按「原活跃 Tab 在 Tab 条里的位置」取邻位，剔除 gone 后占据该位置的是 s3
    expect(applied.activeKey).toBe(useSessions.getState().activeKey);
    expect(applied.activeKey).toBe("s3");
  });

  it("活跃 Tab 被剔除时同项目邻位优先", async () => {
    seedProject("p1");
    seedTab("s1");
    seedTab("s2");
    seedTab("s3", "p1");
    ipcMock.getUiState.mockResolvedValue(
      loadState({
        tabs: {
          order: ["s1", "s2", "s3"],
          activeKey: "gone",
          items: {
            s1: { workspace: "/tmp/s1" },
            s2: { workspace: "/tmp/s2" },
            s3: { workspace: "/tmp/s3", projectId: "p1" },
          },
        },
        activeProject: "p1",
      }),
    );
    await loadUiState();
    const applied = applyUiStateToStores();
    expect(applied.kept).toEqual(["s1", "s2", "s3"]);
    // 同项目邻位（s3 ∈ p1）胜过全局邻位（s1）；返回值与 store 一致
    expect(applied.activeKey).toBe(useSessions.getState().activeKey);
    expect(applied.activeKey).toBe("s3");
  });

  it("未读集合只保留仍存在的会话；活跃会话的未读被不变式清掉", async () => {
    seedTab("s1");
    seedTab("s2");
    ipcMock.getUiState.mockResolvedValue(
      loadState({
        tabs: {
          order: ["s1", "s2"],
          activeKey: "s1",
          items: { s1: { workspace: "/tmp/s1" }, s2: { workspace: "/tmp/s2" } },
        },
        tree: { expanded: {}, collapsed: true, unread: { s1: true, s2: true, gone: true } },
      }),
    );
    await loadUiState();
    applyUiStateToStores();
    // 未读恢复的是上次快照（不是启动全标未读）：已删会话的条目被剔除；活跃会话由 store 不变式清掉
    const unread = useSessions.getState().unread;
    expect(unread.s2).toBe(true);
    expect(unread.s1).toBeFalsy(); // 活跃会话的未读被清（键可能保留为 false）
    expect(unread.gone).toBeUndefined();
    expect(getTreeCollapsed()).toBe(true);
  });
});

describe("关 Tab：内容保留 / 丢弃 / 回填", () => {
  it("tabHasPendingContent：草稿文本、附件、前端队列都算未发送内容", () => {
    seedTab("s1");
    useRun.getState().initTab("s1");
    expect(tabHasPendingContent("s1")).toBe(false);
    useRun.getState().setDraftText("   ", "s1");
    expect(tabHasPendingContent("s1")).toBe(false); // 纯空白不算
    useRun.getState().setDraftText("hello", "s1");
    expect(tabHasPendingContent("s1")).toBe(true);
    useRun.getState().clearDraft("s1");
    useRun.getState().setDraftImages(
      [{ id: "i1", name: "a.png", mime: "image/png", data: "AAA", dataUrl: "data:image/png;base64,AAA" }],
      "s1",
    );
    expect(tabHasPendingContent("s1")).toBe(true); // 只有附件也算
    useRun.getState().clearDraft("s1");
    useRun.setState((s) => {
      s.tabs.s1.queue = [{ id: "q1", text: "排队" }];
    });
    expect(tabHasPendingContent("s1")).toBe(true); // 前端队列也算
  });

  it("选择保留：内容搬进驻留表 → Tab 已关仍落盘 → 重开回填", async () => {
    seedTab("s1");
    useRun.getState().initTab("s1");
    useRun.getState().setDraftText("别丢了我", "s1");
    useRun.getState().setDraftImages(
      [{ id: "i1", name: "a.png", mime: "image/png", data: "AAA", dataUrl: "data:image/png;base64,AAA" }],
      "s1",
    );
    useRun.setState((s) => {
      s.tabs.s1.queue = [{ id: "q1", text: "排队任务" }];
      s.tabs.s1.suggestions = ["接着问一句"];
    });

    retainTabContent("s1");
    closeTab("s1");
    useRun.getState().dispose("s1"); // 关 Tab：运行态桶被删，内容只剩驻留表

    const snap = await buildSnapshot();
    expect(snap.tabs.order).toEqual([]); // Tab 已不在 Tab 条
    expect(snap.drafts.s1.text).toBe("别丢了我");
    expect(snap.drafts.s1.images[0].data).toBe("AAA");
    expect(snap.queue.s1[0].text).toBe("排队任务");
    expect(snap.panels.s1.suggestions).toEqual(["接着问一句"]);

    // 重开该会话：草稿（缩略图按 mime+data 重建）、队列、面板原样回来
    applyRetainedContent("s1");
    const run = useRun.getState();
    expect(run.drafts.s1.text).toBe("别丢了我");
    expect(run.drafts.s1.images[0].dataUrl).toBe("data:image/png;base64,AAA");
    expect(run.tabs.s1.queue[0].text).toBe("排队任务");
    expect(run.tabs.s1.suggestions).toEqual(["接着问一句"]);

    // 幂等：驻留表已清空，再回填不覆盖用户新输入
    run.setDraftText("新输入", "s1");
    applyRetainedContent("s1");
    expect(useRun.getState().drafts.s1.text).toBe("新输入");
  });

  it("选择丢弃：驻留内容清空，快照不再包含该项", async () => {
    seedTab("s1");
    useRun.getState().initTab("s1");
    useRun.getState().setDraftText("要丢的草稿", "s1");
    retainTabContent("s1");
    dropTabContent("s1");
    closeTab("s1");
    useRun.getState().dispose("s1");

    const snap = await buildSnapshot();
    expect(snap.drafts).toEqual({});
    expect(snap.queue).toEqual({});
    applyRetainedContent("s1");
    expect(useRun.getState().drafts.s1).toBeUndefined(); // 无驻留内容 ⇒ 不回填、不建草稿桶
  });
});

describe("滚动锚点 / 左栏树状态", () => {
  it("锚点只落盘仍打开的会话（关掉的 Tab 不留孤儿锚点）", async () => {
    seedTab("s1");
    setScrollAnchor("s1", { kind: "bottom" });
    setScrollAnchor("closed", { kind: "item", idx: 0, sig: "u", offset: 5 });
    const snap = await buildSnapshot();
    expect(snap.scrollAnchors).toEqual({ s1: { kind: "bottom" } });
    expect(getScrollAnchor("closed")).toEqual({ kind: "item", idx: 0, sig: "u", offset: 5 }); // 内存仍在，只是不落盘
    setScrollAnchor("s1", null);
    expect(getScrollAnchor("s1")).toBeNull();
  });

  it("scheduleAnchor 防抖：窗口内的重复调用只记一次；reader 抛错不阻塞后续记录", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    // 防抖窗口内的后续调用直接丢弃（滚动高频回调不反复读布局），落盘的是首个 reader 的结果
    scheduleAnchor("s1", () => ({ kind: "item", idx: 1, sig: "a", offset: 1 }));
    scheduleAnchor("s1", () => ({ kind: "item", idx: 2, sig: "b", offset: 2 }));
    await advance(200);
    expect(getScrollAnchor("s1")).toEqual({ kind: "item", idx: 1, sig: "a", offset: 1 });

    // reader 抛错（容器已卸载）：不把异常抛进事件循环，且防抖标志复位后仍能继续记录
    scheduleAnchor("s1", () => {
      throw new Error("container unmounted");
    });
    await advance(200);
    scheduleAnchor("s1", () => ({ kind: "bottom" }));
    await advance(200);
    expect(getScrollAnchor("s1")).toEqual({ kind: "bottom" });
  });

  it("树展开/折叠落盘；getTreeExpanded 返回副本（外部改不动模块内状态）", async () => {
    setTreeExpanded({ p1: true });
    const copy = getTreeExpanded();
    delete copy.p1;
    expect(getTreeExpanded()).toEqual({ p1: true });
    setTreeCollapsed(true);
    const snap = await buildSnapshot();
    expect(snap.tree.expanded).toEqual({ p1: true });
    expect(snap.tree.collapsed).toBe(true);
  });
});

describe("落盘：防抖 / 最长等待 / 失败不静默", () => {
  it("1.2s 内连续变更只落盘一次；防抖期满写入", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    scheduleFlush();
    await advance(600);
    scheduleFlush(); // 又来一次变更：重置防抖
    await advance(600);
    expect(ipcMock.setUiState).not.toHaveBeenCalled();
    await advance(FLUSH_DEBOUNCE_MS);
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);
  });

  it("流式持续变更下仍按时落盘（最长等待不被防抖无限推迟）", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    useRun.getState().initTab("s1");
    let firstWriteAt = -1;
    // 模拟流式：每 200ms 一次变更，持续 3 秒（纯防抖会把计时器一直往后推 → 一次都不落盘）
    for (let i = 1; i <= 15; i++) {
      useRun.getState().setDraftText(`token-${i}`, "s1");
      scheduleFlush();
      await advance(200);
      if (firstWriteAt < 0 && ipcMock.setUiState.mock.calls.length > 0) firstWriteAt = i * 200;
    }
    expect(firstWriteAt).toBeGreaterThan(0);
    expect(firstWriteAt).toBeLessThanOrEqual(FLUSH_MAX_WAIT_MS);
  });

  it("flushNow 立即落盘并清掉防抖计时器（不会二次重写）", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    scheduleFlush();
    await flushNow();
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);
    await advance(FLUSH_MAX_WAIT_MS + 1000);
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1); // 计时器已清 + 内容未变跳过
  });

  it("内容未变不重复写盘（避免空转 I/O）", async () => {
    seedTab("s1");
    await flushNow();
    await flushNow();
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);
  });

  it("写盘失败不静默：上报 toast，且不更新 lastWritten ⇒ 下次落盘会重试", async () => {
    seedTab("s1");
    ipcMock.setUiState.mockRejectedValueOnce(new Error("磁盘只读"));
    await flushNow();
    expect(
      useUi.getState().notifications.some((n) => n.body.includes("界面状态保存失败") && n.body.includes("磁盘只读")),
    ).toBe(true);
    await flushNow();
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(2);
  });

  it("并发 flushNow 串行化：在途期间的变更不交错，补写拿到的是最新现场态", async () => {
    seedTab("s1");
    useRun.getState().initTab("s1");
    const writes: unknown[] = [];
    const resolvers: (() => void)[] = [];
    ipcMock.setUiState.mockImplementation(
      (state: unknown) =>
        new Promise<void>((resolve) => {
          writes.push(state);
          resolvers.push(resolve);
        }),
    );

    const first = flushNow();
    await until(() => writes.length === 1);
    useRun.getState().setDraftText("在途期间的新草稿", "s1");
    const second = flushNow();
    expect(writes.length).toBe(1); // 在途落盘不因并发调用而交错出第二个快照

    resolvers.shift()!(); // 放行第一次写
    await until(() => writes.length === 2); // 补写启动，快照为最新现场态
    resolvers.shift()!();
    await second;
    expect((writes[1] as UiState).drafts.s1.text).toBe("在途期间的新草稿");
    await first; // 两次调用拿到的是同一个串行链
  });
});

describe("窗口几何（写入逻辑像素）", () => {
  it("物理像素 ÷ scale → 逻辑像素（HiDPI 下不写双倍尺寸）", async () => {
    const snap = await buildSnapshot();
    expect(snap.window).toEqual({ x: 50, y: 100, width: 800, height: 600 });
  });

  it("scale 非法（0 / 非数）时按 1 处理，仍然写入可用几何", async () => {
    winApi.scale = 0;
    expect((await buildSnapshot()).window).toEqual({ x: 100, y: 200, width: 1600, height: 1200 });
    winApi.scale = Number.NaN;
    expect((await buildSnapshot()).window).toEqual({ x: 100, y: 200, width: 1600, height: 1200 });
  });

  it("无 Tauri 运行时 / 窗口 API 失败 → 不写几何也不抛错", async () => {
    winApi.available = false;
    await expect(buildSnapshot()).resolves.toMatchObject({ window: null });
  });

  it("读取失败回退上次成功值（最小化尺寸为 0 也不抹掉已记下的几何）", async () => {
    expect((await buildSnapshot()).window).toEqual({ x: 50, y: 100, width: 800, height: 600 });
    winApi.size = { width: 0, height: 0 };
    expect((await buildSnapshot()).window).toEqual({ x: 50, y: 100, width: 800, height: 600 });
    winApi.size = { width: 1440, height: 900 };
    expect((await buildSnapshot()).window).toEqual({ x: 50, y: 100, width: 720, height: 450 });
  });
});

describe("退出拦截应答", () => {
  it("四个动作都先落盘再原样透传 resolve_exit_request", async () => {
    seedTab("s1");
    const order: string[] = [];
    ipcMock.setUiState.mockImplementation(async () => {
      order.push("flush");
    });
    ipcMock.resolveExitRequest.mockImplementation(async (action: string) => {
      order.push(`resolve:${action}`);
    });
    for (const action of ["exit", "abort", "wait", "cancel"] as const) {
      order.length = 0;
      // 每次改一处现场态：内容未变时 flushNow 会跳过写盘（去重），这里要观察的是「先落盘再应答」的顺序
      useSessions.getState().markUnread(action);
      await respondExitRequest(action);
      expect(order).toEqual(["flush", `resolve:${action}`]);
      expect(ipcMock.resolveExitRequest).toHaveBeenLastCalledWith(action);
    }
  });

  it("resolve 失败（通道已随退出关闭）不抛未捕获异常", async () => {
    seedTab("s1");
    ipcMock.resolveExitRequest.mockRejectedValue(new Error("channel closed"));
    await expect(respondExitRequest("exit")).resolves.toBeUndefined();
  });
});

describe("initUiStatePersistence 订阅覆盖与 reset", () => {
  it("tabs/未读（sessions）与草稿/队列（run）的变更都触发防抖落盘", async () => {
    vi.useFakeTimers();
    seedProject("p1");
    initUiStatePersistence();
    initUiStatePersistence(); // 幂等：重复调用不重复订阅

    seedTab("s1", "p1");
    await advance(FLUSH_DEBOUNCE_MS);
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);

    useSessions.getState().markUnread("s1");
    await advance(FLUSH_DEBOUNCE_MS);
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(2);
    expect((ipcMock.setUiState.mock.calls[1][0] as UiState).tree.unread).toEqual({ s1: true });

    useRun.getState().initTab("s1");
    useRun.getState().setDraftText("边写边存", "s1");
    await advance(FLUSH_DEBOUNCE_MS);
    expect((ipcMock.setUiState.mock.calls.at(-1)![0] as UiState).drafts.s1.text).toBe("边写边存");

    useRun.setState((s) => {
      s.tabs.s1.queue = [{ id: "q1", text: "排队" }];
    });
    await advance(FLUSH_DEBOUNCE_MS);
    expect((ipcMock.setUiState.mock.calls.at(-1)![0] as UiState).queue.s1[0].text).toBe("排队");
  });

  it("退出请求到达立即落盘（不等防抖：后端无 run 在跑时只给 2 秒应答窗口）", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    initUiStatePersistence();
    ipcMock.setUiState.mockClear();
    useUi.setState({ exitRequest: { running: ["s1"] } });
    await advance(0); // 不推进防抖时长
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);
  });

  it("reset 清空内存缓存与计时器（测试间不串味），之后可重新落盘", async () => {
    vi.useFakeTimers();
    seedTab("s1");
    useRun.getState().initTab("s1");
    useRun.getState().setDraftText("草稿", "s1");
    setScrollAnchor("s1", { kind: "bottom" });
    setTreeExpanded({ p1: true });
    setTreeCollapsed(true);
    scheduleFlush(); // 挂上计时器，reset 必须清掉
    await flushNow();
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(1);

    reset();
    expect(getScrollAnchor("s1")).toBeNull();
    expect(getTreeExpanded()).toEqual({});
    expect(getTreeCollapsed()).toBe(false);
    const snap = await buildSnapshot();
    expect(snap.scrollAnchors).toEqual({});
    expect(snap.tree.expanded).toEqual({});
    // lastWritten 也被清空：内容未变也允许重新落盘（测试不会拿到上一轮的去重状态）
    await flushNow();
    expect(ipcMock.setUiState).toHaveBeenCalledTimes(2);
  });
});

describe("E14：会话删除后的孤儿内容不得复活", () => {
  /** 铺一个「关 Tab 时选择保留内容」的会话：驻留表里留下草稿/队列/面板，运行态桶已 dispose */
  function seedRetained(id: string, projectId: string | null = null): void {
    seedTab(id, projectId);
    useRun.getState().initTab(id);
    useRun.getState().setDraftText(`草稿-${id}`, id);
    useRun.setState((s) => {
      s.tabs[id].queue = [{ id: `q-${id}`, text: `队列-${id}` }];
      s.tabs[id].suggestions = ["接着问一句"];
    });
    retainTabContent(id);
    closeTab(id);
    useRun.getState().dispose(id);
  }

  it("removeSession：驻留内容随会话删除被清掉，快照不含该项，下次启动也不回填", async () => {
    seedRetained("s1");
    seedTab("s2"); // 另一个存活会话：保证会话列表非空（否则走「列表不可信」分支）
    ipcMock.listSessions.mockResolvedValue([meta("s2")]);

    await useSessions.getState().removeSession(meta("s1"));

    const snap = await buildSnapshot();
    expect(snap.drafts).toEqual({});
    expect(snap.queue).toEqual({});
    expect(snap.panels).toEqual({});

    // 磁盘往返：下次启动读回这份快照，驻留表不应再出现 s1；即便有人重新打开该会话也不得回填
    reset();
    cleanStores();
    useSessions.setState({ sessions: [meta("s2")] });
    ipcMock.getUiState.mockResolvedValue(JSON.parse(JSON.stringify(snap)));
    await loadUiState();
    applyRetainedContent("s1");
    expect(useRun.getState().drafts.s1).toBeUndefined();
    expect(useRun.getState().tabs.s1).toBeUndefined();
  });

  it("快照侧收敛（第二道保险）：删除路径漏清时，已删会话的驻留内容也不写盘", async () => {
    seedRetained("s1");
    seedTab("s2");
    // 模拟「会话在别处被删」：只更新会话列表，故意不碰驻留表
    useSessions.setState({ sessions: [meta("s2")] });

    const snap = await buildSnapshot();
    expect(snap.drafts.s1).toBeUndefined();
    expect(snap.queue.s1).toBeUndefined();
    expect(snap.panels.s1).toBeUndefined();
    // 内存里的孤儿也一并清掉：否则后续每次快照都要重新判定一遍
    expect(await buildSnapshot()).not.toHaveProperty("drafts.s1");
  });

  it("deleteProject：项目下会话的驻留内容逐个清掉（临时会话不受影响）", async () => {
    seedProject("p1");
    seedRetained("s1", "p1");
    seedRetained("s2", "p1");
    seedRetained("s3"); // 临时会话（projectId = null）：不属级联范围
    ipcMock.deleteProject.mockResolvedValue({ deleted_sessions: 2 });
    ipcMock.listSessions.mockResolvedValue([meta("s3")]);

    await expect(useSessions.getState().deleteProject("p1")).resolves.toBe(2);

    const snap = await buildSnapshot();
    expect(snap.drafts.s1).toBeUndefined();
    expect(snap.drafts.s2).toBeUndefined();
    expect(snap.drafts.s3.text).toBe("草稿-s3"); // 非本项目会话的内容原样保留
  });

  it("会话列表为空（listSessions 失败被 refresh 容错成 []）时不收敛：保留的草稿不被误当孤儿抹掉", async () => {
    seedRetained("s1");
    useSessions.setState({ sessions: [] });
    const snap = await buildSnapshot();
    expect(snap.drafts.s1.text).toBe("草稿-s1");
  });
});
