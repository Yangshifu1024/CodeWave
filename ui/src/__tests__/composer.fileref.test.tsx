// 文件引用入口（[docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)）：
// 附件按钮选文件 → 非图片插入 `@路径` 引用；图片走附件通道；项目外路径先弹放行确认框。
// Composer 独立挂载（不依赖 AppShell），与 composer.paste 同一套环境种子。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Composer 独立挂载必须先初始化 i18next（文案全走 t()）

const ipcMock = vi.hoisted(() => ({
  selectDocumentFiles: vi.fn(async () => [] as string[]),
  checkExternalPath: vi.fn(async (_sid: string, _p: string) => ({ inside: true, dir: "", ref: "" })),
  allowExternalDir: vi.fn(async () => [] as string[]),
  readWorkspaceFileBase64: vi.fn(async () => ({ path: "", size: 0, content: "" })),
  listSkills: vi.fn(async () => []),
  searchWorkspacePaths: vi.fn(async () => []),
  compactSession: vi.fn(async () => null),
  startChat: vi.fn(async () => "ok"),
  cancelRun: vi.fn(async () => null),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import Composer from "../features/chat/Composer";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function seedEnv() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
          headers: [],
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
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
      projectId: null, createdAt: "2026-08-30T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null,
      compacting: false,
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [] } };
  });
}

/** 打开「+」菜单并点「添加附件」；选文件的行为由 ipc.selectDocumentFiles 的 mock 决定。 */
async function clickAttach() {
  fireEvent.click(document.querySelector(".toolbar-left .ant-btn") as HTMLElement);
  const item = await waitFor(() => {
    const el = Array.from(document.querySelectorAll(".ant-dropdown-menu-item")).find((o) =>
      (o.textContent ?? "").includes("附件"),
    );
    expect(el).toBeTruthy();
    return el as HTMLElement;
  });
  fireEvent.click(item);
}

const draftText = () => useRun.getState().drafts["s1"]?.text ?? "";

/** 按去空白后的文本找按钮：antd 会在两个字的按钮里插空格（「取 消」）。 */
function buttonByText(label: string): HTMLElement {
  const el = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === label,
  );
  expect(el, `按钮「${label}」未找到`).toBeTruthy();
  return el as HTMLElement;
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
  ipcMock.selectDocumentFiles.mockResolvedValue([]);
  ipcMock.checkExternalPath.mockImplementation(async () => ({ inside: true, dir: "", ref: "" }));
});

describe("Composer 文件引用入口", () => {
  it("选中非图片文件：插入 @相对路径，不产生图片附件", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/report.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "report.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(draftText()).toContain("@report.xlsx"));
    expect(document.querySelector(".attach-chip")).toBeFalsy();
  });

  it("选中图片路径：读成附件缩略图，不进文本引用", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/chart.png"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "chart.png" });
    ipcMock.readWorkspaceFileBase64.mockResolvedValue({ path: "chart.png", size: 3, content: "aGk=" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.querySelectorAll(".attach-chip").length).toBe(1));
    expect(draftText()).toBe("");
  });

  it("项目外文件：先弹放行确认框；选「仅此一次」→ 不写入项目设置并插入引用", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" })
      .mockResolvedValue({ inside: true, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    // 确认框出现，且列出了所在目录
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    fireEvent.click(screen.getByText("仅此一次"));

    await waitFor(() => expect(draftText()).toContain("@/elsewhere/budget.xlsx"));
    expect(ipcMock.allowExternalDir).toHaveBeenCalledWith("s1", "/elsewhere", false);
  });

  it("项目外文件选「始终允许这个目录」：persist=true 写进项目设置", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" })
      .mockResolvedValue({ inside: true, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    fireEvent.click(screen.getByText("始终允许这个目录"));

    await waitFor(() => expect(ipcMock.allowExternalDir).toHaveBeenCalledWith("s1", "/elsewhere", true));
  });

  it("在放行确认框里取消：不放行也不插入引用", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    // antd 会在两个字的按钮里插空格（「取 消」），所以按去空白后的文本找
    fireEvent.click(buttonByText("取消"));

    await new Promise((r) => setTimeout(r, 30));
    expect(ipcMock.allowExternalDir).not.toHaveBeenCalled();
    expect(draftText()).toBe("");
  });
});
