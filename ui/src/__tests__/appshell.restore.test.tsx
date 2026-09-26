// AppShell 启动现场态恢复 / 退出拦截 / 关 Tab 二次确认 —— 集成回归（会话保存与恢复优化 · 批1 的最后一跳接线）。
//
// 为什么必须有这条测试：批1 的基础模块（utils/uiState、sessions.restoreTabs、ui store 的请求位）各自都有单测，
// 但「基础模块全写完、导出零调用方」的死代码事故在本项目出过两次——真正把
//   读 ui-state → 落到各 store → 打开落盘订阅 → 急切加载活跃 Tab → 绑定事件
// 串起来的唯一调用方是 AppShell。所以这里断言的是**调用链本身**（谁调了谁、点下去回后端走哪条 IPC），
// 而不是各模块内部逻辑（那部分由 uiState.test.ts / composer.per-tab.test.tsx / interrupt-marker.test.tsx 守护）。
//
// 观测口径：
// - 唯一 invoke 入口 ui/src/ipc/client 整体 mock：挂载链与交互面的断言都做在这一层。
//   ipc mock 带 Proxy 兜底——未显式列出的方法一律返回 resolved null，将来挂载链上新增只读调用不会把本文件打挂。
// - @tauri-apps/api/{core,event,window} 与 plugin-notification 按 app.smoke.test.tsx 的既有做法垫桩
//   （AppShell / useTitlebar 有直连 import，不走 ipc 封装）。
// - 「落盘」口径：uiState 的防抖最终走 ipc.setUiState → invoke("set_ui_state")，因此断言 ipc.setUiState 的入参快照。
// - happy-dom 没有布局与关闭动画：只断言 store / 弹窗 DOM / IPC 调用；rc-motion 的隐藏弹窗节点会留在 DOM 里，
//   所以弹窗存在性用「文案包含」而非节点计数，且每个动作用例单独挂载一次。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent, waitFor, act } from "@testing-library/react";

// ---------- 可调桩：夹具 + 每次调用现读的桩值 + listen 捕获表 ----------
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
  /** 首屏载荷（批2 P3：`load_session` 返回 `{ messages, paging }`）——legacy 口径：整份给出、无更早内容 */
  const FIRST_PAGE = {
    messages: MESSAGES,
    paging: {
      format: "legacy",
      loaded_from_seq: 0,
      segment_count: 1,
      total_messages: MESSAGES.length,
      bytes: 0,
      has_more: false,
    },
  };
  /** 每次调用现读的桩值：uiState = get_ui_state 返回的快照，sessions = list_sessions 返回的会话列表 */
  const state = { uiState: null as unknown, sessions: [] as any[] };
  /** listen 捕获表：事件名 → 回调数组（bindEvents 的 29 键与 AppShell 直连的 notify:activate / menu:action 共用） */
  const listeners = new Map<string, ((e: any) => void)[]>();
  const fn = (impl: (...args: any[]) => any = async () => null) => vi.fn(impl);
  const ipcMethods: Record<string, any> = {
    // 挂载链直接依赖
    getConfig: fn(async () => CONFIG),
    getUiState: fn(async () => state.uiState),
    setUiState: fn(async () => undefined),
    listSessions: fn(async () => state.sessions),
    listProjects: fn(async () => []),
    loadSession: fn(async () => FIRST_PAGE),
    sessionRunning: fn(async () => false),
    getSessionPrefs: fn(async () => ({ ...PREFS })),
    restoreLegacyModelPrefs: fn(async () => undefined),
    connectMcp: fn(async () => ({ started: [], failed: [] })),
    getTokenBreakdown: fn(async () => ({
      system_tokens: 0, history_tokens: 0, tool_results_tokens: 0, tool_schema_tokens: 0,
      total_tokens: 0, context_window: 128000, ratio: 0,
    })),
    // 交互面（退出拦截 / 关 Tab 确认 / 中断提示条 / git 身份）
    resolveExitRequest: fn(async () => undefined),
    clearSessionInterrupt: fn(async () => undefined),
    gitStatus: fn(async () => ({ repo: false, entries: [], branch: null })),
    gitDiff: fn(async () => ({ files: [], truncated: false })),
    gitUserInfo: fn(async () => ({ name: null, email: null })),
    // 左栏 / 右栏 / Composer 挂载时会拉取的只读数据（少一个就会在 happy-dom 里炸掉挂载链）
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
  return { PREFS, CONFIG, MESSAGES, state, listeners, ipcMethods };
});

