// AppShell 启动自动清理提示（会话保留期与清理 · [docs/session-cleanup](../../../docs/session-cleanup.md)）——
// 「启动时的那一次清理有删除就轻提示一次」这一跳的回归。
//
// 为什么必须有这条测试：清理的执行在后端，前端只有「读状态 → 判断要不要提示 → 提示」三行逻辑，
// 而判断依赖 localStorage 里的去重记录、提示依赖挂载链走到最后一步。写错任一环的表现都是
// 「每次启动都提示一遍」或「删了会话却什么都不说」——两者都不会让别的用例变红。
//
// 观测口径（沿用 appshell.restore.test.tsx 的既有做法）：
// - 唯一 invoke 入口 ui/src/ipc/client 整体 mock；挂载链上的只读调用逐条列出，未列出的一律返回 resolved null。
// - @tauri-apps/api/{core,event,window} 与 plugin-notification 垫桩（AppShell / useTitlebar 有直连 import）。
// - 提示落到 ui store 的 notifications（useUi.notify），因此断言 notifications 里的正文。
// - happy-dom 无布局：只断言 store 与 localStorage，不碰 DOM 几何。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, waitFor, act } from "@testing-library/react";

const h = vi.hoisted(() => {
  const PREFS = { approval_mode: "auto_edit", model_id: null, reasoning_effort: null };
  const CONFIG = {
    schema_version: 2,
    providers: [
      {
        id: "p1", name: "Test Provider", api_format: "openai_chat",
        base_url: "https://api.example.com/v1", keys: ["***abcd"],
        models: [
          {
            id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
            reasoning_effort: null, vision: true, video: false,
          },
        ],
      },
    ],
    active_model_id: "m1",
    proxy: null,
    network: { allow_private_network: false },
    compact_threshold: 0.6,
    compact_timeout_seconds: 180,
    approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
  };
  const MESSAGES = [
    { role: "user", content: [{ type: "text", text: "你好" }] },
    { role: "assistant", content: [{ type: "text", text: "你好！有什么可以帮你？" }] },
  ];
  /** 一次清理的「运行时间」夹具：去重记录就是拿它比对的 */
  const RUN_AT = "2026-09-20T02:00:00Z";
  const state = {
    uiState: null as unknown,
    sessions: [] as any[],
    /** getCleanupStatus 的返回值；null = 后端还没有清理记录 */
    cleanup: null as null | { last_run_at: string | null; last_deleted: number; last_failed: number },
  };
  const listeners = new Map<string, ((e: any) => void)[]>();
  const fn = (impl: (...args: any[]) => any = async () => null) => vi.fn(impl);
  const ipcMethods: Record<string, any> = {
    getConfig: fn(async () => CONFIG),
    getUiState: fn(async () => state.uiState),
    setUiState: fn(async () => undefined),
    listSessions: fn(async () => state.sessions),
    listProjects: fn(async () => []),
    loadSession: fn(async () => MESSAGES),
    sessionRunning: fn(async () => false),
    getSessionPrefs: fn(async () => ({ ...PREFS })),
    connectMcp: fn(async () => ({ started: [], failed: [] })),
    getTokenBreakdown: fn(async () => ({
      system_tokens: 0, history_tokens: 0, tool_results_tokens: 0, tool_schema_tokens: 0,
      total_tokens: 0, context_window: 128000, ratio: 0,
    })),
    // 本文件的主角：启动时读一次上次清理记录
    getCleanupStatus: fn(async () => state.cleanup),
    // 挂载时会被拉取的其它只读数据（少一个会在 happy-dom 里炸掉挂载链）
    gitStatus: fn(async () => ({ repo: false, entries: [], branch: null })),
    gitDiff: fn(async () => ({ files: [], truncated: false })),
    gitUserInfo: fn(async () => ({ name: null, email: null })),
    listSkills: fn(async () => []),
    listAgents: fn(async () => []),
    listScheduledTasks: fn(async () => []),
    listSessionFiles: fn(async () => []),
    listLogFiles: fn(async () => []),
    searchWorkspacePaths: fn(async () => []),
    loadSubagentHistory: fn(async () => []),
    getMcpConfig: fn(async () => "{}"),
    mcpStatus: fn(async () => []),
    appVersion: fn(async () => "0.0.0-test"),
  };
  return { PREFS, state, listeners, ipcMethods, RUN_AT };
});

