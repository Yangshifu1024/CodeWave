// RightBar 信息页技能段（[docs/slash-skills-and-dollar-agents](../../../docs/slash-skills-and-dollar-agents.md)）：
// 列表渲染（后端分桶排序原样展示）+ 点击技能行弹详情（get_skill 正文渲染）
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import "../i18n"; // mounting RightBar directly requires explicit i18next init (collapse button aria-label goes through t(), docs/sidebar-toggle-buttons)
import RightBar from "../features/shell/RightBar";
import { useSessions } from "../stores/sessions";

// 注：originLabel 现取技能目录名（目录形态 SKILL.md 上一段），与技能行名可能同文本——
// 定位技能行用 .rb-skill-name 而非 getByText（避免撞上 .rb-skill-origin 标签）

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    if (cmd === "list_skills")
      return [
        { name: "zeta", description: "compat 技能", whenToUse: "", origin: "/tmp/ws/.claude/skills/zeta/SKILL.md" },
        { name: "demo", description: "项目技能", whenToUse: "输入 /demo 时", origin: "/tmp/ws/.codewave/skills/demo/SKILL.md" },
      ];
    if (cmd === "get_skill")
      return {
        meta: { name: "demo", description: "项目技能", whenToUse: "输入 /demo 时", origin: "/tmp/ws/.codewave/skills/demo/SKILL.md" },
        body: "# DEMO BODY",
      };
    return null;
  }),
}));

function seedTab() {
  useSessions.setState({
    tabs: [
      {
        key: "s1",
        sessionId: "s1",
        workspace: "/tmp/ws",
        title: "右栏技能",
        projectId: null,
        createdAt: "2026-09-08T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      },
    ],
    activeKey: "s1",
    projects: [],
  });
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, projects: [] });
});

describe("RightBar 技能段", () => {
  it("技能列表渲染，点击行弹出详情正文", async () => {
    seedTab();
    render(<RightBar />);
    // 列表渲染（后端已按 内置>项目>全局>其他 排序，前端原样展示）
    const skillNames = await waitFor(() => {
      const els = Array.from(document.querySelectorAll(".rb-skill-name"));
      expect(els).toHaveLength(2);
      return els;
    });
    expect(skillNames.map((el) => el.textContent).sort()).toEqual(["demo", "zeta"]);
    // 点击项目技能行 → 详情弹层出现，SKILL.md 正文渲染
    fireEvent.click(skillNames.find((el) => el.textContent === "demo") as HTMLElement);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("DEMO BODY"));
    expect(document.body.textContent ?? "").toContain("触发时机");
  });

  it("详情「使用」按钮派发 ws:composer-insert（/name 追加输入框）并关闭弹层", async () => {
    seedTab();
    render(<RightBar />);
    fireEvent.click(
      (await waitFor(() => {
        const el = Array.from(document.querySelectorAll(".rb-skill-name")).find(
          (n) => n.textContent === "demo",
        );
        expect(el).toBeTruthy();
        return el as HTMLElement;
      })),
    );
    await waitFor(() => expect(document.body.textContent ?? "").toContain("DEMO BODY"));
    const inserted: string[] = [];
    const onInsert = (e: Event) => inserted.push((e as CustomEvent<{ text: string }>).detail.text);
    window.addEventListener("ws:composer-insert", onInsert);
    try {
      // antd 两字按钮插空格（「使 用」）：去空白后匹配
      const useBtn = Array.from(document.querySelectorAll(".ant-modal button")).find(
        (b) => (b.textContent ?? "").replace(/\s/g, "").includes("使用"),
      ) as HTMLElement;
      fireEvent.click(useBtn);
      // 契约：/name 以 ws:composer-insert 追加输入框（弹层关闭由 onClose 驱动，happy-dom 下
      // antd Modal 过渡动画不触发、DOM 不卸载，不作断言）
      await waitFor(() => expect(inserted).toEqual(["/demo "]));
    } finally {
      window.removeEventListener("ws:composer-insert", onInsert);
    }
  });
});