vi.mock("../ipc/client", () => ({
  ipc: new Proxy(h.ipcMethods, {
    // 兜底：未列出的 ipc 方法返回 resolved null（避免「mock 少一个方法」造成假失败；要断言的方法都在上面显式列出）
    get(target, prop) {
      if (typeof prop === "symbol") return Reflect.get(target, prop);
      if (!Object.prototype.hasOwnProperty.call(target, prop)) target[prop] = vi.fn(async () => null);
      return target[prop];
    },
  }),
}));

// useTitlebar 直连 invoke("activate_and_show")：返回 null → 前端回落自绘标题栏（与 app.smoke 同口径）
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

// listen 既是 bindEvents 的注册通道，也是本文件观察「事件面真的接上了」的抓手：捕获回调后可直接投喂事件
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
    // uiState.readWindowGeometry 读窗口几何用（逻辑像素口径）；缺了会走 catch 兜底，这里给足值
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
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import { FLUSH_DEBOUNCE_MS, flushNow, reset as resetUiState } from "../utils/uiState";
import type { UiState } from "../utils/uiState";
import type { SessionMeta } from "../ipc/types";

/** 会话 meta 夹具（与后端 SessionMeta 同形；extra 用于挂 interrupted 等场景字段） */
function meta(id: string, extra: Partial<SessionMeta> = {}): SessionMeta {
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
    ...extra,
  };
}

/** ui-state 快照（schema 1）：只关心 Tab 骨架，其余键按空结构补齐（normalizeUiState 对缺键宽容，这里保持真实形状） */
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
    tree: { expanded: {}, groupFolded: {}, collapsed: false, unread: {} },
    panels: {},
  };
}

/** 挂载完整 App（App.tsx 提供 i18n + antd ConfigProvider/App 容器，AppShell 是唯一消费者）；等到读盘第一跳被调用 */
async function mountApp() {
  const utils = render(<App />);
  await waitFor(() => expect(ipc.getUiState).toHaveBeenCalled(), { timeout: 5000 });
  return utils;
}

/** 按文案点按钮：antd 会给两字中文按钮插空格（「取 消」），先去空白再比对 */
async function clickButton(text: string) {
  const want = text.replace(/\s/g, "");
  const btn = Array.from(document.querySelectorAll("button")).find((b) =>
    (b.textContent ?? "").replace(/\s/g, "").includes(want),
  );
  if (!btn) throw new Error(`找不到按钮：${text}`);
  fireEvent.click(btn);
  // antd Modal 挂载 + 内部态收敛需要一拍
  await new Promise((r) => setTimeout(r, 80));
}

afterEach(() => {
  cleanup();
  // zustand store 是模块级单例：面板开关 / 请求位 / 树态复位，避免弹窗与快照串味
  useUi.setState({
    settingsOpen: false, tasksOpen: false, statsOpen: false,
    exitRequest: null, closeTabRequest: null,
    treeExpand: {}, treeCollapsed: false, rightBarOpen: true, rbTab: "info",
    notifications: [], mcpStatus: [], // 中断提示条用例会产生 toast（6 秒后自行消失），别漏给后面的用例
  });
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {}, explorerOpen: true });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
  // uiState 是模块级单例（驻留表 / 锚点 / 落盘计时器）：放最后 reset，免得上面的 setState 又挂上新计时器
  resetUiState();
  h.listeners.clear();
  h.state.uiState = null;
  h.state.sessions = [];
  localStorage.removeItem("ws_explorer_open");
  localStorage.removeItem("ws_right_bar_open");
  vi.clearAllMocks();
  h.ipcMethods.getSessionPrefs.mockImplementation(async () => ({ ...h.PREFS }));
  h.ipcMethods.restoreLegacyModelPrefs.mockImplementation(async () => undefined);
});

