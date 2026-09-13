// Component smoke tests: mount the full App (whole antd tree) and assert the UI really renders.
// Goal: catch "build passes but renders blank" regressions (e.g. a wrong Tabs items config).
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";

// ---------- Tauri IPC mock ----------
const fixtureConfig = {
  schema_version: 2,
  providers: [
    {
      id: "p1", name: "Test Provider", api_format: "openai_chat",
      base_url: "https://api.example.com/v1", keys: ["***abcd"],
      models: [
        {
          id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
          reasoning_effort: null, vision: true, video: false,
        },
      ],
    },
  ],
  active_model_id: "m1",
  proxy: null,
  network: { allow_private_network: false },
  compact_threshold: 0.6,
  approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false },
  validation: { python: true, rust: true, typescript: true, go: true, json: true },
  ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
};

const fixtureSession = {
  id: "s1", title: "冒烟会话", workspace: "/tmp/ws", model_id: "m1",
  created_at: "2026-08-30T00:00:00Z", updated_at: "2026-08-30T00:00:00Z", message_count: 2,
};

const fixtureMessages = [
  { role: "user", content: [{ type: "text", text: "你好" }] },
  { role: "assistant", content: [{ type: "text", text: "你好！有什么可以帮你？" }] },
];

// resolve_ask captor (read by the ask-submit regression case; avoids per-case overrides polluting the global mock)
const resolveAskLog: any[] = [];

// Controls the activate_and_show return value (toggles the titlebar fallback case; null → frontend falls back to custom)
const titlebarState: { result: "custom" | "native" | null } = { result: null };

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
    switch (cmd) {
      case "ping": return "pong";
      case "activate_and_show": return titlebarState.result;
      case "get_config": return fixtureConfig;
      case "list_sessions": return [fixtureSession];
      case "list_projects": return [];
      case "save_project": return null;
      case "delete_project": return { deleted_sessions: 0 };
      case "select_workspace_dir": return "/tmp/ws";
      case "load_session": return fixtureMessages;
      case "git_status": return { repo: false, entries: [] };
      case "git_user_info": return { name: "测试用户", email: "tester@example.com" };
      case "get_token_breakdown":
        return { system_tokens: 1, history_tokens: 2, tool_results_tokens: 0, tool_schema_tokens: 3,
                 total_tokens: 6, context_window: 128000, ratio: 0.01 };
      case "get_mcp_config": return "{}";
      case "mcp_status": return [];
      case "list_skills": return [{ name: "demo", description: "d", whenToUse: "w", origin: "x" }];
      case "list_agents": return [{ name: "backend-dev", description: "后端实现" }];
      case "search_workspace_paths": return [];
      case "list_workspace_dir": return { entries: [] };
      case "get_token_stats": return [];
      case "create_session": return { session_id: "new-1", workspace: "/tmp/ws" };
      case "resolve_ask": resolveAskLog.push(args?.value); return null;
      case "set_session_prefs": return null;
      case "get_session_prefs": return { approval_mode: "auto_edit", model_id: null, reasoning_effort: null };
      default: return null;
    }
  }),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: vi.fn(), close: vi.fn(), setFocus: vi.fn(), show: vi.fn(), unminimize: vi.fn(),
    isMaximized: vi.fn(async () => false), maximize: vi.fn(), unmaximize: vi.fn(),
    onResized: vi.fn(() => Promise.resolve(vi.fn())),
    listen: vi.fn(() => Promise.resolve(vi.fn())),
  }),
}));

vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => false),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

import App from "../App";
import { useUi } from "../stores/ui";
import { useSessions } from "../stores/sessions";
import { applyFrameToTab } from "../stores/runFrames";
import { useRun } from "../stores/run";

async function mountApp() {
  const utils = render(<App />);
  // Left nav always shows the "Projects" heading (the four topbar buttons moved out: settings/stats/tasks entries live in the bottom-left footer, batches after [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md))
  await waitFor(
    () => expect(document.body.textContent ?? "").toContain("项目"),
    { timeout: 5000 },
  );
  await new Promise((r) => setTimeout(r, 50));
  return utils;
}

async function clickButton(text: string) {
  // antd inserts a space inside two-character button labels; strip whitespace before matching
  const btn = screen
    .getAllByRole("button")
    .find((b) => (b.textContent ?? "").replace(/\s/g, "").includes(text));
  if (!btn) throw new Error(`找不到按钮：${text}`);
  fireEvent.click(btn);
  // Modal mount + antd internal state need time to settle
  await new Promise((r) => setTimeout(r, 80));
}

function clickTab(label: string) {
  const tabs = Array.from(document.querySelectorAll(".ant-tabs-tab"));
  const tab = tabs.find((x) => x.textContent?.trim() === label);
  if (!tab) throw new Error(`找不到页签：${label}`);
  fireEvent.click(tab);
  return new Promise((r) => setTimeout(r, 80));
}

/** Icon buttons (located by aria-label): textless buttons such as the three bottom-left footer entries */
async function clickIconBtn(ariaLabel: string) {
  const btn = document.querySelector(`button[aria-label="${ariaLabel}"]`) as HTMLButtonElement | null;
  if (!btn) throw new Error(`找不到按钮：${ariaLabel}`);
  fireEvent.click(btn);
  // Modal mount + antd internal state need time to settle
  await new Promise((r) => setTimeout(r, 80));
}

