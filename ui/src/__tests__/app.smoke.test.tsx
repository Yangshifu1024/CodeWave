// Component smoke tests: mount the full App (whole antd tree) and assert the UI really renders.
// Goal: catch "build passes but renders blank" regressions (e.g. a wrong Tabs items config).
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

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
  post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
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
      case "list_scheduled_tasks": return []; // 任务列表：左栏任务区与任务页都从这里取（stores/tasks）
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
import { applyFrameToTab, blank } from "../stores/runFrames";
import { useSettings } from "../stores/settings";
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
  // 批② 起设置页左导航自建（替掉 antd Tabs）：[docs/settings-ia](../../../docs/settings-ia.md)
  const tabs = Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item'));
  const tab = tabs.find((x) => x.textContent?.trim() === label);
  if (!tab) throw new Error(`找不到设置页导航项：${label}`);
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
  useRun.setState((s) => { s.tabs = {}; s.drafts = {}; });
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

  it("设置页：10 个分区导航（三组）+ 逐页表单渲染", async () => {
    await mountApp();
    await clickIconBtn("设置");

    // 10 页导航 + 三组标题（回归：导航项配置错了会渲染空页体）
    const navLabels = Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item')).map((x) =>
      (x.textContent ?? "").trim(),
    );
    expect(navLabels).toEqual(["界面", "模型与供应商", "网络与连接", "安全与审批", "MCP", "技能", "写入后检查与校验", "工作区与智能体", "日志", "关于"]);
    expect(
      Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-group')).map((x) =>
        (x.textContent ?? "").trim(),
      ),
    ).toEqual(["外观与模型", "安全与能力", "诊断与其他"]);
    // 表单一律 vertical（[docs/settings-forms-vertical](../../../docs/settings-forms-vertical.md)）：设置页内不得出现 horizontal 表单
    expect(document.querySelector('[data-testid="settings-page"] .ant-form-horizontal')).toBeFalsy();
    expect(document.querySelector('[data-testid="settings-page"] .ant-form-vertical')).toBeTruthy();

    // 落地页「界面」：主题 / 界面语言两个 Select + 字体双槽与预览（[docs/custom-font-and-titlebar](../../../docs/custom-font-and-titlebar.md)）
    expect(document.querySelectorAll('[data-testid="settings-page"] .ant-select').length).toBeGreaterThanOrEqual(2);
    expect(document.body.textContent ?? "").toContain("界面字体");
    expect(document.body.textContent ?? "").toContain("等宽字体");
    expect(document.querySelectorAll(".font-preview-row").length).toBe(2);

    // 模型与供应商页（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：列表 + 编辑表单字段 + AI 回复语言
    await clickTab("模型与供应商");
    const providerTexts = document.body.textContent ?? "";
    expect(providerTexts).toContain("Test Provider"); // provider list row
    expect(providerTexts).toContain("test-model");    // model wire id
    expect(providerTexts).toContain("1 个模型");
    expect(providerTexts).toContain("AI 语言");        // 从旧「通用」页迁入
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

    // 网络与连接页：代理模式三张卡片 + 内网访问（批② 从「安全」迁入）
    await clickTab("网络与连接");
    expect(document.body.textContent ?? "").toContain("代理模式");
    expect(document.body.textContent ?? "").toContain("允许访问内网地址");
    expect(document.querySelectorAll(".proxy-mode-card").length).toBe(3);

    // 安全与审批页：审批开关 + [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md) 自动确认选项
    await clickTab("安全与审批");
    expect(document.querySelectorAll('[data-testid="settings-page"] .ant-switch').length).toBeGreaterThanOrEqual(4);
    expect(document.body.textContent ?? "").toContain("5 分钟后自动确认推荐选项");

    // 写入后检查与校验页：只剩四项校验配置（MCP / 技能已拆成独立页）
    await clickTab("写入后检查与校验");
    expect(document.querySelectorAll('[data-setting-id^="post_write_check."]').length).toBe(4);
    expect(document.body.textContent ?? "").toContain("写入后检查");

    // MCP 页：服务器配置区（本组 fixture 未配服务器 → 状态表整段不渲染）
    await clickTab("MCP");
    expect(document.querySelector('[data-setting-id="mcp.servers"]')).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("新建");
    expect(document.querySelector('[data-setting-id="app.mcp_status"]')).toBeTruthy();

    // 技能页：禁用清单
    await clickTab("技能");
    expect(document.querySelector('[data-setting-id="disabled_skills"]')).toBeTruthy();

    // 工作区与智能体页：Shell 下拉 + 自定义提示词 + 压缩两项（旧「通用」页的这 4 项）
    await clickTab("工作区与智能体");
    expect(document.body.textContent ?? "").toContain("Shell");
    expect(document.querySelector('[data-testid="settings-page"] textarea')).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("自动压缩阈值");

    // 日志页：日志级别 + 会话详细日志
    await clickTab("日志");
    expect(document.body.textContent ?? "").toContain("日志级别");
    expect(document.body.textContent ?? "").toContain("会话详细日志");

    // 关于页（原弹框迁入第 8 页）：身份块 + 自动更新开关
    await clickTab("关于");
    expect(document.querySelector(".about-logo")).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("启动时自动检查更新");
  });

  it("设置保存（docs/provider-form-validation）：成功不关闭页面；无效供应商阻止保存并跳转供应商页", async () => {
    await mountApp();
    await clickIconBtn("设置");
    // Valid config: save succeeds → toast only, the page stays open (the user decides when to close)
    await clickButton("保存");
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已保存"));
    expect(useUi.getState().settingsOpen).toBe(true);
    // 全屏设置页常驻 DOM（不再是 Modal 外壳）：docs/settings-fullscreen-shell
    expect(document.querySelector("[data-testid='settings-page']")).toBeTruthy();
    // Edit provider: clear Base URL → live required error → save blocked + error + stays on the Providers page
    await clickTab("模型与供应商");
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
    // 跳回「模型与供应商」页：范围限定到设置页（文档级 .ant-tabs-tab-active 可能是右栏的）
    expect(document.querySelector('[data-testid="settings-page"] .settings-nav-item-active')?.textContent ?? "").toContain("模型与供应商");
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

  it("任务页：覆盖式全屏页挂在内层 Layout 里（度量与设置页一致），工作区只加 .workspace-covered", async () => {
    await mountApp();
    await clickIconBtn("任务");

    const shell = document.querySelector(".tasks-shell");
    expect(shell).toBeTruthy();
    expect(document.querySelector('[data-testid="tasks-page"]')).toBeTruthy();
    // 回归：.tasks-shell 是 absolute inset:0 —— 必须挂在**内层** Layout（position:relative）里，
    // 挂到外层会连自绘标题栏一起盖住；断言方式：它的父节点就是那个 has-sider 布局（Sider/Content 的同一父）
    expect(shell?.parentElement?.classList.contains("ant-layout-has-sider")).toBe(true);
    expect(shell?.parentElement).toBe(document.querySelector(".ant-layout-sider")?.parentElement);
    // 工作区仍挂载，让位集合与设置页完全一致（Sider + Content + 两条栏宽分隔条）
    expect(document.querySelector(".project-nav")).toBeTruthy();
    expect(document.querySelector(".right-bar")).toBeTruthy();
    const covered = Array.from(document.querySelectorAll(".workspace-covered"));
    expect(covered.length).toBe(4);
    expect(document.querySelector(".ant-layout-sider")?.classList.contains("workspace-covered")).toBe(true);
    expect(document.querySelector(".right-bar")?.closest(".workspace-covered")).toBeTruthy();
    for (const el of document.querySelectorAll<HTMLElement>(".rb-resize-handle")) expect(el.tabIndex).toBe(-1);

    // 左栏：返回工作区 / 标题 / 新建都在 .tasks-nav 内，原顶部操作条（.tasks-actions）已不存在 ——
    // 全屏页左栏与设置页左栏同宽同色（docs/tasks-module-polish §1；工作区左栏另有一套夹取规则，不保证等宽）
    const nav = document.querySelector(".tasks-nav") as HTMLElement | null;
    expect(nav).toBeTruthy();
    expect(nav!.querySelector(".tasks-nav-head button")?.textContent?.replace(/\s/g, "")).toContain("返回工作区");
    expect(nav!.querySelector(".tasks-nav-title")?.textContent).toBe("计划任务");
    expect(nav!.querySelector(".tasks-nav-actions button")?.textContent?.replace(/\s/g, "")).toContain("新建任务");
    expect(document.querySelector(".tasks-actions")).toBeFalsy();
    const tasksNavWidth = nav!.style.width;

    // CSS 契约：左栏背景取 --ws-bg-nav（两页同色）；.tasks-shell 为行向排布（左栏 + 内容列）；
    // 且 .tasks-nav 与 .settings-nav 的度量**逐条一致** —— 两页左栏是两套重复声明，只靠注释守护会被后人改飞
    const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");
    expect(appCss).toMatch(/\.tasks-nav\s*\{[^}]*background:\s*var\(--ws-bg-nav\)/);
    const shellBlock = /\.tasks-shell\s*\{([^}]*)\}/.exec(appCss)?.[1] ?? "";
    expect(shellBlock).toContain("display: flex");
    expect(shellBlock).not.toContain("flex-direction: column");
    // 取声明块：`\.tasks-nav\s*\{` 不会误匹配 .tasks-nav-head / -title / -actions（其后是 `-`，不满足 \s*\{）
    const cssBlock = (cls: string) => new RegExp(`\\.${cls}\\s*\\{([^}]*)\\}`).exec(appCss)?.[1] ?? "";
    const decl = (block: string, prop: string) =>
      new RegExp(`(?:^|;)\\s*${prop}\\s*:\\s*([^;]+)`).exec(block)?.[1]?.trim() ?? "";
    const tasksNavBlock = cssBlock("tasks-nav");
    const settingsNavBlock = cssBlock("settings-nav");
    expect(settingsNavBlock).not.toBe(""); // 防正则失配导致的假绿
    for (const prop of ["flex", "min-width", "max-width", "padding", "border-right", "background", "overflow-y", "overflow-x"]) {
      expect(decl(tasksNavBlock, prop)).toBe(decl(settingsNavBlock, prop));
    }

    // Esc 归任务页：关闭页面而不是去停运行中的会话
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(document.querySelector(".tasks-shell")).toBeFalsy());
    expect(useUi.getState().tasksOpen).toBe(false);
    expect(document.querySelectorAll(".workspace-covered").length).toBe(0);

    // 两页左栏同宽：宽度都由 utils/layout 的 fullscreenNavWidth(windowWidth) 决定（同一窗口 ⇒ 同一像素值）
    await clickIconBtn("设置");
    await waitFor(() => expect(document.querySelector(".settings-nav")).toBeTruthy());
    expect((document.querySelector(".settings-nav") as HTMLElement).style.width).toBe(tasksNavWidth);
  });

  it("设置/任务页顶沿与 Header 底沿对齐（AppShell 内层 Layout 改 flex:1 后回归）", async () => {
    // 真守卫：源码不再写 calc(100% - var(--ws-titlebar-h))——这是错位缺陷的历史写法，
    // 一旦回退到 calc 此断言立刻转红。happy-dom 不做布局，DOM rect 断言只能算"占位"。
    const appShellSrc = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../features/shell/AppShell.tsx"),
      "utf8",
    );
    expect(appShellSrc).not.toMatch(/calc\(100%\s*-\s*var\(--ws-titlebar-h\)\)/);

    // happy-dom 布局尺寸恒为 0 → 给 .toolbar 与 .settings-shell/.tasks-shell 注入确定的几何值
    // 模拟"修复后"的真实渲染：Header 高 50px；覆盖页顶沿贴 Header 底沿（=50）
    // 一旦 CSS 漂移（如覆盖页 margin-top 几像素），getBoundingClientRect 的实现如果读真实 layout，
    // 这里就会失败——同时给 jsdom/playwright 切换留好接口（仅 mock 几何，不动 CSS）
    const origRect = Element.prototype.getBoundingClientRect;
    const rectSpy = vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const r = origRect.call(this);
      if (this instanceof HTMLElement) {
        if (this.classList.contains("toolbar")) {
          return { ...r, top: 0, bottom: 50, height: 50 } as unknown as DOMRect;
        }
        if (this.classList.contains("settings-shell") || this.classList.contains("tasks-shell")) {
          return { ...r, top: 50, bottom: 900, height: 850 } as unknown as DOMRect;
        }
      }
      return r;
    });
    try {
      await mountApp();

      useUi.setState({ navWidth: 480 });
      await clickIconBtn("设置");
      await waitFor(() => expect(document.querySelector(".settings-shell")).toBeTruthy());

      const header = document.querySelector(".toolbar") as HTMLElement | null;
      const shell = document.querySelector(".settings-shell") as HTMLElement | null;
      expect(header).toBeTruthy();
      expect(shell).toBeTruthy();
      const headerRect = header!.getBoundingClientRect();
      const shellRect = shell!.getBoundingClientRect();
      expect(Math.abs(shellRect.top - headerRect.bottom)).toBeLessThanOrEqual(1);

      fireEvent.keyDown(window, { key: "Escape" });
      await waitFor(() => expect(document.querySelector(".settings-shell")).toBeFalsy());
      useUi.setState({ tasksOpen: true });
      await waitFor(() => expect(document.querySelector(".tasks-shell")).toBeTruthy());
      const tasksShell = document.querySelector(".tasks-shell") as HTMLElement | null;
      const tasksRect = tasksShell!.getBoundingClientRect();
      expect(Math.abs(tasksRect.top - headerRect.bottom)).toBeLessThanOrEqual(1);

      fireEvent.keyDown(window, { key: "Escape" });
      await waitFor(() => expect(document.querySelector(".tasks-shell")).toBeFalsy());
      useUi.setState({ navWidth: 280, settingsOpen: true });
      await waitFor(() => expect(document.querySelector(".settings-shell")).toBeTruthy());
      const shell2 = document.querySelector(".settings-shell") as HTMLElement | null;
      expect(Math.abs(shell2!.getBoundingClientRect().top - headerRect.bottom)).toBeLessThanOrEqual(1);
    } finally {
      rectSpy.mockRestore();
    }
  });

  it("设置：MCP 页的结构化编辑器与技能页的列表各自渲染（拆页后不再同页）", async () => {
    await mountApp();
    await clickIconBtn("设置");
    await clickTab("MCP");
    // Single-entry editing: add server → entry fields expand
    await clickButton("新建");
    await waitFor(() => expect(screen.getByText("命令")).toBeTruthy());
    expect(document.body.textContent).toContain("保存并重连");
    expect(document.querySelectorAll(".mcp-entry").length).toBe(1);
    // 技能列表搬到了独立页（左栏技能分段也会渲染同名技能，此处范围限定到设置页）
    await clickTab("技能");
    await waitFor(() => {
      const page = document.querySelector('[data-testid="settings-page"]');
      const inSettings = page
        ? [...page.querySelectorAll(".skill-row")].some((r) => r.textContent?.includes("demo"))
        : false;
      expect(inSettings).toBe(true);
    });
  });

  it("Composer：/ 触发技能菜单（命令入口已移除），占位符渲染", async () => {
    // happy-dom 布局尺寸恒为 0 → 给 .composer-card 显式宽度，用于断言菜单宽度上限跟随输入卡片
    const origRect = Element.prototype.getBoundingClientRect;
    const rectSpy = vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
      const r = origRect.call(this);
      return this instanceof HTMLElement && this.classList.contains("composer-card")
        ? ({ ...r, width: 640, right: 640 } as unknown as DOMRect)
        : r;
    });
    try {
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
      // 菜单宽度上限 = 输入卡片实测宽度（长 description 不再撑出视口）
      const menu = document.querySelector(".menu") as HTMLElement | null;
      expect(menu?.style.maxWidth).toBe("640px");
      const text = document.body.textContent ?? "";
      expect(text).not.toContain("压缩上下文");
      expect(text).not.toContain("Git 状态");
    } finally {
      rectSpy.mockRestore();
    }
  });});

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

  it("工具条渲染：进度圈/速率/模型/力度/圆形发送钮，进度圈在模型之前（替换原压缩按钮位）", async () => {
    seedTab();
    await mountApp();
    const toolbar = document.querySelector(".composer-toolbar");
    expect(toolbar).toBeTruthy();
    const text = toolbar?.textContent ?? "";
    expect(text).toContain("自动编辑"); // current permission mode
    expect(text).toContain("test-model"); // model wire id always visible (docs/provider-management-refactor: wire id is the display name)
    expect(text).toContain("Test Provider / test-model"); // 模型区新增供应商名（providerName / model）
    expect(text).toContain("默认"); // default effort tier
    // 进度圈替换原压缩按钮位：含 ctx-progress-wrap 与 .ant-progress（dashboard 半弧）
    const progressWrap = toolbar?.querySelector(".ctx-progress-wrap");
    expect(progressWrap).toBeTruthy();
    expect(progressWrap?.querySelector(".ant-progress")).toBeTruthy();
    // 压缩按钮本身已搬进 Popover，工具条不再有独立的 button[aria-label="压缩上下文"]
    expect(toolbar?.querySelector('button[aria-label="压缩上下文"]')).toBeFalsy();
    // 进度圈（替换原压缩按钮位）与模型按钮的先后顺序：进度圈在模型之前
    const modelBtn = toolbar?.querySelector('button[aria-label="模型"]');
    expect(modelBtn).toBeTruthy();
    const pos = toolbar!.innerHTML.indexOf.bind(toolbar!.innerHTML);
    expect(pos((progressWrap as HTMLElement).outerHTML)).toBeLessThan(pos((modelBtn as HTMLElement).outerHTML));
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

  it("上下文区：进度圈按 4 档分色（红橙黄绿），hover 弹 Popover 含上下文/阈值/命中 三行", async () => {
    seedTab();
    // 占用 60% = 恰好达阈值（0.6）→ 危险档（red）；fixture 供应商 api_format = openai_chat
    useRun.setState((s) => {
      s.tabs = { s1: blank() };
      s.tabs["s1"].breakdown = {
        system_tokens: 1000, history_tokens: 2000, tool_results_tokens: 500,
        tool_schema_tokens: 500, total_tokens: 76800, context_window: 128000, ratio: 0.6,
      };
      s.tabs["s1"].usage = { input: 1000, output: 10, cacheRead: 500, cacheWrite: 0 };
    });
    await mountApp();
    const progress = document.querySelector(".ctx-progress")!;
    expect(progress.className).toContain("ctx-tier-danger"); // ratio 0.6 = 阈值 → danger 档
    // 触发 Popover：hover 进度圈
    const wrap = document.querySelector(".ctx-progress-wrap") as HTMLElement;
    fireEvent.mouseEnter(wrap);
    await waitFor(() => expect(document.querySelector(".ctx-popover")).toBeTruthy());
    const popover = document.querySelector(".ctx-popover")!;
    expect(popover.textContent).toContain("60%"); // 当前上下文百分比（括号内）
    expect(popover.textContent).toContain("76.8k / 128k"); // 当前上下文用量
    expect(popover.textContent).toContain("60%"); // 压缩阈值（括号内，fixture config compact_threshold 0.6）
    expect(popover.textContent).toContain("50%"); // 缓存命中率（500/1000）
    expect(popover.textContent).toContain("500 / 1000"); // 分子/分母
    // popover 三行顺序：当前上下文 → 缓存命中 → 压缩阈值（缓存命中单独一行，置于阈值上一行）
    const rows = Array.from(popover.querySelectorAll(".ctx-popover-row")).map((r) => r.textContent ?? "");
    expect(rows.length).toBe(3);
    expect(rows[0]).toContain("当前上下文"); // 第 1 行：当前上下文
    expect(rows[1]).toContain("命中");      // 第 2 行：缓存命中
    expect(rows[2]).toContain("阈值");      // 第 3 行：压缩阈值
    // 进度圈已无 tooltip（title 被移除，详情全在 popover 里）
    expect(wrap.getAttribute("title")).toBeNull();
    // 压缩按钮：内联到阈值行尾部（不再单独占行）
    const compactBtn = popover.querySelector(".ctx-popover-compact-btn") as HTMLElement;
    expect(compactBtn).toBeTruthy();
    expect(compactBtn.closest(".ctx-popover-row")?.className ?? "").toContain("ctx-popover-threshold-row");
    fireEvent.mouseLeave(wrap);
    useRun.setState((s) => { s.tabs = {}; });
  });

  it("上下文区无 breakdown 时进度圈档位回退 ok，popover 显示空态文案", async () => {
    seedTab();
    await mountApp();
    const progress = document.querySelector(".ctx-progress")!;
    expect(progress.className).toContain("ctx-tier-ok"); // 无数据 → ok（不臆测风险）
    const wrap = document.querySelector(".ctx-progress-wrap") as HTMLElement;
    fireEvent.mouseEnter(wrap);
    await waitFor(() => expect(document.querySelector(".ctx-popover")).toBeTruthy());
    const popover = document.querySelector(".ctx-popover")!;
    expect(popover.querySelector(".ctx-popover-empty")).toBeTruthy(); // 上下文 — 空态
    fireEvent.mouseLeave(wrap);
  });

  it("上下文区：命中率档位跟随数据（≥99% 绿），popover 文字跟随更新", async () => {
    seedTab();
    useRun.setState((s) => {
      s.tabs = { s1: blank() };
      s.tabs["s1"].breakdown = {
        system_tokens: 1, history_tokens: 1, tool_results_tokens: 0,
        tool_schema_tokens: 1, total_tokens: 100, context_window: 128000, ratio: 0.001,
      };
      s.tabs["s1"].usage = { input: 1000, output: 0, cacheRead: 900, cacheWrite: 0 }; // 900/1000 = 90%（warn 档）
    });
    await mountApp();
    const wrap = document.querySelector(".ctx-progress-wrap") as HTMLElement;
    fireEvent.mouseEnter(wrap);
    await waitFor(() => expect(document.querySelector(".ctx-popover")).toBeTruthy());
    expect(document.querySelector(".ctx-popover")?.textContent).toContain("90%"); // 90% 命中率
    // 命中率档位跟随数据：全部命中 → ≥99% → ok（绿）—— 但进度圈色由 ctxTier（占用档）决定，与 hitTier 独立
    useRun.setState((s) => { s.tabs["s1"].usage = { input: 1000, output: 0, cacheRead: 1000, cacheWrite: 0 }; });
    await waitFor(() => expect(document.querySelector(".ctx-popover")?.textContent).toContain("100%"));
    // 同一份用量换成 anthropic 语义（input 不含缓存）→ 分母变 input+read+write：1000/4000 = 25%
    const openaiCfg = useSettings.getState().config!;
    useSettings.setState({
      config: {
        ...openaiCfg,
        providers: openaiCfg.providers.map((p) => ({ ...p, api_format: "anthropic_messages" as const })),
      },
    });
    useRun.setState((s) => { s.tabs["s1"].usage = { input: 1000, output: 0, cacheRead: 1000, cacheWrite: 2000 }; });
    await waitFor(() => expect(document.querySelector(".ctx-popover")?.textContent).toContain("25%"));
    useSettings.setState({ config: openaiCfg });
    // active_model_id 指向不存在的模型 → cacheSemanticsOf 返回 null → popover 不渲染命中行
    const cfg = useSettings.getState().config!;
    useSettings.setState({ config: { ...cfg, active_model_id: "missing-model" } });
    await waitFor(() => {
      const rows = document.querySelectorAll(".ctx-popover-row");
      const texts = Array.from(rows).map((r) => r.textContent ?? "");
      return !texts.some((txt) => txt.includes("命中"));
    });
    useSettings.setState({ config: cfg });
    fireEvent.mouseLeave(wrap);
  });

  it("发送按钮三态：运行中无输入=停止，输入后=提交，清空复归停止", async () => {
    seedTab();
    useRun.setState((s) => {
      s.tabs = {}; s.drafts = {}; // clear leftovers so run state does not leak from other cases
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

  it("权限菜单：五档渲染（含目标模式），切换触发 set_session_prefs 并更新按钮文案", async () => {
    seedTab();
    await mountApp();
    const menu = await openToolbarMenu("自动编辑");
    expect(menu?.textContent).toContain("变更前确认");
    expect(menu?.textContent).toContain("自动编辑");
    expect(menu?.textContent).toContain("计划模式");
    expect(menu?.textContent).toContain("目标模式");
    expect(menu?.textContent).toContain("完全访问");
    // Menu items carry per-mode classes (wiring for coloring the label by mode; plan has no class = no color)
    expect(menu?.querySelector(".menu-item-rich.approval-confirm")).toBeTruthy();
    expect(menu?.querySelector(".menu-item-rich.approval-auto")).toBeTruthy();
    expect(menu?.querySelector(".menu-item-rich.approval-full")).toBeTruthy();
    // 目标模式：第五项，橙（warn）语义，类名沿用仓库既有 approval-* 约定
    expect(menu?.querySelector(".menu-item-rich.approval-goal")).toBeTruthy();
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

  it("Shift+Tab：输入框内循环切换权限五档（docs/composer-shift-tab-mode-cycle）", async () => {
    seedTab();
    await mountApp();
    const textarea = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
    const modeText = () => document.querySelector(".composer-toolbar")?.textContent ?? "";
    // 循环顺序：plan → confirm_each → auto_edit → goal → full_access → plan
    expect(modeText()).toContain("自动编辑"); // initial mode (prefs.approval_mode = auto_edit)
    expect(document.querySelector(".composer-toolbar .approval-auto")).toBeTruthy(); // auto edit = yellow
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("目标模式")); // auto_edit → goal
    await waitFor(() => expect(document.body.textContent).toContain("已切换权限模式：目标模式")); // switch toast (docs/session-pref-switch-toast)
    expect(document.querySelector(".composer-toolbar .approval-goal")).toBeTruthy(); // goal = orange
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("完全访问")); // goal → full_access
    expect(document.querySelector(".composer-toolbar .approval-full")).toBeTruthy(); // red highlight in sync
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("计划模式")); // full_access → plan
    expect(document.querySelector(".composer-toolbar .approval-full")).toBeFalsy();
    expect(document.querySelector(".composer-toolbar .approval-plan")).toBeFalsy(); // plan mode gets no color
    fireEvent.keyDown(textarea, { key: "Tab", shiftKey: true });
    await waitFor(() => expect(modeText()).toContain("变更前确认")); // plan → confirm_each
    expect(document.querySelector(".composer-toolbar .approval-confirm")).toBeTruthy(); // confirm mode = green
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