describe("AppShell 启动链路（会话保存与恢复优化 · 批1）", () => {
  it("旧 Tab 的模型与力度先迁移，回读只覆盖成后端确认后的值", async () => {
    h.state.sessions = [meta("s1")];
    const snapshot = uiSnapshot(["s1"], "s1");
    snapshot.tabs.items.s1.prefs = { approval_mode: "full_access", model_id: "m1", reasoning_effort: "max" };
    h.state.uiState = snapshot;
    let backendPrefs = { ...h.PREFS, approval_mode: "plan", model_id: null as string | null, reasoning_effort: null as string | null };
    h.ipcMethods.restoreLegacyModelPrefs.mockImplementation(async (_id: string, modelId: string | null, effort: string | null) => {
      backendPrefs = { ...backendPrefs, model_id: modelId, reasoning_effort: effort };
    });
    h.ipcMethods.getSessionPrefs.mockImplementation(async () => ({ ...backendPrefs }));

    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs[0]?.prefs.approval_mode).toBe("plan"));
    expect(useSessions.getState().tabs[0].prefs.model_id).toBe("m1");
    expect(ipc.restoreLegacyModelPrefs).toHaveBeenCalledWith("s1", "m1", "max");
    expect(useSessions.getState().tabs[0].prefs.reasoning_effort).toBe("max");
    expect(useSessions.getState().tabs[0].prefs.approval_mode).toBe("plan");
    expect(vi.mocked(ipc.restoreLegacyModelPrefs).mock.invocationCallOrder[0])
      .toBeLessThan(vi.mocked(ipc.getSessionPrefs).mock.invocationCallOrder[0]);
  });

  it("挂载即走「读盘 → 落 store → 急切加载活跃 Tab → 绑定事件」全链（守「模块写完没人调」的死代码事故）", async () => {
    h.state.sessions = [meta("s1")];
    h.state.uiState = uiSnapshot(["s1"], "s1");
    await mountApp();

    // 读盘 + 两份列表（listSessions / listProjects 必须先到位，否则 restoreTabs 会把 Tab 全当失效引用剔掉）
    expect(ipc.getUiState).toHaveBeenCalled();
    expect(ipc.listSessions).toHaveBeenCalled();
    expect(ipc.listProjects).toHaveBeenCalled();

    // 快照真的落进 store（不是读了盘就丢）
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]));
    expect(useSessions.getState().activeKey).toBe("s1");

    // 活跃 Tab 急切加载：loadSession + 运行参数 + 后端运行态都要问一遍（骨架 Tab 不走这条，见下一条用例）
    await waitFor(() => expect(ipc.loadSession).toHaveBeenCalledWith("s1", "/tmp/s1"));
    expect(ipc.getSessionPrefs).toHaveBeenCalledWith("s1");
    expect(ipc.sessionRunning).toHaveBeenCalledWith("s1");

    // bindEvents 注册生效：handler 表里每个键都真的 listen 上了（键集合由 events.contract.test.ts 守护）
    const keys = Object.keys(useRun.getState().bindGlobalHandlers());
    await waitFor(() => {
      for (const k of keys) expect(h.listeners.has(k)).toBe(true);
    });
    // AppShell 直连 listen 的两条（不走 handler 表）：系统通知回跳 + macOS 应用菜单动作
    for (const name of ["notify:activate", "menu:action"]) expect(h.listeners.has(name)).toBe(true);

    // 回调是活的（不是「listen 上了但 handler 是死的」）：投喂退出请求事件 → 落到 ui store 的请求位
    act(() => {
      h.listeners.get("app:exit_requested")![0]({ payload: { running: ["s1"] } });
    });
    expect(useUi.getState().exitRequest).toEqual({ running: ["s1"] });
  });

  it("快照恢复：两个 Tab 都回来、活跃 Tab 正确、非活跃 Tab 保持骨架（不急切拉消息）", async () => {
    h.state.sessions = [meta("s1"), meta("s2")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s2");
    await mountApp();

    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1", "s2"]));
    expect(useSessions.getState().activeKey).toBe("s2");
    // 标题取自快照（现场态权威），不是回查会话列表
    expect(useSessions.getState().tabs[0].title).toBe("会话 s1");
    // 批1 约定：只有活跃 Tab 立即拉消息，其余 loaded=false，首次激活才拉（省启动开销）
    expect(useSessions.getState().tabs[0].loaded).toBe(false);

    await waitFor(() => expect(ipc.loadSession).toHaveBeenCalledWith("s2", "/tmp/s2"));
    expect(ipc.loadSession).not.toHaveBeenCalledWith("s1", expect.anything());
  });

  it("失效引用剔除：快照里的 Tab 不在 listSessions 结果里 → 静默剔除、不抛错、活跃回落邻位（守「重启后 Tab 全没了」）", async () => {
    // s2 已被删除：快照还引用着它，listSessions 不再返回
    h.state.sessions = [meta("s1")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s2");
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await mountApp();
      await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]));
      // 活跃 Tab 失效 → 回落邻位（不是空态，也不是抛错中断启动）
      expect(useSessions.getState().activeKey).toBe("s1");
      // 恢复流程不因剔除而进 catch 分支（进 catch = 整批降级为无快照启动）
      expect(warn.mock.calls.flat().join(" ")).not.toContain("ui-state 恢复失败");
      await waitFor(() => expect(ipc.loadSession).toHaveBeenCalledWith("s1", "/tmp/s1"));
    } finally {
      warn.mockRestore();
    }
  });

  it("绑定早于 hydrate：读盘未完成时到达的 app:exit_requested 也被接住（单次下发不重放）", async () => {
    h.state.sessions = [meta("s1")];
    // 把 getUiState 卡住 = hydrate 停在读盘这一步（真实启动里这一段还有配置加载与两份列表，
    // 旧实现把 bindEvents 放在它们之后，这整段窗口的退出事件无人接）
    let release!: (v: unknown) => void;
    const gate = new Promise<unknown>((r) => {
      release = r;
    });
    vi.mocked(ipc.getUiState).mockImplementationOnce(() => gate as Promise<unknown>);

    const utils = render(<App />);
    // 事件面必须已订阅：旧顺序下这里会一直等到 hydrate 完成（超时失败）
    await waitFor(() => expect(h.listeners.get("app:exit_requested")).toBeTruthy(), { timeout: 5000 });
    expect(ipc.getUiState).toHaveBeenCalled();

    act(() => {
      h.listeners.get("app:exit_requested")![0]({ payload: { running: ["s1"] } });
    });
    expect(useUi.getState().exitRequest).toEqual({ running: ["s1"] });

    // 放行读盘：hydrate 收尾不得把这个请求抹掉（否则用户仍然只能看到无请求的界面）
    await act(async () => {
      release(uiSnapshot(["s1"], "s1"));
      await Promise.resolve();
    });
    expect(useUi.getState().exitRequest).toEqual({ running: ["s1"] });
    utils.unmount();
  });
});