vi.mock("../ipc/client", () => ({
  ipc: new Proxy(h.ipcMethods, {
    // 兜底：未列出的 ipc 方法返回 resolved null（避免「mock 少一个方法」造成假失败）
    get(target, prop) {
      if (typeof prop === "symbol") return Reflect.get(target, prop);
      if (!Object.prototype.hasOwnProperty.call(target, prop)) target[prop] = vi.fn(async () => null);
      return target[prop];
    },
  }),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, cb: (e: any) => void) => {
    const arr = h.listeners.get(name) ?? [];
    arr.push(cb);
    h.listeners.set(name, arr);
    return () => {};
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn(), close: vi.fn(), setFocus: vi.fn(), show: vi.fn(), unminimize: vi.fn(),
    isMaximized: vi.fn(async () => false), maximize: vi.fn(), unmaximize: vi.fn(),
    onResized: vi.fn(() => Promise.resolve(vi.fn())),
    listen: vi.fn(() => Promise.resolve(vi.fn())),
    scaleFactor: async () => 1,
    outerPosition: async () => ({ x: 0, y: 0 }),
    innerSize: async () => ({ width: 1200, height: 800 }),
  }),
}));

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => false),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));

import App from "../App";
import { ipc } from "../ipc/client";
import { i18n } from "../i18n";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { reset as resetUiState } from "../utils/uiState";
import type { UiState } from "../utils/uiState";
import type { SessionMeta } from "../ipc/types";

/** 去重记录键（与 AppShell 里的 CLEANUP_NOTICE_SEEN_KEY 同值；键名本身就是契约） */
const SEEN_KEY = "ws_cleanup_notice_seen_run";

function meta(id: string): SessionMeta {
  return {
    id,
    title: `会话 ${id}`,
    workspace: `/tmp/${id}`,
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    message_count: 0,
    project_id: null,
    roots: [`/tmp/${id}`],
    running: false,
    interrupted: null,
  };
}

/** ui-state 快照（schema 1）：只需一个 Tab，其余键按空结构补齐 */
function uiSnapshot(keys: string[], activeKey: string | null): UiState {
  const items: Record<string, any> = {};
  for (const k of keys) {
    items[k] = {
      workspace: `/tmp/${k}`,
      title: `会话 ${k}`,
      projectId: null,
      createdAt: "2026-09-01T00:00:00Z",
      prefs: { ...h.PREFS },
    };
  }
  return {
    schema: 1,
    tabs: { order: keys, activeKey, items },
    activeProject: null,
    window: null,
    scrollAnchors: {},
    drafts: {},
    queue: {},
    tree: { expanded: {}, collapsed: false, unread: {} },
    panels: {},
  };
}

/** 挂载完整 App，并等到挂载链走到「读上次清理记录」这一步（该步在 hydrate 之后） */
async function mountAppAndReadCleanup() {
  const utils = render(<App />);
  await waitFor(() => expect(ipc.getCleanupStatus).toHaveBeenCalled(), { timeout: 5000 });
  return utils;
}

/** 提示正文（应用内轻提示落 store，正文即文案） */
function toastBodies(): string[] {
  return useUi.getState().notifications.map((n) => n.body);
}

/** 等到「这次清理的提示已经出现」；不出现则由 waitFor 超时判红 */
async function waitForToast(text: string) {
  await waitFor(() => expect(toastBodies()).toContain(text));
}

afterEach(() => {
  cleanup();
  // zustand store 是模块级单例：通知堆栈 / 面板开关 / 请求位全部复位，避免串味
  useUi.setState({
    settingsOpen: false, tasksOpen: false, statsOpen: false,
    exitRequest: null, closeTabRequest: null, notifications: [], mcpStatus: [],
    rightBarOpen: true, rbTab: "info", treeExpand: {}, treeCollapsed: false,
  });
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {}, explorerOpen: true });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
  // uiState 是模块级单例：放最后 reset，免得上面的 setState 又挂上新计时器
  resetUiState();
  h.listeners.clear();
  h.state.uiState = null;
  h.state.sessions = [];
  h.state.cleanup = null;
  localStorage.removeItem(SEEN_KEY);
  localStorage.removeItem("ws_explorer_open");
  localStorage.removeItem("ws_right_bar_open");
  vi.clearAllMocks();
});