afterEach(() => {
  cleanup();
  // zustand stores are module-level singletons: reset panel toggles so modals do not leak across cases
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rightBarOpen: true, rbTab: "info" });
  // Reset sidebar toggles + localStorage memory ([docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md))
  useSessions.setState({ explorerOpen: true });
  localStorage.removeItem("ws_explorer_open");
  localStorage.removeItem("ws_right_bar_open");
  // Clear run state (incl. ask) between cases: since [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md) an active ask covers the Composer; leftovers would hide the input from later cases
  useRun.setState((s) => { s.tabs = {}; });
});

describe("App 渲染冒烟", () => {
  it("主窗口：左下角状态区 + 右栏四页签 + 左侧导航分区渲染", async () => {
    await mountApp();
    const text = document.body.textContent ?? "";
    // The four topbar buttons moved out (changes → RightBar tab; tasks/stats/settings → bottom-left footer); the topbar must no longer contain "Settings"
    expect(document.querySelector(".toolbar")?.textContent ?? "").not.toContain("设置");
    expect(document.querySelector(".toolbar")?.textContent ?? "").not.toContain("统计");
    // Topbar tab strip removed ([docs/session-nav-new-top](../../../docs/session-nav-new-top.md)): session switching/closing consolidated into the left nav + shortcuts
    expect(document.querySelector(".tab-strip")).toBeFalsy();
    // Left nav sections (the "Files" button was removed, [docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md)/30)
    for (const label of ["项目", "任务"]) {
      expect(text).toContain(label);
    }
    // Fixed bottom-left status area: git identity bar (avatar+name/email) + tasks/stats/settings icon entries
    expect(document.querySelector(".sider-footer .git-id")).toBeTruthy();
    for (const label of ["任务", "统计", "设置"]) {
      expect(document.querySelector(`.sider-footer button[aria-label="${label}"]`)).toBeTruthy();
    }
    // RightBar four tabs (reordered in [docs/rightbar-visual-batch](../../../docs/rightbar-visual-batch.md)): Info first and default; Changes (migrated from the old topbar entry) moved last
    const rbTabs = Array.from(document.querySelectorAll(".rb-tabs .ant-tabs-tab")).map((x) => x.textContent?.trim());
    expect(rbTabs[0]).toBe("信息");
    for (const label of ["日志", "文件", "变更"]) {
      expect(rbTabs).toContain(label);
    }
    // Custom titlebar ([docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)): the drag layer covers the topbar with a drag-region marker; once active, html is marked custom
    const dragZone = document.querySelector(".titlebar-drag-zone") as HTMLElement;
    expect(dragZone).toBeTruthy();
    expect(dragZone.hasAttribute("data-tauri-drag-region")).toBe(true);
    await waitFor(() => expect(document.documentElement.dataset.titlebarMode).toBe("custom"));
    // Topbar no longer shows the model name (consolidated into the Composer toolbar model menu tooltip, [docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md))
    expect(text).not.toContain("test-model");
    // Empty state with no session: guide block renders, Composer hidden（引导按钮文案=临时会话，i18n app.emptyNewChat）
    expect(document.querySelector(".chat-empty-guide")).toBeTruthy();
    expect(document.querySelector(".composer")).toBeFalsy();
    const guideBtns = Array.from(document.querySelectorAll(".chat-empty-guide .guide-actions button")).map((b) => b.textContent ?? "");
    expect(guideBtns.some((t) => t.includes("临时会话"))).toBe(true);
  });

  it("屏蔽 WebView 默认右键菜单（contextmenu preventDefault）", async () => {
    await mountApp();
    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    document.body.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
  });

  it("空态引导：新建临时会话进入会话态，Composer 恢复", async () => {
    await mountApp();
    expect(document.querySelector(".composer")).toBeFalsy();
    const btn = Array.from(document.querySelectorAll(".chat-empty-guide button")).find((b) =>
      (b.textContent ?? "").includes("临时会话"),
    ) as HTMLElement;
    fireEvent.click(btn);
    // openFreeSession → create_session mock returns new-1 → once the Tab exists the guide disappears + Composer returns
    await waitFor(() => expect(document.querySelector(".composer")).toBeTruthy());
    expect(document.querySelector(".chat-empty-guide")).toBeFalsy();
  });

  it("设置弹窗：6 个分区页签 + 表单字段渲染", async () => {
    await mountApp();
    await clickIconBtn("设置");

    // All tabs render (regression: wrong Tabs items config → blank content)
    const tabTexts = Array.from(document.querySelectorAll(".ant-tabs-tab")).map((x) => x.textContent).join("|");
    for (const tab of ["通用", "外观", "供应商", "安全", "MCP", "技能"]) {
      expect(tabTexts).toContain(tab);
    }
    // All forms vertical ([docs/settings-forms-vertical](../../../docs/settings-forms-vertical.md)): no horizontal form inside the settings modal
    expect(document.querySelector(".ant-modal .ant-form-horizontal")).toBeFalsy();
    expect(document.querySelector(".ant-modal .ant-form-vertical")).toBeTruthy();
    // General tab form renders: language select exists
    expect(document.querySelectorAll(".ant-select").length).toBeGreaterThan(0);
    // Custom prompt textarea exists
    expect(document.querySelectorAll("textarea").length).toBeGreaterThan(0);

    // Switch to the Appearance tab: dual font slots + preview render ([docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md))
    await clickTab("外观");
    const appearanceText = document.body.textContent ?? "";
    expect(appearanceText).toContain("界面字体");
    expect(appearanceText).toContain("等宽字体");
    expect(document.querySelectorAll(".font-preview-row").length).toBe(2);

    // Switch to the Providers tab ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md)): list + edit form fields
    await clickTab("供应商");
    const providerTexts = document.body.textContent ?? "";
    expect(providerTexts).toContain("Test Provider"); // provider list row
    expect(providerTexts).toContain("test-model");    // model wire id
    expect(providerTexts).toContain("1 个模型");
    // Catalog fill removed ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md): user input is the single source of truth)
    expect(providerTexts).not.toContain("从目录填充");
    // Enter edit: form fields render
    await clickButton("编辑供应商");
    const editTexts = document.body.textContent ?? "";
    expect(editTexts).toContain("Base URL"); // form label
    expect(editTexts).toContain("API Key");
    expect(editTexts).toContain("模型列表");
    const inputs = Array.from(document.querySelectorAll("input")) as HTMLInputElement[];
    expect(inputs.some((i) => i.value === "https://api.example.com/v1")).toBe(true);

    // Switch to the Security tab: switches exist + [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md) auto-confirm approval option renders
    await clickTab("安全");
    expect(document.querySelectorAll(".ant-switch").length).toBeGreaterThanOrEqual(9);
    expect((document.body.textContent ?? "")).toContain("5 分钟后自动确认推荐选项");
  });

  it("设置保存（docs/provider-form-validation）：成功不关闭弹框；无效供应商阻止保存并跳转供应商页签", async () => {
    await mountApp();
    await clickIconBtn("设置");
    // Valid config: save succeeds → toast only, modal stays open (the user decides when to close)
    await clickButton("保存");
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已保存"));
    expect(useUi.getState().settingsOpen).toBe(true);
    expect(document.querySelector(".ant-modal")).toBeTruthy();
    // Edit provider: clear Base URL → live required error → save blocked + error + stays on the Providers tab
    await clickTab("供应商");
    await clickButton("编辑供应商");
    const urlInput = Array.from(document.querySelectorAll("input")).find(
      (i) => (i as HTMLInputElement).value === "https://api.example.com/v1",
    ) as HTMLInputElement;
    expect(urlInput).toBeTruthy();
    fireEvent.change(urlInput, { target: { value: "" } });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.body.textContent ?? "").toContain("必填");
    await clickButton("保存");
    await waitFor(() => expect(document.body.textContent ?? "").toContain("供应商配置无效"));
    expect(useUi.getState().settingsOpen).toBe(true);
    // Scope to the modal: the first document-level .ant-tabs-tab-active may be the RightBar/topbar tab strip
    expect(document.querySelector(".ant-modal .ant-tabs-tab-active")?.textContent ?? "").toContain("供应商");
  });

  it("自绘标题栏：后端返回 native 时 html 标记 native（回退布局分支，docs/custom-font-and-titlebar）", async () => {
    titlebarState.result = "native";
    try {
      await mountApp();
      await waitFor(() => expect(document.documentElement.dataset.titlebarMode).toBe("native"));
    } finally {
      titlebarState.result = null;
    }
  });

  it("项目导航：fixture 会话按项目分组渲染", async () => {
    await mountApp();
    // Left nav shows it directly (no click needed): project group + session rows
    await waitFor(() => expect(screen.getByText("冒烟会话")).toBeTruthy());
    // fixture session has no project_id → grouped under temp sessions
    expect(document.body.textContent).toContain("临时会话");
    expect(document.body.textContent).toContain("还没有任务");
  });

  it("侧栏折叠（docs/sidebar-toggle-buttons + docs/titlebar-logo-toggle 开合入口收口到标题栏 Logo；docs/sidebar-collapse-animation-and-titlebar-blend 左栏折叠 0 宽完全隐藏 + 右栏裁切折叠动画）：仅顶栏按钮开合，localStorage 记忆", async () => {
    await mountApp();
    // Both sidebars present by default
    expect(document.querySelector(".project-nav")).toBeTruthy();
    expect(document.querySelector(".right-bar")).toBeTruthy();
    // Right sidebar toggle lives in the topbar button (tab-row button removed, [docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md) follow-up)
    expect(document.querySelector('.tb-main-seg button[aria-label="折叠右侧栏"]')).toBeTruthy();
    // Left sidebar toggle lives in the titlebar logo ([docs/titlebar-logo-toggle](../../../docs/titlebar-logo-toggle.md); right-segment head since [docs/titlebar-logo-right-segment](../../../docs/titlebar-logo-right-segment.md)): arrows swap on hover, click toggles
    fireEvent.click(document.querySelector('.tb-main-seg button[aria-label="折叠左侧栏"]')!);
    await waitFor(() => {
      // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md): collapsed width 0 = fully hidden; the nav stays mounted (clipped by the Sider shell) and the rail is retired
      expect(document.querySelector(".project-nav")).toBeTruthy();
      expect(document.querySelector(".sider-rail")).toBeFalsy();
      expect(document.querySelector(".tb-left-closed")).toBeTruthy();
      expect(document.querySelector(".tb-main-cleared")).toBeTruthy();
    });
    expect(localStorage.getItem("ws_explorer_open")).toBe("0");
    // Regression ([docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md) fix): collapsed state must keep the Sider shell — has-sider only counts real Sider children;
    // swapping in a plain div flips the inner Layout to vertical and squeezes Content to 0 height (right sidebar "fully collapsed")
    expect(document.querySelector(".ant-layout-has-sider")).toBeTruthy();
    // [docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md): collapsed width is 0 (narrow rail retired) — the Sider fully disappears with a 0.2s width ease.
    // Review 🟡2: match the actual width declaration, not a loose substring ("280px" also contains "0px")
    const siderStyle = (document.querySelector(".ant-layout-sider") as HTMLElement).getAttribute("style") ?? "";
    expect(siderStyle).toMatch(/width:\s*0px/);
    expect(siderStyle).not.toContain("48px");
    // Click the titlebar logo to expand → restored
    fireEvent.click(document.querySelector('.tb-main-seg button[aria-label="展开左侧栏"]')!);
    await waitFor(() => {
      expect(document.querySelector(".tb-left-closed")).toBeFalsy();
      expect(document.querySelector(".tb-main-cleared")).toBeFalsy();
    });
    expect(localStorage.getItem("ws_explorer_open")).toBe("1");
    // Right sidebar collapse: stays mounted ([docs/sidebar-collapse-animation-and-titlebar-blend](../../../docs/sidebar-collapse-animation-and-titlebar-blend.md)), shell class flips to the clip-collapsed state
    fireEvent.click(document.querySelector('button[aria-label="折叠右侧栏"]')!);
    await waitFor(() => {
      expect(document.querySelector(".right-bar-closed")).toBeTruthy();
      expect(document.querySelector(".rb-tabs")).toBeTruthy();
      expect(document.querySelector(".right-rail")).toBeFalsy();
    });
    expect(localStorage.getItem("ws_right_bar_open")).toBe("0");
    // Toggle icon swaps with state (缺陷修复：图标此前固定 PicRightOutlined 不随状态变化)——折叠态 MenuUnfold（展开方向），展开态 MenuFold（收起方向）
    const collapsedIcon = document.querySelector('button[aria-label="展开右侧栏"] svg');
    expect(collapsedIcon?.getAttribute("data-icon")).toBe("menu-unfold");
    // Topbar toggle button (the only open/close entry, [docs/titlebar-content-batch](../../../docs/titlebar-content-batch.md)) → restored
    fireEvent.click(document.querySelector('button[aria-label="展开右侧栏"]')!);
    await waitFor(() => expect(document.querySelector(".right-bar-closed")).toBeFalsy());
    expect(localStorage.getItem("ws_right_bar_open")).toBe("1");
    const openIcon = document.querySelector('button[aria-label="折叠右侧栏"] svg');
    expect(openIcon?.getAttribute("data-icon")).toBe("menu-fold");
  });

  it("新建项目弹框：名称 + 单目录 + 保存", async () => {
    const { container } = await mountApp();
    const addBtn = container.querySelector('button[title="新建项目"]') as HTMLButtonElement;
    expect(addBtn).toBeTruthy();
    fireEvent.click(addBtn);
    await waitFor(() => expect(screen.getByText("项目名称")).toBeTruthy());
    const nameInput = screen.getByPlaceholderText("例如：CodeWave") as HTMLInputElement;
    fireEvent.change(nameInput, { target: { value: "我的项目" } });
    // Single-directory semantics: the select_workspace_dir mock returns /tmp/ws directly; after clicking "Choose Directory" a Tag appears
    const pickDir = Array.from(document.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("选择目录"),
    )!;
    fireEvent.click(pickDir);
    await waitFor(() => expect(screen.getByText("/tmp/ws")).toBeTruthy());
    const save = Array.from(document.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === "保存",
    ) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(screen.getByText("我的项目")).toBeTruthy(), { timeout: 3000 });
  });

  it("删除项目：影响说明确认后级联删除", async () => {
    await mountApp();
    useSessions.setState({
      projects: [{ id: "p1", name: "待删项目", directory: "/tmp/ws", created_at: "2026-08-30T00:00:00Z" }],
    });
    const manageBtn = document.querySelector('button[title="管理项目"]') as HTMLButtonElement;
    fireEvent.click(manageBtn);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("待删项目"));
    fireEvent.click(document.querySelector('button[title="删除项目"]') as HTMLButtonElement);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("连带删除该项目下"));
    expect(document.body.textContent).toContain("0 个会话");
    const okBtn = Array.from(document.querySelectorAll(".ant-modal-confirm-btns button")).find(
        (b) => (b.textContent ?? "").replace(/\s/g, "") === "删除",
      ) as HTMLButtonElement;
    fireEvent.click(okBtn);
    // happy-dom does not run close animations: assert the nav no longer renders that project
    await waitFor(
      () => expect(document.querySelector(".project-nav")?.textContent ?? "").not.toContain("待删项目"),
      { timeout: 3000 },
    );
  });

  it("统计弹窗：空数据占位渲染", async () => {
    await mountApp();
    await clickIconBtn("统计");
    await waitFor(() => expect(screen.getByText("Token 用量（近 30 天）")).toBeTruthy());
  });

  it("设置：MCP 结构化编辑器渲染", async () => {
    await mountApp();
    await clickIconBtn("设置");
    await clickTab("MCP");
    // Single-entry editing: add server → entry fields expand
    await clickButton("添加服务器");
    await waitFor(() => expect(screen.getByText("命令")).toBeTruthy());
    expect(document.body.textContent).toContain("保存并重连");
    expect(document.querySelectorAll(".mcp-entry").length).toBe(1);
    // Skills tab still works（左栏技能分段也会渲染同名技能，此处范围限定到设置弹框）
    await clickTab("技能");
    await waitFor(() => {
      const inModal = [...document.querySelectorAll(".ant-modal")].some((m) =>
        [...m.querySelectorAll(".skill-row")].some((r) => r.textContent?.includes("demo")),
      );
      expect(inModal).toBe(true);
    });
  });

  it("Composer：/ 触发技能菜单（命令入口已移除），占位符渲染", async () => {
    // Composer is hidden with no session → seed an active session (store is a module-level singleton; avoid clicking the guide to prevent leftovers)
    useSessions.setState({
      tabs: [{
        key: "s-seed", sessionId: "s-seed", workspace: "/tmp/ws", title: "种子会话",
        projectId: null, createdAt: "2026-08-30T00:00:00Z",
        prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
      }],
      activeKey: "s-seed",
    });
    await mountApp();
    // Note: rc-textarea renders hidden mirror nodes; locate the visible input by placeholder
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    expect(textarea).toBeTruthy();
    fireEvent.change(textarea, { target: { value: "/" } });
    // 菜单只列技能（list_skills mock）：/demo 在列，且旧命令项不再出现
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/demo"));
    const text = document.body.textContent ?? "";
    expect(text).not.toContain("压缩上下文");
    expect(text).not.toContain("Git 状态");
  });
});

