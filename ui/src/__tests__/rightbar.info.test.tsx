// 右栏信息页结构变更（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）：
// ① 数据目录行移除；② 「会话」段（开始时间）移除；③ 技能/当前计划可折叠且折叠态记得住。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n";
import RightBar from "../features/shell/RightBar";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { COLLAPSED_SECTIONS_KEY, EXPANDED_QUOTA_KEY, PREFERRED_EDITOR_KEY, readCollapsedSections, readExpandedQuotaProviders, readPreferredEditor } from "../utils/rightbarPrefs";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_skills")
      return [
        { name: "demo", description: "项目技能", whenToUse: "", origin: "/tmp/ws/.codewave/skills/demo/SKILL.md" },
      ];
    // 新命令显式给空列表（后端契约是数组；返回 null 会误导组件）
    if (cmd === "list_editors" || cmd === "quota_snapshots") return [];
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
        title: "右栏信息",
        projectId: "p1",
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: [
      { id: "p1", name: "demo", directory: "D:/demo/project", data_dir: "D:/demo/project/.codewave", created_at: "2026-09-08T00:00:00Z" },
    ],
  });
  useUi.setState({ rightBarOpen: true, rbTab: "info" });
}

const renderBar = () =>
  render(
    <AntApp>
      <RightBar />
    </AntApp>,
  );

afterEach(() => {
  cleanup();
  localStorage.clear();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
  useRun.setState({ tabs: {} } as any);
});

describe("右栏信息页：数据目录行与会话段移除", () => {
  it("项目目录段只剩主目录路径，不渲染数据目录行", async () => {
    seed();
    renderBar();
    await screen.findByText("D:/demo/project");
    const info = document.querySelector(".rb-info-scroll")!;
    // 数据目录（.codewave 路径）不得出现；结构钉子：项目目录段只有一个路径行、没有 dim 副行
    expect(info.textContent).not.toContain(".codewave");
    const projectSection = info.querySelector(".rb-section")!;
    expect(projectSection.querySelectorAll(".rb-path")).toHaveLength(1);
    expect(projectSection.querySelectorAll(".rb-dim")).toHaveLength(0);
  });

  it("「会话」段（开始时间）整体移除，旧段的 .rb-line 结构与文案都不再出现", async () => {
    seed();
    renderBar();
    await screen.findByText("D:/demo/project");
    const info = document.querySelector(".rb-info-scroll")!;
    expect(screen.queryByText("开始时间")).toBeNull();
    // 老实现里「会话」段是唯一的 .rb-line；它的消失是结构级证据（强于字面量断言）
    expect(info.querySelectorAll(".rb-line")).toHaveLength(0);
  });
});

describe("右栏信息页：技能 / 当前计划折叠", () => {
  it("默认展开，点击折叠后写 localStorage，重挂载仍保持折叠", async () => {
    seed();
    useRun.setState({
      tabs: { s1: { todos: [{ title: "接入额度段", status: "in_progress" }] } },
    } as any);
    const { unmount } = renderBar();

    const header = (label: string) =>
      screen.getByText(label).closest(".ant-collapse-header") as HTMLElement;

    await waitFor(() => expect(header("技能").getAttribute("aria-expanded")).toBe("true"));
    expect(header("当前计划").getAttribute("aria-expanded")).toBe("true");
    expect(localStorage.getItem(COLLAPSED_SECTIONS_KEY)).toBeNull();

    fireEvent.click(header("技能"));
    await waitFor(() => expect(header("技能").getAttribute("aria-expanded")).toBe("false"));
    // 计划段不受影响
    expect(header("当前计划").getAttribute("aria-expanded")).toBe("true");
    expect(JSON.parse(localStorage.getItem(COLLAPSED_SECTIONS_KEY)!)).toEqual(["skills"]);

    unmount();
    renderBar();
    await waitFor(() =>
      expect(
        screen.getByText("技能").closest(".ant-collapse-header")!.getAttribute("aria-expanded"),
      ).toBe("false"),
    );
  });

  it("无计划时只渲染技能一段（不做「折叠了不存在的段」）", async () => {
    seed();
    renderBar();
    await screen.findByText("技能");
    expect(screen.queryByText("当前计划")).toBeNull();
  });
});

describe("右栏偏好读盘容错", () => {
  afterEach(() => {
    cleanup();
    localStorage.clear();
  });

  it("localStorage 非法值一律回默认，不抛错", () => {
    localStorage.setItem(COLLAPSED_SECTIONS_KEY, "not-json");
    expect([...readCollapsedSections()]).toEqual([]);
    // 合法元素保留、非法元素剔除
    localStorage.setItem(COLLAPSED_SECTIONS_KEY, JSON.stringify(["skills", 42, null, "plan"]));
    expect([...readCollapsedSections()].sort()).toEqual(["plan", "skills"]);
    // 非数组 / 非字符串都不得让额度家展开态崩掉
    localStorage.setItem(EXPANDED_QUOTA_KEY, JSON.stringify("nope"));
    expect([...readExpandedQuotaProviders()]).toEqual([]);
    localStorage.setItem(EXPANDED_QUOTA_KEY, JSON.stringify(["deepseek", 7]));
    expect([...readExpandedQuotaProviders()]).toEqual(["deepseek"]);
    // 空白编辑器记忆等同未选过
    localStorage.setItem(PREFERRED_EDITOR_KEY, "   ");
    expect(readPreferredEditor()).toBeNull();
  });
});
