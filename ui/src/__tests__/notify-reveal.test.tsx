// Notification click reveal regression tests (notification-click-reveal batch): in-app notification stack click-through + run:done unfocused notification carries the session
// Covers: open tab jumps / closed tab reopens / deleted session falls back / toast does not jump / run:done notification carries sessionId
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";

const { focusSpy } = vi.hoisted(() => ({ focusSpy: vi.fn() }));

const fixtureConfig = {
  schema_version: 2,
  providers: [
    {
      id: "p1", name: "Test Provider", api_format: "openai_chat",
      base_url: "https://api.example.com/v1", keys: ["***abcd"],
      models: [
        { id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: true, video: false },
      ],
    },
  ],
  active_model_id: "m1",
  proxy: null,
  network: { allow_private_network: false },
  compact_threshold: 0.6,
  approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false },
  validation: { python: true, rust: true, typescript: true, go: true, json: true },
  ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
};

const sessionFixture: SessionMeta = {
  id: "s1", title: "通知回跳会话", workspace: "/tmp/ws", model_id: "m1",
  created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z", message_count: 0,
  project_id: null, roots: [],
};

const fixtureMessages = [
  { role: "user", content: [{ type: "text", text: "你好" }] },
  { role: "assistant", content: [{ type: "text", text: "你好！" }] },
];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, _args?: any) => {
    switch (cmd) {
      case "ping": return "pong";
      case "activate_and_show": return null;
      case "get_config": return fixtureConfig;
      case "list_sessions": return [sessionFixture];
      case "list_projects": return [];
      case "load_session": return fixtureMessages;
      case "get_session_prefs": return { approval_mode: "auto_edit", model_id: null, reasoning_effort: null };
      case "git_status": return { repo: false, entries: [] };
      case "git_user_info": return { name: null, email: null };
      case "get_token_breakdown": return null;
      case "mcp_status": return [];
      case "list_skills": return [];
      case "notify_system": return "native";
      default: return null;
    }
  }),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    setFocus: focusSpy,
    show: vi.fn(),
    unminimize: vi.fn(),
    minimize: vi.fn(),
    close: vi.fn(),
    isMaximized: vi.fn(async () => false),
    maximize: vi.fn(),
    unmaximize: vi.fn(),
    onResized: vi.fn(() => Promise.resolve(vi.fn())),
    listen: vi.fn(() => Promise.resolve(vi.fn())),
  }),
}));

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => false),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));

import App from "../App";
import { useUi } from "../stores/ui";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import type { SessionMeta } from "../ipc/types";
import { DEFAULT_PREFS } from "../ipc/types";

function tabOf(id: string, title: string) {
  return { key: id, sessionId: id, workspace: "/tmp/ws", title, projectId: null, createdAt: "2026-01-01T00:00:00Z", prefs: { ...DEFAULT_PREFS } };
}

/** Locate the notify button: antd inserts a space inside two-character buttons (pitfalls list), so strip whitespace before matching */
function findNotifyButton(title: string): HTMLElement {
  const btn = screen
    .getAllByRole("button")
    .find((b) => (b.textContent ?? "").replace(/\s/g, "").includes(title));
  if (!btn) throw new Error(`找不到通知按钮：${title}`);
  return btn;
}

beforeEach(() => {
  focusSpy.mockClear();
});

afterEach(() => {
  cleanup();
  // zustand stores are module-level singletons (pitfalls list): reset between tests (same panel-toggle checklist as smoke)
  useUi.setState({ notifications: [], settingsOpen: false, tasksOpen: false, statsOpen: false, rightBarOpen: true, rbTab: "info" });
  useRun.setState({ tabs: {}, drafts: {} });
  useSessions.setState({ tabs: [], activeKey: null, sessions: [sessionFixture], explorerOpen: true });
  vi.restoreAllMocks();
});

describe("通知点击回跳", () => {
  it("站内通知点击：Tab 已开 → 切换 activeKey + 通知移除（窗口回前台由后端原生处理）", async () => {
    useSessions.setState({ tabs: [tabOf("s1", "通知回跳会话"), tabOf("s2", "另一会话")], activeKey: "s2" });
    useUi.getState().notify("通知回跳会话", "任务完成", "s1");
    render(<App />);
    await waitFor(() => expect(document.body.textContent).toContain("项目"));

    fireEvent.click(findNotifyButton("任务完成"));
    expect(useSessions.getState().activeKey).toBe("s1");
    await waitFor(() => expect(useUi.getState().notifications).toHaveLength(0));
  });

  it("站内通知点击：Tab 未开 → 从会话列表重开并落位", async () => {
    useSessions.setState({ tabs: [], activeKey: null, sessions: [sessionFixture] });
    useUi.getState().notify("通知回跳会话", "任务完成", "s1");
    render(<App />);
    await waitFor(() => expect(document.body.textContent).toContain("项目"));

    fireEvent.click(findNotifyButton("任务完成"));
    await waitFor(() => {
      const st = useSessions.getState();
      expect(st.tabs.map((t) => t.sessionId)).toContain("s1");
      expect(st.activeKey).toBe("s1");
    });
  });

  it("站内通知点击：会话已删除 → 无 Tab 动作（窗口回前台由后端处理）", async () => {
    useSessions.setState({ tabs: [], activeKey: null, sessions: [] });
    useUi.getState().notify("幽灵会话", "任务完成", "gone");
    render(<App />);
    await waitFor(() => expect(document.body.textContent).toContain("项目"));

    fireEvent.click(findNotifyButton("任务完成"));
    expect(useSessions.getState().activeKey).toBeNull();
  });

  it("toast 类通知（无 sessionId）→ 不跳转", async () => {
    useSessions.setState({ tabs: [], activeKey: null, sessions: [sessionFixture] });
    useUi.getState().toast("通用提示");
    render(<App />);
    await waitFor(() => expect(document.body.textContent).toContain("项目"));

    fireEvent.click(findNotifyButton("通用提示"));
    expect(useSessions.getState().activeKey).toBeNull();
  });

  it("run:done 失焦通知携带 sessionId（站内 + 系统通知可回跳的数据源）", async () => {
    const hasFocusSpy = vi.spyOn(document, "hasFocus").mockReturnValue(false);
    useSessions.setState({ tabs: [], activeKey: null, sessions: [sessionFixture] });
    render(<App />);
    await waitFor(() => expect(document.body.textContent).toContain("项目"));

    // Set running, then dispatch run:done by invoking the handler directly (same handler the listen router uses)
    useRun.getState().initTab("s1");
    useRun.setState((s) => {
      s.tabs["s1"].running = true;
    });
    const handlers = useRun.getState().bindGlobalHandlers();
    handlers["run:done"]({ session: "s1", run_id: "r1" });

    await waitFor(() => {
      const notifications = useUi.getState().notifications;
      expect(notifications).toHaveLength(1);
      expect(notifications[0].sessionId).toBe("s1");
      expect(notifications[0].body).toContain("任务完成");
    });
    expect(useRun.getState().tabs["s1"].running).toBe(false);
    hasFocusSpy.mockRestore();
  });
});