/** 进入「有快照的正常启动」：一个会话 + 一份指向它的现场态快照 */
function seedStartup() {
  h.state.sessions = [meta("s1")];
  h.state.uiState = uiSnapshot(["s1"], "s1");
}

describe("AppShell 启动自动清理提示（会话保留期与清理）", () => {
  it("这次启动的清理删了 N 条 → 给一条含条数的轻提示，并把这次清理的时间记进 localStorage", async () => {
    seedStartup();
    h.state.cleanup = { last_run_at: h.RUN_AT, last_deleted: 3, last_failed: 0 };

    await mountAppAndReadCleanup();

    const expected = i18n.t("app.cleanupAutoDone", { n: 3 });
    await waitForToast(expected);
    // 只说删了几条（正文里带条数），不复述后端术语
    expect(expected).toContain("3");
    expect(localStorage.getItem(SEEN_KEY)).toBe(h.RUN_AT);
  });

  it("localStorage 已记下同一次清理 → 不重复提示（同一次清理只提示一遍）", async () => {
    seedStartup();
    h.state.cleanup = { last_run_at: h.RUN_AT, last_deleted: 3, last_failed: 0 };
    localStorage.setItem(SEEN_KEY, h.RUN_AT);

    await mountAppAndReadCleanup();
    // 状态照旧读了一次（后端记录是权威），但提示不再出现
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]));
    await new Promise((r) => setTimeout(r, 50));
    expect(toastBodies()).toEqual([]);
    expect(localStorage.getItem(SEEN_KEY)).toBe(h.RUN_AT);
  });

  it("这次清理一条都没删 → 不提示，但记录照样更新（下次启动不再回头比较这一次）", async () => {
    seedStartup();
    h.state.cleanup = { last_run_at: h.RUN_AT, last_deleted: 0, last_failed: 0 };

    await mountAppAndReadCleanup();
    await waitFor(() => expect(localStorage.getItem(SEEN_KEY)).toBe(h.RUN_AT));
    expect(toastBodies()).toEqual([]);
  });

  it("还没清理过（last_run_at = null）→ 不提示、不写记录（没有可提示的内容）", async () => {
    seedStartup();
    h.state.cleanup = { last_run_at: null, last_deleted: 0, last_failed: 0 };

    await mountAppAndReadCleanup();
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]));
    expect(toastBodies()).toEqual([]);
    expect(localStorage.getItem(SEEN_KEY)).toBeNull();
  });

  it("读状态失败 → 静默，启动链路的其它调用照常完成（不因清理提示而中断启动）", async () => {
    seedStartup();
    vi.mocked(ipc.getCleanupStatus).mockRejectedValueOnce(new Error("状态文件读不出来"));

    await mountAppAndReadCleanup();

    // 提示没有出现，且启动链路的其它步骤都走过：两份列表 → 读盘 → 活跃 Tab 急切加载
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]));
    expect(toastBodies()).toEqual([]);
    expect(ipc.getUiState).toHaveBeenCalled();
    expect(ipc.listSessions).toHaveBeenCalled();
    expect(ipc.listProjects).toHaveBeenCalled();
    await waitFor(() => expect(ipc.loadSession).toHaveBeenCalledWith("s1", "/tmp/s1"));
    expect(localStorage.getItem(SEEN_KEY)).toBeNull();
  });

  it("挂载链只读一次清理状态，且打开设置页不会把提示再放一遍", async () => {
    seedStartup();
    h.state.cleanup = { last_run_at: h.RUN_AT, last_deleted: 1, last_failed: 0 };

    await mountAppAndReadCleanup();
    await waitForToast(i18n.t("app.cleanupAutoDone", { n: 1 }));
    // 挂载链（AppShell 启动流程）里就这一次调用：不额外触发任何清理，也不掐点轮询
    expect(ipc.getCleanupStatus).toHaveBeenCalledTimes(1);

    // 设置页自己也读同一条记录（既有行为，与本次提示无关）；提示只在启动时判定一次，所以不会再放一遍
    act(() => {
      useUi.getState().showSettings();
    });
    act(() => {
      useUi.setState({ settingsOpen: false });
    });
    await new Promise((r) => setTimeout(r, 50));
    expect(toastBodies()).toHaveLength(1);
  });
});
