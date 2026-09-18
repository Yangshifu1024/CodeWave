// 「在编辑器中打开」下拉（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
// 默认 = 第一个检测到的编辑器；改选即用该编辑器打开当前目录并记忆；一个都没检测到则不渲染控件。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { PREFERRED_EDITOR_KEY } from "../utils/rightbarPrefs";

const calls: { cmd: string; args: any }[] = [];
let editors: any[] = [
  { id: "vscode", name: "VS Code", path: "D:/App/Microsoft VS Code/Code.exe" },
  { id: "zed", name: "Zed", path: "D:/App/Zed/zed.exe" },
];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    calls.push({ cmd, args });
    if (cmd === "list_skills") return [];
    if (cmd === "list_editors") return editors;
    if (cmd === "quota_snapshots") return [];
    return null;
  }),
}));

function seed() {
  useSessions.setState({
    tabs: [
      {
        key: "s1",
        sessionId: "s1",
        workspace: "D:/demo/project",
        title: "编辑器下拉",
        projectId: null,
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: [],
  });
  useUi.setState({ rightBarOpen: true, rbTab: "info" });
}

const renderBar = () =>
  render(
    <AntApp>
      <RightBar />
    </AntApp>,
  );

async function openDropdown() {
  // antd 6.6：展开需对 .ant-select 根元素 mouseDown（`.ant-select-selector` 已不存在）
  const root = document.querySelector(".rb-editor-select")!;
  fireEvent.mouseDown(root);
  return waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy());
}

afterEach(() => {
  cleanup();
  calls.length = 0;
  editors = [
    { id: "vscode", name: "VS Code", path: "D:/App/Microsoft VS Code/Code.exe" },
    { id: "zed", name: "Zed", path: "D:/App/Zed/zed.exe" },
  ];
  localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

describe("右栏「在编辑器中打开」下拉", () => {
  it("默认显示第一个检测到的编辑器，且不自动打开", async () => {
    seed();
    renderBar();
    await waitFor(() =>
      expect(document.querySelector(".rb-editor-select")!.textContent).toContain("VS Code"),
    );
    expect(calls.find((c) => c.cmd === "open_in_editor")).toBeUndefined();
  });

  it("改选其他编辑器：用该编辑器打开当前工作区并记忆为新默认", async () => {
    seed();
    renderBar();
    await waitFor(() => expect(document.querySelector(".rb-editor-select")).toBeTruthy());
    await openDropdown();

    const zedOption = Array.from(document.querySelectorAll(".ant-select-item-option")).find((o) =>
      (o.textContent ?? "").includes("Zed"),
    ) as HTMLElement;
    fireEvent.click(zedOption);

    await waitFor(() => {
      const call = calls.find((c) => c.cmd === "open_in_editor");
      expect(call).toBeTruthy();
      expect(call!.args.editorId).toBe("zed");
      expect(call!.args.path).toBe("D:/demo/project");
    });
    expect(localStorage.getItem(PREFERRED_EDITOR_KEY)).toBe("zed");
  });

  it("上次选择仍可用时优先展示它（不再是列表第一个）", async () => {
    localStorage.setItem(PREFERRED_EDITOR_KEY, "zed");
    seed();
    renderBar();
    await waitFor(() =>
      expect(document.querySelector(".rb-editor-select")!.textContent).toContain("Zed"),
    );
  });

  it("一个编辑器都没检测到时不渲染控件（文件管理器按钮仍在）", async () => {
    editors = [];
    seed();
    renderBar();
    await waitFor(() => expect(screen.getByLabelText("在文件管理器中打开")).toBeTruthy());
    expect(document.querySelector(".rb-editor-select")).toBeNull();
  });
});