describe("Composer 工具条（docs/composer-toolbar-batch-report）", () => {
  const DEFAULT_PREFS: import("../ipc/types").SessionPrefs = {
    approval_mode: "auto_edit", model_id: null, reasoning_effort: null,
  };

  function seedTab() {
    useSessions.setState({
      tabs: [{
        key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
        projectId: null, createdAt: "2026-08-30T00:00:00Z", prefs: { ...DEFAULT_PREFS },
      }],
      activeKey: "s1",
    });
  }

  async function openToolbarMenu(btnText: string) {
    const btn = Array.from(document.querySelectorAll(".composer-toolbar button")).find(
      (b) => (b.textContent ?? "").includes(btnText),
    );
    if (!btn) throw new Error(`工具条找不到按钮：${btnText}`);
    fireEvent.click(btn);
    await new Promise((r) => setTimeout(r, 120));
    return document.querySelector(".ant-dropdown:not(.ant-dropdown-hidden) .ant-dropdown-menu");
  }

  it("工具条渲染：压缩/上下文/模型/力度/圆形发送钮，压缩与上下文在模型之前", async () => {
    seedTab();
    await mountApp();
    const toolbar = document.querySelector(".composer-toolbar");
    expect(toolbar).toBeTruthy();
    const text = toolbar?.textContent ?? "";
    expect(text).toContain("自动编辑"); // current permission mode
    expect(text).toContain("上下文 "); // context label always visible (restored per user request)
    expect(text).toContain("test-model"); // model wire id always visible (docs/provider-management-refactor: wire id is the display name)
    expect(text).toContain("默认"); // default effort tier
    // Compact icon button exists with a semantic label (formerly a standalone button, now iconified)
    const compactBtn = toolbar?.querySelector('button[aria-label="压缩上下文"]');
    expect(compactBtn).toBeTruthy();
    // Order: the compact icon and the context label both precede the model icon button
    const order = [
      compactBtn!,
      toolbar?.querySelector(".ctx-label")!,
      toolbar?.querySelector('button[aria-label="模型"]')!,
    ];
    for (const el of order) expect(el).toBeTruthy();
    const pos = toolbar!.innerHTML.indexOf.bind(toolbar!.innerHTML);
    expect(pos((compactBtn as HTMLElement).outerHTML)).toBeLessThan(pos((order[2] as HTMLElement).outerHTML));
    expect(pos((order[1] as HTMLElement).outerHTML)).toBeLessThan(pos((order[2] as HTMLElement).outerHTML));
    const send = toolbar?.querySelector(".send-btn") as HTMLButtonElement;
    expect(send).toBeTruthy();
    expect(send.disabled).toBe(true); // disabled on empty text
    expect(document.querySelector(".composer-card")).toBeTruthy();
    // Multiple border beams ([docs/antd6-upgrade-and-composer-border-beam](../../../docs/antd6-upgrade-and-composer-border-beam.md)): 3 BorderBeams spread evenly along the input card border
    expect(document.querySelectorAll(".composer-card .ant-border-beam").length).toBe(3);
    // Beam timing narrowed ([docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)): idle/unfocused beams carry composer-beam-idle; shown on focus, restored on blur.
    // Hidden visually (opacity+pointer-events), NOT display:none: a display swap on blur (mousedown) cancels the pending
    // click in WKWebView and swallowed the first click on sidebar rows (assertions count the class, agnostic to hiding).
    const idleBeams = () => document.querySelectorAll(".composer-card .ant-border-beam.composer-beam-idle").length;
    expect(idleBeams()).toBe(3);
    const beamTa = document.querySelector(".composer-card textarea") as HTMLTextAreaElement;
    fireEvent.focus(beamTa);
    expect(idleBeams()).toBe(0);
    fireEvent.blur(beamTa);
    expect(idleBeams()).toBe(3);
  });

  it("发送按钮三态：运行中无输入=停止，输入后=提交，清空复归停止", async () => {
    seedTab();
    useRun.setState((s) => {
      s.tabs = {}; // clear leftovers so run state does not leak from other cases
      s.tabs["s1"] = {
        items: [], running: true, streamGen: 0, ask: null, breakdown: null,
        todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
        queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
    await mountApp();
    const btn = () => document.querySelector(".send-btn") as HTMLButtonElement;
    expect(btn().getAttribute("aria-label")).toBe("停止"); // running + empty input
    const ta = document.querySelector(".composer-card textarea") as HTMLTextAreaElement;
    fireEvent.change(ta, { target: { value: "排队消息" } });
    expect(btn().getAttribute("aria-label")).toBe("提交"); // running + has input
    expect(btn().disabled).toBe(false);
    fireEvent.change(ta, { target: { value: "" } });
    expect(btn().getAttribute("aria-label")).toBe("停止"); // back to stop after clearing
    // Clean up run-state leftovers: otherwise later cases get the queuePlaceholder (running branch)
    useRun.setState((s) => {
      delete s.tabs["s1"];
    });
  });

  it("权限菜单：四档渲染，切换触发 set_session_prefs 并更新按钮文案", async () => {
    seedTab();
    await mountApp();
    const menu = await openToolbarMenu("自动编辑");
    expect(menu?.textContent).toContain("变更前确认");
    expect(menu?.textContent).toContain("自动编辑");
    expect(menu?.textContent).toContain("计划模式");
    expect(menu?.textContent).toContain("完全访问");
    // Menu items carry per-mode classes (wiring for coloring the label by mode; plan has no class = no color)
    expect(menu?.querySelector(".menu-item-rich.approval-confirm")).toBeTruthy();
    expect(menu?.querySelector(".menu-item-rich.approval-auto")).toBeTruthy();
    expect(menu?.querySelector(".menu-item-rich.approval-full")).toBeTruthy();
    const planItem = Array.from(menu?.querySelectorAll("li.ant-dropdown-menu-item") ?? []).find((x) =>
      x.textContent?.includes("计划模式"),
    );
    expect(planItem?.querySelector(".menu-item-rich")?.className).not.toContain("approval-");
    expect(planItem?.querySelector(".menu-item-rich")).toBeTruthy();
    const item = Array.from(menu?.querySelectorAll(".ant-dropdown-menu-item") ?? []).find((x) =>
      x.textContent?.includes("完全访问"),
    ) as HTMLElement;
    fireEvent.click(item);
    await waitFor(() => expect((document.querySelector(".composer-toolbar")?.textContent ?? "")).toContain("完全访问"));
    // Switch toast ([docs/session-pref-switch-toast](../../../docs/session-pref-switch-toast.md))
    await waitFor(() => expect(document.body.textContent).toContain("已切换权限模式：完全访问"));
    // Red highlight class (full access = danger tier, [docs/composer-shift-tab-mode-cycle](../../../docs/composer-shift-tab-mode-cycle.md))
    expect(document.querySelector(".composer-toolbar .approval-full")).toBeTruthy();
    // Pill icon switches with the mode (regression: SafetyOutlined was once hardcoded so the icon never changed)
    expect(document.querySelector(".composer-toolbar .approval-full .anticon-safety-certificate")).toBeTruthy();
  });

  it("Shift+Tab：输入框内循环切换权限四档（docs/composer-shift-tab-mode-cycle）", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    const modeText = () => document.querySelector(".composer-toolbar")?.textContent ?? "";
    expect(modeText()).toContain("自动编辑"); // initial mode
    expect(document.querySelector(".composer-toolbar .approval-auto")).toBeTruthy(); // auto edit = orange
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("计划模式")); // auto_edit → plan
    await waitFor(() => expect(document.body.textContent).toContain("已切换权限模式：计划模式")); // switch toast (docs/session-pref-switch-toast)
    expect(document.querySelector(".composer-toolbar .approval-plan")).toBeFalsy(); // plan mode gets no color
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("完全访问")); // plan → full_access
    expect(document.querySelector(".composer-toolbar .approval-full")).toBeTruthy(); // red highlight in sync
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("变更前确认")); // full_access wraps around to confirm_each
    expect(document.querySelector(".composer-toolbar .approval-full")).toBeFalsy();
    expect(document.querySelector(".composer-toolbar .approval-confirm")).toBeTruthy(); // confirm mode = blue
  });

  it("模型菜单：图标入口 + 供应商分组 + 视觉标签 + 管理供应商入口 + 切换镜像 prefs", async () => {
    seedTab();
    await mountApp();
    // Model button is now an icon ([docs/plan-mode-workflow](../../../docs/plan-mode-workflow.md)): locate by aria-label
    const modelBtn = document.querySelector('.composer-toolbar button[aria-label="模型"]') as HTMLButtonElement;
    expect(modelBtn).toBeTruthy();
    expect(modelBtn.title).toContain("test-model"); // tooltip keeps full model info (provider · wire id)
    fireEvent.click(modelBtn);
    await new Promise((r) => setTimeout(r, 120));
    const menu = document.querySelector(".ant-dropdown:not(.ant-dropdown-hidden) .ant-dropdown-menu");
    expect(menu?.textContent).toContain("Test Provider"); // group header = provider name (docs/provider-management-refactor)
    expect(menu?.textContent).toContain("管理供应商");
    expect(menu?.textContent).toContain("视觉"); // vision model badge
    // Switch to the only model → set_session_prefs mirror
    const before = useSessions.getState().tabs.find((x) => x.key === "s1")?.prefs.model_id;
    const item = Array.from(menu?.querySelectorAll(".ant-dropdown-menu-item") ?? []).find((x) =>
      x.textContent?.includes("test-model"),
    ) as HTMLElement;
    fireEvent.click(item);
    await waitFor(() =>
      expect(useSessions.getState().tabs.find((x) => x.key === "s1")?.prefs.model_id).not.toBe(before),
    );
    // Switch toast ([docs/session-pref-switch-toast](../../../docs/session-pref-switch-toast.md))
    await waitFor(() => expect(document.body.textContent).toContain("已切换模型：test-model"));
  });

  it("力度菜单：五档（默认/低/中/高/最高）可选", async () => {
    seedTab();
    await mountApp();
    const menu = await openToolbarMenu("默认");
    for (const label of ["低", "中", "高", "最高"]) {
      expect(menu?.textContent).toContain(label);
    }
    const item = Array.from(menu?.querySelectorAll(".ant-dropdown-menu-item") ?? []).find((x) =>
      x.textContent?.trim() === "高",
    ) as HTMLElement;
    fireEvent.click(item);
    await new Promise((r) => setTimeout(r, 80));
    expect((document.querySelector(".composer-toolbar")?.textContent ?? "")).toContain("高");
  });

  it("ask 提交：plan 档选中「执行方案」→ 同步切自动编辑且回答送达（回归：submit 内调 hook 抛错致按钮失效）", async () => {
    seedTab();
    // Set plan mode to simulate a plan-mode ask
    useSessions.setState({
      tabs: [{
        key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
        projectId: null, createdAt: "2026-08-30T00:00:00Z",
        prefs: { approval_mode: "plan", model_id: null, reasoning_effort: null },
      }],
      activeKey: "s1",
    });
    await mountApp();
    // Inject the ask state directly (no real backend call)
    useRun.setState((s) => {
      s.tabs["s1"] = {
        items: [], running: false, streamGen: 0,
        ask: { askId: "a1", kind: "ask" as const, questions: [{ id: "q1", question: "是否执行？", options: [{ id: "approve", label: "执行方案" }, { id: "revise", label: "补充意见" }] }],
        },
        breakdown: null, todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
    await waitFor(() => expect(document.querySelector(".ask-card")).toBeTruthy());
    // [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md): while ask is active the ask card covers the entire input area and the Composer itself does not render (zcode-style single bottom input)
    expect(document.querySelector(".composer-card")).toBeNull();
    resolveAskLog.length = 0; // clear leftovers from earlier cases
    // Requirements batch: clicking the approve option submits directly (no second submit button); pill sync still happens on approval
    fireEvent.click(screen.getByText("执行方案"));
    await waitFor(() => expect(resolveAskLog.length).toBe(1));
    expect(resolveAskLog[0].answers.q1.selections).toContain("approve");
    await waitFor(() =>
      expect(useSessions.getState().tabs.find((t) => t.key === "s1")?.prefs.approval_mode).toBe("auto_edit"),
    );
    // After the ask clears (the real flow clears via backend events; here we clear explicitly) the Composer is restored as-is
    useRun.setState((s) => { const t = s.tabs["s1"]; if (t) t.ask = null; });
    await waitFor(() => expect(document.querySelector(".composer-card")).toBeTruthy());
  });

  it("/ 技能：点击菜单项直接插入 /name 前缀（详情弹层在右栏）", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "/" } });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/demo"));
    const item = Array.from(document.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("/demo"),
    ) as HTMLElement;
    fireEvent.click(item);
    await waitFor(() => expect(textarea.value).toBe("/demo "));
  });

  it("$ 子代理菜单：输入 $ 列出内置角色，选中插入 $role", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: "$" } });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("$backend-dev"));
    const item = Array.from(document.querySelectorAll("button")).find((b) =>
      (b.textContent ?? "").includes("$backend-dev"),
    ) as HTMLElement;
    fireEvent.click(item);
    await waitFor(() => expect(textarea.value).toBe("$backend-dev "));
  });

  it("Cmd/Ctrl+L：焦点跳到输入框", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    fireEvent.keyDown(window, { key: "l", metaKey: true, cancelable: true });
    await waitFor(() => expect(document.activeElement).toBe(textarea));
  });

  it("思考流式期间向上滚动：跟随立即暂停且不被拽回（docs/thinking-scroll-fix）", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    // Create a running session (no real backend call) and enter streaming through the real send path
    useRun.setState((s) => {
      s.tabs["s1"] = {
        items: [],
        running: false,
        streamGen: 0,
        ask: null,
        breakdown: null,
        todos: [],
        suggestions: [],
        subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
        gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
    fireEvent.change(textarea, { target: { value: "想一个复杂问题" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    await waitFor(() => expect(useRun.getState().tabs["s1"]?.running).toBe(true));
    // Thinking stream: multiple delta_thinking frames (collapsed, thinking block not expanded)
    useRun.setState((s) => {
      const t = s.tabs["s1"]!;
      for (const ch of ["第一段思考。", "第二段思考。", "第三段思考。", "第四段思考。"]) {
        applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: ch } as any);
      }
    });
    await waitFor(() => expect(document.body.textContent ?? "").toContain("思考中"));
    const scroller = document.querySelector(".chat-messages") as HTMLElement;
    expect(scroller).toBeTruthy();
    // Wheel up while collapsed → follow pauses immediately, the "scroll to bottom" button appears
    fireEvent.wheel(scroller, { deltaY: -100 });
    await waitFor(() => expect(screen.getByLabelText("滚动到底部")).toBeTruthy());
    // Content keeps growing (thinking stream unfinished): follow must stay paused and the view must not be dragged back to the bottom (button must not disappear).
    // happy-dom has no layout, so geometric assertions are meaningless (scrollTop is always 0); assert behavior instead of geometry.
    useRun.setState((s) => {
      const t = s.tabs["s1"]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "第五段思考，足够长以推动内容增长并触发跟随 effect。" } as any);
    });
    await new Promise((r) => setTimeout(r, 300)); // pass the 150ms grace window and the async effect chain
    expect(screen.getByLabelText("滚动到底部")).toBeTruthy();
    // Wheel down (not at bottom) must not wrongly resume following; clicking "scroll to bottom" resumes following
    fireEvent.wheel(scroller, { deltaY: 100 });
    expect(screen.getByLabelText("滚动到底部")).toBeTruthy();
    fireEvent.click(screen.getByLabelText("滚动到底部"));
    await waitFor(() => expect(screen.queryByLabelText("滚动到底部")).toBeFalsy());
  });

  it("思考跑马灯（docs/thinking-marquee-rewrite）：全宽 band 展示最新行，换行后上翻到新行", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    useRun.setState((s) => {
      s.tabs["s1"] = {
        items: [],
        running: false,
        streamGen: 0,
        ask: null,
        breakdown: null,
        todos: [],
        suggestions: [],
        subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
        gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
      };
    });
    fireEvent.change(textarea, { target: { value: "想一个复杂问题" } });
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    await waitFor(() => expect(useRun.getState().tabs["s1"]?.running).toBe(true));
    useRun.setState((s) => {
      const t = s.tabs["s1"]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "第一行思考内容。" } as any);
    });
    // Band lives in the thinking header (single row, filling the space right of the title) and shows the latest line
    await waitFor(() => expect(document.querySelector(".thinking-label .thinking-marquee")).toBeTruthy());
    await waitFor(() =>
      expect(document.querySelector(".thinking-marquee .thinking-roll-line.in")?.textContent).toContain("第一行思考内容"),
    );
    // In-place growth of the same line swaps the text without a roll-up ([docs/thinking-marquee-rewrite](../../../docs/thinking-marquee-rewrite.md) revision)
    useRun.setState((s) => {
      const t = s.tabs["s1"]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "继续补充同一段内容。" } as any);
    });
    await new Promise((r) => setTimeout(r, 120));
    expect(document.querySelector(".thinking-roll.rolling")).toBeFalsy();
    expect(document.querySelector(".thinking-marquee .thinking-roll-line.in")?.textContent).toContain("继续补充同一段内容");
    // A newline arrives → the band flips up to the new latest line
    useRun.setState((s) => {
      const t = s.tabs["s1"]!;
      applyFrameToTab(t, { type: "delta_thinking", gen: 0, text: "\n第二行思考内容更长一些。" } as any);
    });
    await waitFor(() =>
      expect(document.querySelector(".thinking-marquee .thinking-roll-line.in")?.textContent).toContain("第二行思考内容"),
    );
    useRun.setState((s) => {
      delete s.tabs["s1"];
    });
  });
});
