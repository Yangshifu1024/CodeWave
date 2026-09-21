// RightBar Files tab ([docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)): count always visible / list rendering / click opens the viewer / deleted files greyed out and unclickable;
// plus the run store's writeTick write signal (incremented on create/edit success, not on failure or other tools)
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // RightBar mounted standalone must init i18next explicitly (collapse button aria-label goes through t(), docs/sidebar-toggle-buttons)
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useUi } from "../stores/ui";

const calls: { cmd: string; args: any }[] = [];

const fixture = [
  {
    path: "/ws/.codewave/tasks/20260901-010101-auth/requirement.md",
    first_op: "create", last_op: "create",
    first_at: "2026-09-01T01:01:01Z", last_at: "2026-09-01T01:01:01Z",
    count: 1, exists: true, size: 100,
  },
  {
    path: "/ws/docs/架构图.png",
    first_op: "create", last_op: "create",
    first_at: "2026-09-01T01:02:01Z", last_at: "2026-09-01T01:02:01Z",
    count: 1, exists: true, size: 2048,
  },
  {
    path: "/ws/old-note.md",
    first_op: "create", last_op: "edit",
    first_at: "2026-09-01T01:03:01Z", last_at: "2026-09-01T01:04:01Z",
    count: 2, exists: false, size: 0,
  },
];

// list_session_files response control: return fixture directly / defer (hanging, for the stale-response guard case on session switch)
let fileMode: "fixture" | "defer" = "fixture";
let filePayload: any = fixture;
let fileResolvers: ((v: any) => void)[] = [];

// [docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)：回退入口的备份清单（默认空 = 没备份）
let backupsPayload: any[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "list_session_files") {
      if (fileMode === "defer") return new Promise((resolve) => fileResolvers.push(resolve));
      return filePayload;
    }
    if (cmd === "list_document_backups") return backupsPayload;
    if (cmd === "restore_document_backup") {
      return { path: args?.path ?? "", restoredFrom: args?.backupPath ?? "", currentBackup: null, size: 9 };
    }
    if (cmd === "read_workspace_file") return { path: args?.path ?? "", size: 12, content: "# Hello" };
    if (cmd === "read_workspace_file_base64") return { path: args?.path ?? "", size: 4, content: "aGk=" };
    // InfoPanel 段：list_skills / list_editors / quota_snapshots 一律给空数组（后端契约是数组，null 会误导组件）
    if (cmd === "list_skills" || cmd === "list_editors" || cmd === "quota_snapshots") return [];
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
  calls.length = 0;
  fileMode = "fixture";
  filePayload = fixture;
  fileResolvers = [];
  backupsPayload = [];
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  // Reset the right-sidebar toggle singleton ([docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md)): prevent collapsed state leaking across cases
  useUi.setState({ rightBarOpen: true });
  localStorage.removeItem("ws_right_bar_open");
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
});

function openFilesTab() {
  fireEvent.click(screen.getByText(/^文件/));
}

it("Files 标签页：tab 计数常显 + 列表渲染（未激活也拉取计数）", async () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  // Defaults to the info tab, but the count is already fetched (tab label always visible)
  await waitFor(() => expect(screen.getByText("文件 3")).toBeTruthy());
  openFilesTab();
  expect(await screen.findByText("requirement.md")).toBeTruthy();
  expect(screen.getByText("架构图.png")).toBeTruthy();
  expect(screen.getByText("old-note.md")).toBeTruthy();
  expect(screen.getByText("共 3 个")).toBeTruthy();
});

it("点击 md 文件：走文本通道并在弹窗内渲染 markdown", async () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  await waitFor(() => expect(screen.getByText("文件 3")).toBeTruthy());
  openFilesTab();
  fireEvent.click(await screen.findByText("requirement.md"));
  await waitFor(() => expect(screen.getByText("Hello")).toBeTruthy());
  const call = calls.find((c) => c.cmd === "read_workspace_file");
  expect(call?.args?.sessionId).toBe("s1");
  expect(call?.args?.path).toContain("requirement.md");
});

it("点击图片文件：走 base64 通道", async () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  await waitFor(() => expect(screen.getByText("文件 3")).toBeTruthy());
  openFilesTab();
  fireEvent.click(await screen.findByText("架构图.png"));
  await waitFor(() => {
    const call = calls.find((c) => c.cmd === "read_workspace_file_base64");
    expect(call?.args?.path).toContain("架构图.png");
  });
});

it("已删除文件灰显且不可打开", async () => {
  seedTabs(["s1"]);
  render(<RightBar />);
  await waitFor(() => expect(screen.getByText("文件 3")).toBeTruthy());
  openFilesTab();
  const gone = await screen.findByText("old-note.md");
  expect(gone.closest(".rb-file")?.classList.contains("rb-file-gone")).toBe(true);
  fireEvent.click(gone);
  await new Promise((r) => setTimeout(r, 20));
  expect(calls.some((c) => c.cmd.startsWith("read_"))).toBe(false);
});

it("切会话后晚到的旧产物响应不覆盖新列表", async () => {
  fileMode = "defer";
  seedTabs(["s1", "s2"]);
  render(<RightBar />);
  await waitFor(() => expect(fileResolvers.length).toBe(1));
  // Switch to s2: sessionId change → a new request for s2 fires immediately
  useSessions.setState({ activeKey: "s2" });
  await waitFor(() => expect(fileResolvers.length).toBe(2));
  // The old session's (s1) response arrives late: it must be dropped by the stale guard
  fileResolvers[0]([{ ...fixture[0], path: "/ws/old-session.md" }]);
  fileResolvers[1]([{ ...fixture[0], path: "/ws/new-session.md" }]);
  openFilesTab();
  expect(await screen.findByText("new-session.md")).toBeTruthy();
  expect(screen.queryByText("old-session.md")).toBeNull();
});