describe("退出拦截（会话保存与恢复优化 · 批1）", () => {
  for (const [label, action] of [
    ["等完成", "wait"],
    ["中断并保存后退出", "abort"],
    ["取消", "cancel"],
  ] as const) {
    it(`「${label}」→ 回后端 resolve_exit_request("${action}")，应答完成后请求位清空`, async () => {
      h.state.sessions = [meta("s1"), meta("s2")];
      h.state.uiState = uiSnapshot(["s1"], "s1");
      await mountApp();

      act(() => {
        useUi.setState({ exitRequest: { running: ["s1", "s2"] } });
      });
      await waitFor(() => expect(document.body.textContent ?? "").toContain("仍有任务在运行"));
      // 弹窗必须列出在跑的会话：用户要知道自己会中断什么
      expect(document.body.textContent ?? "").toContain("以下 2 个会话仍在运行");

      await clickButton(label);
      await waitFor(() => expect(ipc.resolveExitRequest).toHaveBeenCalledWith(action));
      // 应答完成（或通道已关而失败）后才清：留幽灵弹窗会让应用卡在「退不出去」
      await waitFor(() => expect(useUi.getState().exitRequest).toBeNull());
    });
  }
});

describe("关 Tab 二次确认（会话保存与恢复优化 · 批1）", () => {
  it("「丢弃并关闭」：Tab 被关掉且请求位清空", async () => {
    h.state.sessions = [meta("s1"), meta("s2")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(2));

    act(() => {
      useUi.setState({ closeTabRequest: "s1" });
    });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("该会话有未发送内容"));
    expect(document.body.textContent ?? "").toContain("草稿或排队消息尚未发出");

    await clickButton("丢弃并关闭");
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s2"]));
    expect(useUi.getState().closeTabRequest).toBeNull();
  });

  it("「取消」：只清请求位，Tab 与草稿原样留着", async () => {
    h.state.sessions = [meta("s1"), meta("s2")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(2));
    act(() => {
      useRun.setState((s) => {
        s.drafts["s1"] = { text: "珍贵草稿", images: [], refs: [] };
      });
    });

    act(() => {
      useUi.setState({ closeTabRequest: "s1" });
    });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("该会话有未发送内容"));

    await clickButton("取消");
    await waitFor(() => expect(useUi.getState().closeTabRequest).toBeNull());
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1", "s2"]);
    expect(useRun.getState().drafts["s1"]!.text).toBe("珍贵草稿");
  });

  it("「关闭但保留草稿」：Tab 关闭、内容搬进驻留表等重开回填", async () => {
    h.state.sessions = [meta("s1"), meta("s2")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(2));
    act(() => {
      useRun.setState((s) => {
        s.drafts["s1"] = { text: "留着下次发", images: [], refs: [] };
      });
    });

    act(() => {
      useUi.setState({ closeTabRequest: "s1" });
    });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("该会话有未发送内容"));

    await clickButton("关闭但保留草稿");
    await waitFor(() => expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s2"]));
    expect(useUi.getState().closeTabRequest).toBeNull();
    expect(useRun.getState().drafts["s1"]).toBeUndefined(); // 关 Tab 会 dispose 运行态分桶
  });

  it("防幽灵弹窗：请求指向已不存在的 Tab → 不渲染弹窗且状态被清", async () => {
    h.state.sessions = [meta("s1")];
    h.state.uiState = uiSnapshot(["s1"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(1));

    // 会话被删除 / 项目级联删除后再点关闭：请求位会指向一个不存在的 key
    act(() => {
      useUi.setState({ closeTabRequest: "ghost" });
    });
    await waitFor(() => expect(useUi.getState().closeTabRequest).toBeNull());
    expect(document.body.textContent ?? "").not.toContain("该会话有未发送内容");
  });

  it("Cmd+W 入口：有未发送内容时不直接关，而是置二次确认请求（三个关闭入口共用这一处弹窗）", async () => {
    h.state.sessions = [meta("s1")];
    h.state.uiState = uiSnapshot(["s1"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(1));
    act(() => {
      useRun.setState((s) => {
        s.drafts["s1"] = { text: "未发送", images: [], refs: [] };
      });
    });

    fireEvent.keyDown(window, { key: "w", metaKey: true, cancelable: true });

    await waitFor(() => expect(useUi.getState().closeTabRequest).toBe("s1"));
    expect(useSessions.getState().tabs.map((t) => t.key)).toEqual(["s1"]); // 没有被直接关掉
    await waitFor(() => expect(document.body.textContent ?? "").toContain("该会话有未发送内容"));
    act(() => {
      useUi.getState().setCloseTabRequest(null); // 收尾：别把关闭态漏给下一个用例
    });
  });
});

