// 「在编辑器中打开」按钮（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
// 按钮恒显示通用编辑器图标（CodeOutlined）+ caret（CaretDownOutlined），不展示当前选中编辑器名称；
// 点击展开 Dropdown 列出候选编辑器，单击即用该编辑器打开当前目录并记忆为新默认；
// 一个编辑器都没检测到则不渲染控件（文件管理器按钮仍在）。
// 顶部对齐修复后行为零变化（编辑器按钮挂在右栏，与标题栏无关）。
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
        title: "编辑器按钮",
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

// 打开下拉：antd 6 Dropdown trigger="click" — 直接 click 按钮即可（不需要 mouseDown）
async function openDropdown() {
  const btn = document.querySelector<HTMLElement>(".rb-editor-btn")!;
  fireEvent.click(btn);
  return waitFor(() =>
    expect(document.querySelector(".ant-dropdown-menu")).toBeTruthy(),
  );
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

describe("右栏「在编辑器中打开」按钮", () => {
  it("按钮恒显示通用编辑器图标 + caret（不含任何编辑器名），且不自动打开", async () => {
    seed();
    renderBar();
    await waitFor(() => expect(document.querySelector(".rb-editor-btn")).toBeTruthy());
    const btn = document.querySelector(".rb-editor-btn")!;
    // 按钮文本必须只含通用图标 + caret，绝不携带编辑器名称
    expect(btn.textContent ?? "").not.toContain("VS Code");
    expect(btn.textContent ?? "").not.toContain("Zed");
    expect(btn.querySelector(".rb-editor-btn-icon")).toBeTruthy();
    expect(btn.querySelector(".rb-editor-btn-caret")).toBeTruthy();
    // 记忆未设值 → data-active 不挂载
    expect(btn.getAttribute("data-active")).toBeNull();
    // 不会自动派发打开
    expect(calls.find((c) => c.cmd === "open_in_editor")).toBeUndefined();
  });

  it("点击 dropdown 中的编辑器项：用该编辑器打开目录并记忆为新默认；按钮文字仍为通用图标", async () => {
    seed();
    renderBar();
    await waitFor(() => expect(document.querySelector(".rb-editor-btn")).toBeTruthy());
    await openDropdown();

    // antd Dropdown 菜单项选择器
    const zedItem = Array.from(
      document.querySelectorAll<HTMLElement>(".ant-dropdown-menu-item"),
    ).find((o) => (o.textContent ?? "").includes("Zed"));
    expect(zedItem).toBeTruthy();
    fireEvent.click(zedItem!);

    await waitFor(() => {
      const call = calls.find((c) => c.cmd === "open_in_editor");
      expect(call).toBeTruthy();
      expect(call!.args.editorId).toBe("zed");
      expect(call!.args.path).toBe("D:/demo/project");
    });
    expect(localStorage.getItem(PREFERRED_EDITOR_KEY)).toBe("zed");

    // 单击触发后按钮文字依旧不出现编辑器名（**始终**显示通用图标 + caret）
    const btn = document.querySelector(".rb-editor-btn")!;
    expect(btn.textContent ?? "").not.toContain("Zed");
    expect(btn.textContent ?? "").not.toContain("VS Code");
    // 记忆后 caret 挂 data-active=true（视觉提示）
    expect(btn.getAttribute("data-active")).toBe("true");
  });

  it("上次选择仍可用时：按钮挂 data-active=true（视觉提示用，不展示名称）", async () => {
    localStorage.setItem(PREFERRED_EDITOR_KEY, "zed");
    seed();
    renderBar();
    await waitFor(() => expect(document.querySelector(".rb-editor-btn")).toBeTruthy());
    const btn = document.querySelector(".rb-editor-btn")!;
    expect(btn.getAttribute("data-active")).toBe("true");
    expect(btn.textContent ?? "").not.toContain("Zed");
  });

  it("一个编辑器都没检测到时不渲染控件（文件管理器按钮仍在）", async () => {
    editors = [];
    seed();
    renderBar();
    await waitFor(() => expect(screen.getByLabelText("在文件管理器中打开")).toBeTruthy());
    expect(document.querySelector(".rb-editor-btn")).toBeNull();
  });
});
