// 设置页技能页签：来源短标签 + 重新加载（reload_skills 清缓存重扫）+ 托管技能删除（delete_skill + Popconfirm）。
// 挂载方式与 shell.settings.test.tsx 同源（standalone + useUi 控制开关），两字按钮按去空白文本匹配。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n";
import SettingsPage from "../features/panels/SettingsPage";
import { useUi } from "../stores/ui";
import { useSettings } from "../stores/settings";
import type { ConfigState } from "../ipc/types";

function makeConfig(overrides: Partial<ConfigState> = {}): ConfigState {
  return {
    schema_version: 2,
    providers: [],
    active_model_id: null,
    proxy: null,
    network: { allow_private_network: false },
    compact_threshold: 0.6,
    compact_timeout_seconds: 180,
    approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false, command_allowlist: [] },
    validation: { python: true, rust: true, typescript: true, go: true, json: true },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    ...overrides,
  };
}

// 三来源覆盖：内置（不可删/显示「内置」）、托管目录（可删/短标签取技能目录名）、compat 目录（不可删）
const fixtureSkills = [
  { name: "builtin-skill", description: "内置技能", whenToUse: "", origin: "<builtin>", deletable: false },
  { name: "demo", description: "项目技能", whenToUse: "输入 /demo 时", origin: "C:/proj/.codewave/skills/demo-dir/SKILL.md", deletable: true },
  { name: "compat", description: "兼容技能", whenToUse: "", origin: "C:/ws/.claude/skills/compat-dir/SKILL.md", deletable: false },
];

let calls: string[] = [];

async function baseInvoke(cmd: string, args?: any) {
  calls.push(args ? `${cmd}:${JSON.stringify(args)}` : cmd);
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": return null;
    case "list_available_shells": return [];
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "list_skills": return JSON.parse(JSON.stringify(fixtureSkills));
    case "reload_skills": return JSON.parse(JSON.stringify([...fixtureSkills, { name: "fresh", description: "新技能", whenToUse: "", origin: "C:/ws/.codewave/skills/fresh/SKILL.md", deletable: true }]));
    case "delete_skill": return null;
    default: throw new Error(`unmocked command: ${cmd}`);
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class {
    onmessage: any = null;
  },
}));

async function invokeMock() {
  return (await import("@tauri-apps/api/core")).invoke as any;
}

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  useUi.setState({ settingsOpen: false, settingsTab: "general" });
  useSettings.setState({ config: null, loaded: false });
  calls = [];
});

function buttonByText(text: string): HTMLElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLElement;
}

async function openSkillsTab() {
  useSettings.setState({ config: makeConfig(), loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "skills" });
  render(
    <AntApp>
      <SettingsPage />
    </AntApp>,
  );
  await waitFor(() => expect(screen.getByText("demo")).toBeTruthy());
}

describe("设置页技能页签", () => {
  it("每行渲染来源短标签：内置显示「内置」，目录形态取技能目录名并悬浮完整 origin", async () => {
    await openSkillsTab();
    // 内置：显示「内置」标签
    expect(document.body.textContent ?? "").toContain("内置");
    // 目录形态：短标签取技能目录名（demo-dir / compat-dir），非路径尾段 SKILL.md
    const demoOrigin = screen.getByText("demo-dir");
    expect(demoOrigin.getAttribute("title")).toBe("C:/proj/.codewave/skills/demo-dir/SKILL.md");
    expect(screen.getByText("compat-dir")).toBeTruthy();
  });

  it("删除按钮只出现在 deletable 行（托管目录），compat/内置不显示", async () => {
    await openSkillsTab();
    const delBtns = document.querySelectorAll('button[aria-label="删除技能"]');
    expect(delBtns).toHaveLength(1);
  });

  it("点击重新加载：调用 reload_skills 且返回列表立即渲染（新技能可见）", async () => {
    await openSkillsTab();
    fireEvent.click(buttonByText("重新加载"));
    await waitFor(() => expect(calls.some((c) => c.startsWith("reload_skills"))).toBe(true));
    // 重载返回的最新列表渲染（新技能出现；按描述定位避免与来源短标签同名撞车）
    await waitFor(() => expect(screen.getByText("新技能")).toBeTruthy());
    // 成功提示
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已重新加载，共 4 个技能"));
  });

  it("删除：Popconfirm 列明路径，确认后调用 delete_skill 并移除该行", async () => {
    await openSkillsTab();
    fireEvent.click(document.querySelector('button[aria-label="删除技能"]') as HTMLElement);
    // Popconfirm 出现：标题含技能名，description 列明完整路径
    await waitFor(() => expect(document.body.textContent ?? "").toContain("删除技能「demo」？"));
    expect(document.body.textContent ?? "").toContain("C:/proj/.codewave/skills/demo-dir/SKILL.md");
    // antd 默认 locale（无 ConfigProvider）：确认按钮 OK
    const ok = Array.from(document.querySelectorAll(".ant-popover button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === "OK",
    ) as HTMLElement;
    expect(ok).toBeTruthy();
    fireEvent.click(ok);
    await waitFor(() =>
      expect(calls.some((c) => c.startsWith("delete_skill:") && c.includes('"name":"demo"'))).toBe(true),
    );
    // 行移除 + 成功提示
    await waitFor(() => expect(screen.queryByText("demo-dir")).toBeNull());
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已删除技能：demo"));
  });

  it("删除失败：报错提示且该行保留（不乐观移除）", async () => {
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "delete_skill") throw new Error("删除被后端拒绝");
      return baseInvoke(cmd, args);
    });
    await openSkillsTab();
    fireEvent.click(document.querySelector('button[aria-label="删除技能"]') as HTMLElement);
    const ok = await waitFor(() =>
      Array.from(document.querySelectorAll(".ant-popover button")).find(
        (b) => (b.textContent ?? "").replace(/\s/g, "") === "OK",
      ) as HTMLElement,
    );
    fireEvent.click(ok);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("删除失败"));
    // 失败路径：行保留（deletable 标记仍在，不误删 UI 状态）
    expect(screen.getByText("demo-dir")).toBeTruthy();
  });
});
