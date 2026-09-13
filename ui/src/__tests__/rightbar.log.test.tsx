// RightBar log tab ([docs/session-logging-report](../../../docs/session-logging-report.md) → [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md) switched to three Tabs):
// zero IPC when inactive / loads content when active / stale-response guard after session switch
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import "../i18n"; // mounting RightBar directly requires explicit i18next init (collapse button aria-label goes through t(), docs/sidebar-toggle-buttons)
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";

const calls: { cmd: string; args: any }[] = [];
let pendingLogResolvers: ((v: any) => void)[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "read_session_log") {
      return new Promise((resolve) => pendingLogResolvers.push(resolve));
    }
    // InfoPanel 新增技能段：list_skills 返回空列表（非 null，避免渲染崩溃）
    if (cmd === "list_skills") return [];
    return null;
  }),
}));

const prefs: import("../ipc/types").SessionPrefs = {
  approval_mode: "auto_edit",
  model_id: null,
  reasoning_effort: null,
};

function seedTabs(ids: string[]) {
  useSessions.setState({
    tabs: ids.map((id) => ({
      key: id,
      sessionId: id,
      workspace: "/tmp/ws",
      title: id,
      projectId: null,
      createdAt: "2026-09-01T00:00:00Z",
      prefs,
    })),
    activeKey: ids[0],
    projects: [],
  });
}

afterEach(() => {
  cleanup();
  pendingLogResolvers = [];
  calls.length = 0;
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

it("日志标签页未激活时不发起日志 IPC", () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  expect(screen.getByText("信息")).toBeTruthy();
  expect(calls.filter((c) => c.cmd.startsWith("read_") || c.cmd === "list_log_files")).toHaveLength(0);
});

it("激活日志标签页后会话视图加载日志内容", async () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  fireEvent.click(screen.getByText("日志"));
  // empty state shown while the request is pending
  expect(screen.getByText("（暂无日志）")).toBeTruthy();
  await waitFor(() => expect(pendingLogResolvers.length).toBe(1));
  pendingLogResolvers[0]({ content: "[2026-09-01T10:00:00] [INFO] run r1 开始", truncated: false });
  await waitFor(() => expect(screen.getByText(/run r1 开始/)).toBeTruthy());
  const call = calls.find((c) => c.cmd === "read_session_log");
  expect(call?.args?.sessionId).toBe("s1");
  expect(call?.args?.tailLines).toBe(300);
});

it("切会话后晚到的旧会话响应不覆盖新视图", async () => {
  seedTabs(["s1", "s2"]);
  render(<RightBar />);
  fireEvent.click(screen.getByText("日志"));
  await waitFor(() => expect(pendingLogResolvers.length).toBe(1));
  // switch to existing s2: refresh deps change → effects immediately fire a new request for s2 (no stacking within the 2s polling interval)
  useSessions.setState({ activeKey: "s2" });
  await waitFor(() => expect(pendingLogResolvers.length).toBe(2));
  // the old session (s1) response arrives late: must be dropped by the stale guard
  pendingLogResolvers[0]({ content: "OLD-SESSION-LOG", truncated: false });
  pendingLogResolvers[1]({ content: "SESSION-B-LOG", truncated: false });
  await waitFor(() => expect(screen.getByText("SESSION-B-LOG")).toBeTruthy());
  expect(screen.queryByText("OLD-SESSION-LOG")).toBeNull();
});
