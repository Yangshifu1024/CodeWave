// RightBar 信息页签·项目目录段：文件管理器图标按钮（open_dir IPC 派发 + 目标目录取值分支）
// mock 记录 cmd/args 断言派发，不真开文件管理器
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // mounting RightBar directly requires explicit i18next init (collapse button aria-label goes through t(), docs/sidebar-toggle-buttons)
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";

const invoked: { cmd: string; args: Record<string, unknown> }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "list_skills") return [];
    if (cmd === "list_editors" || cmd === "quota_snapshots") return [];
    invoked.push({ cmd, args: args ?? {} });
    return null;
  }),
}));

function seedTab(projectId: string | null, directory?: string) {
  useSessions.setState({
    tabs: [
      {
        key: "s1",
        sessionId: "s1",
        workspace: "/tmp/ws",
        title: "右栏目录",
        projectId,
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: projectId && directory ? [{ id: projectId, name: "demo", directory, data_dir: null, created_at: "2026-09-08T00:00:00Z" }] : [],
  });
}

afterEach(() => {
  cleanup();
  invoked.length = 0;
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

describe("RightBar 项目目录·文件管理器按钮", () => {
  it("项目会话：点击图标按钮派发 open_dir，目标为主目录 project.directory", async () => {
    seedTab("p1", "D:/demo/project");
    render(
      <AntApp>
        <RightBar />
      </AntApp>,
    );
    const btn = await screen.findByRole("button", { name: "在文件管理器中打开" });
    fireEvent.click(btn);
    await waitFor(() => {
      const call = invoked.find((c) => c.cmd === "open_dir");
      expect(call).toBeTruthy();
      expect(call!.args.path).toBe("D:/demo/project");
    });
  });

  it("临时会话：点击图标按钮派发 open_dir，目标为工作区 tab.workspace", async () => {
    seedTab(null);
    render(
      <AntApp>
        <RightBar />
      </AntApp>,
    );
    const btn = await screen.findByRole("button", { name: "在文件管理器中打开" });
    fireEvent.click(btn);
    await waitFor(() => {
      const call = invoked.find((c) => c.cmd === "open_dir");
      expect(call).toBeTruthy();
      expect(call!.args.path).toBe("/tmp/ws");
    });
  });
});
