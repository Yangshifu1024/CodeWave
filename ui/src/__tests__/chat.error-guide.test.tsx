// Error card guidance ([docs/auth-error-guidance](../../../docs/auth-error-guidance.md)): auth/billing error items render the "open model settings" shortcut that
// opens the settings modal on the providers tab; other errors stay button-free dead ends.
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import "../i18n"; // ChatMessages mounted standalone must init i18next explicitly (otherwise t() returns the raw key)
import ChatMessages from "../features/chat/ChatMessages";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

function seedWithItem(item: { kind: "error"; text: string; errorKind?: string }) {
  useSessions.setState({
    tabs: [{ key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null } }],
    activeKey: "s1",
    projects: [],
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [item], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
      queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

describe("error card auth/billing guidance (docs/auth-error-guidance)", () => {
  beforeEach(() => {
    useUi.setState({ settingsOpen: false, settingsTab: "general" });
  });
  afterEach(cleanup);

  it("auth error shows hint + shortcut button; click opens settings on the providers tab", () => {
    seedWithItem({ kind: "error", text: "认证失败：Header中未收到Authorization参数 (HTTP 401)", errorKind: "auth" });
    render(<ChatMessages />);
    expect(screen.getByText("认证失败，请检查对应供应商的 API Key 配置后重试。")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "打开模型设置" }));
    expect(useUi.getState().settingsOpen).toBe(true);
    expect(useUi.getState().settingsTab).toBe("providers");
  });

  it("billing error shows the billing hint", () => {
    seedWithItem({ kind: "error", text: "计费/余额错误：Insufficient Balance (HTTP 402)", errorKind: "billing" });
    render(<ChatMessages />);
    expect(screen.getByText("计费/余额错误，请检查对应供应商的账户状态后重试。")).toBeTruthy();
    expect(screen.getByRole("button", { name: "打开模型设置" })).toBeTruthy();
  });

  it("other errors render without the guidance row", () => {
    seedWithItem({ kind: "error", text: "网络错误：connection refused" });
    render(<ChatMessages />);
    expect(screen.getByText("网络错误：connection refused")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "打开模型设置" })).toBeNull();
  });
});
