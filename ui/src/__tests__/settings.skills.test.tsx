import { configSnapshot, configSaved } from "./configFixtures";
// 审计清单独立于启用索引；删除绑定真实来源和版本，重扫展示下层回退。
// 挂载方式与 shell.settings.test.tsx 同源（standalone + useUi 控制开关），两字按钮按去空白文本匹配。
import { describe, it, expect, vi, afterEach } from "vitest";
import { act, render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n";
import SettingsPage from "../features/panels/SettingsPage";
import { useUi } from "../stores/ui";
import { useSettings } from "../stores/settings";
import type { ConfigState, SkillInspection, SkillMeta } from "../ipc/types";
import SkillsPanel from "../features/panels/SkillsPanel";

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
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
    ui: { font_size_sans: 15, font_size_mono: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    ...overrides,
  };
}

// 三来源覆盖：内置（不可删/显示「内置」）、托管目录（可删/短标签取技能目录名）、compat 目录（不可删）
const fixtureSkills: SkillMeta[] = [
  { name: "builtin-skill", description: "内置技能", whenToUse: "", origin: "<builtin>", deletable: false },
  { name: "demo", description: "项目技能", whenToUse: "输入 /demo 时", origin: "C:/proj/.codewave/skills/demo-dir/SKILL.md", deletable: true,
    resource: { source: "project_managed", identity: "C:/proj/.codewave/skills/demo-dir/SKILL.md", version: "project-v1" } },
  { name: "compat", description: "兼容技能", whenToUse: "", origin: "C:/ws/.claude/skills/compat-dir/SKILL.md", deletable: false },
];

let calls: string[] = [];
let deleted = false;
let configuredDisabled: string[] = [];
const fallback: SkillMeta = { name: "demo", description: "下层技能", whenToUse: "", origin: "<builtin>", deletable: false,
  resource: { source: "builtin", identity: "builtin:skill:demo", version: "builtin-v1" } };

function inspection(reload = false): SkillInspection {
  const entries = structuredClone(fixtureSkills).map((meta) => ({ name: meta.name, disabled: configuredDisabled.includes(meta.name),
    candidates: meta.name === "demo" ? deleted ? [{ meta: fallback, selected: true }] : [{ meta, selected: true }, { meta: fallback, selected: false }] : [{ meta, selected: true }] }));
  if (reload && !deleted) entries.push({ name: "fresh", disabled: false, candidates: [{ meta: { name: "fresh", description: "新技能", whenToUse: "", origin: "<builtin>", deletable: false }, selected: true }] });
  return { entries, issues: [] };
}

async function baseInvoke(cmd: string, args?: any) {
  calls.push(args ? `${cmd}:${JSON.stringify(args)}` : cmd);
  switch (cmd) {
    case "get_config": return configSnapshot(makeConfig());
    case "save_config": configuredDisabled = args.config.disabled_skills; return configSaved(args?.config);
    case "list_available_shells": return [];
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "inspect_instruction_resources": return { resources: [], issues: [] };
    case "inspect_skills": return inspection(args?.reload);
    case "delete_skill": deleted = true; return null;
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
  useUi.setState({ settingsOpen: false, settingsTab: "appearance" });
  useSettings.setState({ config: null, snapshot: null, loaded: false, rolePending: {}, roleErrors: {}, roleRetry: {} });
  calls = [];
  deleted = false;
  configuredDisabled = [];
});

function buttonByText(text: string): HTMLElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLElement;
}

async function openSkillsTab(config = makeConfig()) {
  configuredDisabled = config.disabled_skills;
  useSettings.setState({ config, snapshot: configSnapshot(config), loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "skills" });
  render(
    <AntApp>
      <SettingsPage />
    </AntApp>,
  );
  await waitFor(() => expect(screen.getByText("demo")).toBeTruthy());
}