describe("现场态落盘与中断提示条（会话保存与恢复优化 · 批1）", () => {
  it("落盘不变量：挂载即打开 store→落盘订阅；防抖窗口内不写，到期后写入最新现场态", async () => {
    h.state.sessions = [meta("s1"), meta("s2")];
    h.state.uiState = uiSnapshot(["s1", "s2"], "s1");
    await mountApp();
    await waitFor(() => expect(useSessions.getState().tabs.length).toBe(2));

    // 口径：先手动 flush 一次，把挂载期（refresh / restoreTabs）挂上的防抖计时器清掉——
    // 否则它会在下面的断言窗口里提前落一次盘，把「防抖窗口内不写」变成假失败
    await flushNow();
    vi.mocked(ipc.setUiState).mockClear();

    // 非活跃 Tab 的未读集合变化（活跃会话的未读由 sessions 订阅自动清，不作落盘差异）
    act(() => {
      useSessions.getState().markUnread("s2");
    });
    expect(ipc.setUiState).not.toHaveBeenCalled(); // 1.2s 防抖窗口内不落盘（高频变更只写最后一次）

    await act(async () => {
      await new Promise((r) => setTimeout(r, FLUSH_DEBOUNCE_MS + 300));
    });
    expect(ipc.setUiState).toHaveBeenCalled();
    const last = vi.mocked(ipc.setUiState).mock.calls.at(-1)![0] as UiState;
    expect(last.schema).toBe(1);
    expect(last.tabs.order).toEqual(["s1", "s2"]); // Tab 现场态完整落盘
    expect(last.tabs.activeKey).toBe("s1");
    expect(last.tree.unread.s2).toBe(true); // 变更真的进了这一份快照
  });

  it("中断提示条：活跃会话带 interrupted → 顶部渲染，点「清除中断标记」回后端并就地撤标记", async () => {
    h.state.sessions = [meta("s1", { interrupted: { kind: "quit", at: "2026-09-20T02:00:00Z" } })];
    h.state.uiState = uiSnapshot(["s1"], "s1");
    await mountApp();

    await waitFor(() => expect(document.querySelector(".interrupt-banner")).toBeTruthy());
    const banner = document.querySelector(".interrupt-banner") as HTMLElement;
    expect(banner.textContent ?? "").toContain("上次运行时被中断（正常退出前中止）");
    expect(banner.getAttribute("data-kind")).toBe("quit"); // 样式按中断类型分档

    await clickButton("清除中断标记");
    await waitFor(() => expect(ipc.clearSessionInterrupt).toHaveBeenCalledWith("s1"));
    // 就地更新列表项（不重拉整份会话列表）+ 成功提示
    await waitFor(() => expect(document.querySelector(".interrupt-banner")).toBeFalsy());
    expect(useSessions.getState().sessions[0].interrupted).toBeNull();
  });
});