describe("文档回退入口", () => {
  /** 一条被本应用改过的表格：只有这种产物才会有备份。 */
  const xlsx = {
    path: "/ws/预算表.xlsx",
    first_op: "create", last_op: "edit",
    first_at: "2026-09-20T01:00:00Z", last_at: "2026-09-20T02:00:00Z",
    count: 2, exists: true, size: 4096,
  };

  async function openPanel(files: any[]) {
    filePayload = files;
    seedTabs(["s1"]);
    // 回退成功要弹提示，而提示走 App.useApp()（与仓库其他组件同一惯例）——
    // 在没包 <AntApp> 的上下文里 message 方法是 undefined，一调就抛，
    // 连后续的刷新也会跟着跳掉。所以这一组用例必须包上 AntApp。
    render(<AntApp><RightBar /></AntApp>);
    openFilesTab();
  }

  it("表格行上有回退按钮；列出版本、确认后真的回退并刷新列表", async () => {
    backupsPayload = [
      { path: "/data/tmp/document-backup/B-new.xlsx.bak", at: "2026-09-20T02:00:00Z", size: 4096 },
      { path: "/data/tmp/document-backup/A-old.xlsx.bak", at: "2026-09-20T01:00:00Z", size: 2048 },
    ];
    await openPanel([xlsx]);
    await screen.findByText("预算表.xlsx");

    const btn = document.querySelector(".rb-file-btn") as HTMLElement;
    expect(btn).toBeTruthy();
    fireEvent.click(btn);

    // 拉清单：路径与会话都传对了
    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "list_document_backups");
      expect(c?.args?.path).toBe("/ws/预算表.xlsx");
      expect(c?.args?.sessionId).toBe("s1");
    });
    // 两份版本可选，默认选中最新那份（清单新的在前）
    const radios = await waitFor(() => {
      const r = document.querySelectorAll(".ant-radio-wrapper");
      expect(r.length).toBe(2);
      return r;
    });
    expect((radios[0] as HTMLElement).textContent ?? "").toContain("4 KB");
    expect((radios[1] as HTMLElement).textContent ?? "").toContain("2 KB");

    // antd 会在两个字的按钮里插空格（「回 退」），所以按去空白后的文本找
    const ok = Array.from(document.querySelectorAll(".ant-modal-footer button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === "回退",
    ) as HTMLElement;
    expect(ok).toBeTruthy();
    const listCallsBefore = calls.filter((c) => c.cmd === "list_session_files").length;
    fireEvent.click(ok);

    await waitFor(() => {
      const c = calls.find((x) => x.cmd === "restore_document_backup");
      // 默认回退到最新那份备份
      expect(c?.args).toMatchObject({
        sessionId: "s1",
        path: "/ws/预算表.xlsx",
        backupPath: "/data/tmp/document-backup/B-new.xlsx.bak",
      });
    });
    // 回退改了体积与修改时间：成功后列表刷新一次
    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "list_session_files").length).toBeGreaterThan(listCallsBefore),
    );
    // 点回退不应把预览一并打开
    expect(calls.some((c) => c.cmd.startsWith("read_workspace_file"))).toBe(false);
  });

  it("没有备份：弹框里说明原因且确定键禁用，不发回退请求", async () => {
    backupsPayload = [];
    await openPanel([xlsx]);
    await screen.findByText("预算表.xlsx");
    fireEvent.click(document.querySelector(".rb-file-btn") as HTMLElement);

    await waitFor(() => expect(document.body.textContent ?? "").toContain("还没有备份"));
    const ok = Array.from(document.querySelectorAll(".ant-modal-footer button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === "回退",
    ) as HTMLButtonElement;
    expect(ok.disabled).toBe(true);
    fireEvent.click(ok);
    await new Promise((r) => setTimeout(r, 20));
    expect(calls.some((c) => c.cmd === "restore_document_backup")).toBe(false);
  });

  it("非文档产物（比如 markdown）不显示回退按钮", async () => {
    await openPanel([fixture[0]]);
    await screen.findByText("requirement.md");
    expect(document.querySelector(".rb-file-btn")).toBeNull();
  });

  it("已删除的文档行不显示回退按钮（没东西可回退）", async () => {
    backupsPayload = [{ path: "/data/tmp/document-backup/x.bak", at: "2026-09-20T02:00:00Z", size: 10 }];
    await openPanel([{ ...xlsx, exists: false }]);
    await screen.findByText("预算表.xlsx");
    expect(document.querySelector(".rb-file-btn")).toBeNull();
  });
});

describe("writeTick 写入信号", () => {
  const session = "s-files";
  const ev = (tool: string, ok: boolean) => ({
    session, run_id: "r", batch_id: "b", call_index: 0, call_key: "b:0",
    tool, args_preview: "{}",
    outcome: { ok, data: {} }, duration_ms: 1,
  });

  function seedBucket() {
    useRun.setState((s) => {
      s.tabs[session] = {
        items: [], running: true, streamGen: 0, ask: null, breakdown: null,
        todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
  }

  it("create/edit 成功递增；失败与其他工具不递增", () => {
    seedBucket();
    useRun.getState().onToolResult(session, ev("create", true) as any, true);
    expect(useRun.getState().tabs[session]!.writeTick).toBe(1);
    useRun.getState().onToolResult(session, ev("edit", true) as any, true);
    expect(useRun.getState().tabs[session]!.writeTick).toBe(2);
    useRun.getState().onToolResult(session, ev("create", false) as any, false);
    useRun.getState().onToolResult(session, ev("read", true) as any, true);
    expect(useRun.getState().tabs[session]!.writeTick).toBe(2);
  });
});