describe("设置页技能页：禁用清单", () => {
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

  it("点击重新加载：绕过缓存重查审计清单，新技能立即可见", async () => {
    await openSkillsTab();
    fireEvent.click(buttonByText("重新加载"));
    await waitFor(() => expect(calls.some((c) => c.startsWith("inspect_skills:") && c.includes('"reload":true'))).toBe(true));
    // 重载返回的最新列表渲染（新技能出现；按描述定位避免与来源短标签同名撞车）
    await waitFor(() => expect(screen.getByText("新技能")).toBeTruthy());
    // 成功提示
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已重新加载，共 4 个技能"));
  });

  it("删除：确认目录语义，提交来源和版本，重查后展示真实下层回退", async () => {
    await openSkillsTab();
    fireEvent.click(document.querySelector('button[aria-label="删除技能"]') as HTMLElement);
    // Popconfirm 出现：标题含技能名，description 列明完整路径
    await waitFor(() => expect(document.body.textContent ?? "").toContain("删除技能「demo」？"));
    expect(document.body.textContent ?? "").toContain("C:/proj/.codewave/skills/demo-dir/SKILL.md");
    expect(document.body.textContent ?? "").toContain("删除整个技能目录及内容");
    expect(document.body.textContent ?? "").toContain("单文件形态只删除该文件");
    // antd 默认 locale（无 ConfigProvider）：确认按钮 OK
    const ok = Array.from(document.querySelectorAll(".ant-popover button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === "OK",
    ) as HTMLElement;
    expect(ok).toBeTruthy();
    fireEvent.click(ok);
    await waitFor(() =>
      expect(calls).toContain('delete_skill:{"sessionId":null,"name":"demo","expectedSource":"C:/proj/.codewave/skills/demo-dir/SKILL.md","expectedVersion":"project-v1"}'),
    );
    // 行移除 + 成功提示
    await waitFor(() => expect(screen.queryByText("demo-dir")).toBeNull());
    expect(screen.getByText("下层技能")).toBeTruthy();
    expect(screen.getByText("demo")).toBeTruthy();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已删除技能：demo"));
  });

  it("技能为空时显示「暂无技能」空态（批④ 修的缺陷：此前显示「暂无会话」）", async () => {
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) =>
      cmd === "inspect_skills" ? { entries: [], issues: [] } : baseInvoke(cmd, args),
    );
    useSettings.setState({ config: makeConfig(), snapshot: configSnapshot(makeConfig()), loaded: true });
    useUi.setState({ settingsOpen: true, settingsTab: "skills" });
    render(
      <AntApp>
        <SettingsPage />
      </AntApp>,
    );
    // 空态文案是「暂无技能」（settings.skillsEmpty）；不得退回借用 sessions.empty 的「暂无会话」
    await waitFor(() => expect(document.body.textContent ?? "").toContain("暂无技能"));
    expect(document.body.textContent ?? "").not.toContain("暂无会话");
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

  it("展开同名影子来源，只有 selected 来源可删除；缺失版本时删除不可用", async () => {
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd !== "inspect_skills") return baseInvoke(cmd, args);
      const result = inspection();
      delete result.entries[1].candidates[0].meta.resource;
      return result;
    });
    await openSkillsTab();
    expect(screen.queryByText("builtin:skill:demo")).toBeNull();
    fireEvent.click(screen.getByText("被覆盖的来源（1）"));
    await waitFor(() => expect(screen.getByText("builtin:skill:demo")).toBeTruthy());
    expect(screen.getByText("版本：builtin-v1")).toBeTruthy();
    expect(document.querySelector('button[aria-label="删除技能"]')?.hasAttribute("disabled")).toBe(true);
  });

  it("禁用技能重开仍显示，开关只改草稿；取消后保留已保存禁用状态", async () => {
    await openSkillsTab(makeConfig({ disabled_skills: ["demo"] }));
    const toggle = screen.getByRole("switch", { name: "启用技能 demo" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText("当前运行：禁用")).toBeTruthy();
    const reads = calls.filter((call) => call.startsWith("inspect_skills:")).length;
    fireEvent.click(toggle);
    expect(screen.getByText("待保存：启用")).toBeTruthy();
    expect(calls.some((call) => call.startsWith("toggle_skill") || call.startsWith("save_config"))).toBe(false);
    expect(calls.filter((call) => call.startsWith("inspect_skills:")).length).toBe(reads);
    fireEvent.click(buttonByText("取消"));
    await waitFor(() => expect(toggle.getAttribute("aria-checked")).toBe("false"));
    expect(screen.queryByText("待保存：启用")).toBeNull();
    cleanup();
    await openSkillsTab(makeConfig({ disabled_skills: ["demo"] }));
    expect(screen.getByRole("switch", { name: "启用技能 demo" }).getAttribute("aria-checked")).toBe("false");
  });

  it("保存启停草稿通过现有配置提交；缺文件的禁用名称仍可重新启用", async () => {
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd !== "inspect_skills") return baseInvoke(cmd, args);
      const result = inspection();
      result.entries.push({ name: "missing", disabled: true, candidates: [] });
      return result;
    });
    await openSkillsTab(makeConfig({ disabled_skills: ["missing"] }));
    expect(screen.getByText("来源文件不存在，保留禁用配置供检查或重新启用。")).toBeTruthy();
    fireEvent.click(screen.getByRole("switch", { name: "启用技能 missing" }));
    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(calls.some((call) => call.startsWith("save_config:") && call.includes('"disabled_skills":[]'))).toBe(true));
    await waitFor(() => expect(screen.queryByText("待保存：启用")).toBeNull());
  });

  it("读取失败明确显示且可重试；部分目录诊断不掩盖可用候选", async () => {
    const invoke = await invokeMock();
    let failed = true;
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd !== "inspect_skills") return baseInvoke(cmd, args);
      if (failed) throw new Error("目录读取被拒绝");
      return { ...inspection(), issues: [{ path: "/private/skills", message: "无读取权限" }] };
    });
    useSettings.setState({ config: makeConfig(), snapshot: configSnapshot(makeConfig()), loaded: true });
    useUi.setState({ settingsOpen: true, settingsTab: "skills" });
    render(<AntApp><SettingsPage /></AntApp>);
    await waitFor(() => expect(screen.getByText("目录读取被拒绝")).toBeTruthy());
    expect(screen.queryByText("暂无技能")).toBeNull();
    failed = false;
    fireEvent.click(buttonByText("重新加载"));
    await waitFor(() => expect(screen.getByText("demo")).toBeTruthy());
    expect(screen.getByText("/private/skills")).toBeTruthy();
    expect(screen.getByText("无读取权限")).toBeTruthy();
  });

  it("切换会话后迟到的读取和删除结果不能覆盖新项目", async () => {
    const invoke = await invokeMock();
    let settleOld: ((value: SkillInspection) => void) | undefined;
    let settleDelete: (() => void) | undefined;
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "inspect_skills") {
        if (args.sessionId === "old") return new Promise<SkillInspection>((resolve) => { settleOld = resolve; });
        const result = inspection();
        result.entries[1].name = "new-project";
        return result;
      }
      if (cmd === "delete_skill") return new Promise<void>((resolve) => { settleDelete = resolve; });
      return baseInvoke(cmd, args);
    });
    const props = { disabled: [], savedDisabled: [], onToggle: vi.fn() };
    const view = render(<AntApp><SkillsPanel {...props} sessionId="old" /></AntApp>);
    await waitFor(() => expect(settleOld).toBeTypeOf("function"));
    view.rerender(<AntApp><SkillsPanel {...props} sessionId="new" /></AntApp>);
    await waitFor(() => expect(screen.getByText("new-project")).toBeTruthy());
    await act(async () => { settleOld!(inspection()); });
    await waitFor(() => expect(screen.queryByText("demo")).toBeNull());
    fireEvent.click(document.querySelector('button[aria-label="删除技能"]') as HTMLElement);
    const ok = await waitFor(() => buttonByText("OK"));
    fireEvent.click(ok);
    await waitFor(() => expect(settleDelete).toBeTypeOf("function"));
    view.rerender(<AntApp><SkillsPanel {...props} sessionId="third" /></AntApp>);
    await waitFor(() => expect(document.querySelector('[aria-busy="false"]')).toBeTruthy());
    await act(async () => { settleDelete!(); });
    await waitFor(() => expect(screen.queryByText("已删除技能：demo")).toBeNull());
    expect(screen.getByText("new-project")).toBeTruthy();
  });

  it("运行状态来自实际审计，保存基线变化才重新读取，草稿变化不读取", async () => {
    const invoke = await invokeMock();
    let actualDisabled = true;
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd !== "inspect_skills") return baseInvoke(cmd, args);
      const result = inspection();
      result.entries = result.entries.filter((entry) => entry.name === "demo");
      result.entries[0].disabled = actualDisabled;
      return result;
    });
    const onToggle = vi.fn();
    const view = render(<AntApp><SkillsPanel sessionId="project" disabled={[]} savedDisabled={[]} onToggle={onToggle} /></AntApp>);
    await waitFor(() => expect(screen.getByText("当前运行：禁用")).toBeTruthy());
    const countReads = () => invoke.mock.calls.filter(([cmd]: [string]) => cmd === "inspect_skills").length;
    const reads = countReads();
    view.rerender(<AntApp><SkillsPanel sessionId="project" disabled={["demo"]} savedDisabled={[]} onToggle={onToggle} /></AntApp>);
    expect(screen.getByText("待保存：禁用")).toBeTruthy();
    expect(countReads()).toBe(reads);
    actualDisabled = false;
    view.rerender(<AntApp><SkillsPanel sessionId="project" disabled={["demo"]} savedDisabled={["demo"]} onToggle={onToggle} /></AntApp>);
    await waitFor(() => expect(countReads()).toBe(reads + 1));
    await waitFor(() => expect(screen.getByText("当前运行：启用")).toBeTruthy());
    expect(screen.queryByText("待保存：禁用")).toBeNull();
  });

  it("切换会话关闭旧删除确认，迟到确认不能发送删除 IPC", async () => {
    const props = { disabled: [], savedDisabled: [], onToggle: vi.fn() };
    const view = render(<AntApp><SkillsPanel {...props} sessionId="first" /></AntApp>);
    await waitFor(() => expect(screen.getByText("demo")).toBeTruthy());
    fireEvent.click(document.querySelector('button[aria-label="删除技能"]') as HTMLElement);
    const oldConfirm = await waitFor(() => buttonByText("OK"));
    view.rerender(<AntApp><SkillsPanel {...props} sessionId="second" /></AntApp>);
    await waitFor(() => expect(document.querySelector('[aria-busy="false"]')).toBeTruthy());
    await act(async () => { fireEvent.click(oldConfirm); });
    expect(calls.some((call) => call.startsWith("delete_skill:"))).toBe(false);
    expect(screen.getByText("demo-dir")).toBeTruthy();
  });
});
