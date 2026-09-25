// 设置全屏页容器契约（[docs/settings-fullscreen-shell](../../../docs/settings-fullscreen-shell.md) /
// 10 页（8 页重划 + MCP / 技能拆页）与注册表 [docs/settings-ia](../../../docs/settings-ia.md)）：
// 覆盖式全屏页必须**不影响正在运行的会话**——工作区全程挂载、只藏可见性、Esc 绝不停运行、
// 离开前有未保存改动时四条路径（切页 / 返回工作区 / 页内 Esc / 关窗退出）共用同一份三选拦截。
// 批② 起左导航自建（三组 10 页，替掉 antd Tabs），因此本文件同时守护：页名与页序、逐页脏点、
// 「关于」页（原 AboutModal 迁入第 8 页）、左下角不再有「关于」入口、macOS 菜单落关于页。
// mock 结构对齐 app.smoke.test.tsx（整树 App 挂载是唯一能验证「工作区仍挂载 + Esc 走全局键」的方式）。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// 事件 handler 记录（macOS 菜单 menu:action 的落点要能被真实触发：AppShell 自己 listen 该键）
const eventMocks = vi.hoisted(() => ({ handlers: new Map<string, (e: any) => void>() }));

// ---------- Tauri IPC mock ----------
const fixtureConfig = {
  schema_version: 2,
  providers: [
    {
      id: "p1", name: "Test Provider", api_format: "openai_chat",
      base_url: "https://api.example.com/v1", keys: ["***abcd"],
      models: [
        { id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000, reasoning_effort: null, vision: true, video: false },
      ],
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
  shell: { selection: null },
};

const fixtureSession = {
  id: "s1", title: "设置页测试会话", workspace: "/tmp/ws", model_id: "m1",
  created_at: "2026-08-30T00:00:00Z", updated_at: "2026-08-30T00:00:00Z", message_count: 0,
};

// 停止运行路径（Esc 不得触发）：记录 cancel_run 调用
const cancelRunLog: any[] = [];
// 退出应答（关窗路径的「留在原地」必须回后端，否则应用永远退不出去）
const exitAnswers: any[] = [];

/** 默认 IPC 实现（用例可 mockImplementation 覆盖，afterEach 复位） */
async function baseInvoke(cmd: string, args?: any) {
  switch (cmd) {
    case "ping": return "pong";
    case "activate_and_show": return null;
    case "get_config": return fixtureConfig;
    case "save_config": return null;
    case "list_sessions": return [fixtureSession];
    case "list_projects": return [];
    case "load_session": return [];
    case "create_session": return { session_id: "new-1", workspace: "/tmp/ws" };
    case "git_status": return { repo: false, entries: [] };
    case "git_user_info": return { name: "测试用户", email: "tester@example.com" };
    case "get_token_breakdown":
      return { system_tokens: 1, history_tokens: 2, tool_results_tokens: 0, tool_schema_tokens: 3, total_tokens: 6, context_window: 128000, ratio: 0.01 };
    case "get_token_stats": return [];
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "list_skills": return [];
    case "list_agents": return [];
    case "search_workspace_paths": return [];
    case "list_workspace_dir": return { entries: [] };
    case "list_available_shells": return [];
    case "get_session_prefs": return { approval_mode: "auto_edit", model_id: null, reasoning_effort: null };
    case "set_session_prefs": return null;
    // 关于页（原弹框迁入）：版本号 + 两个外链动作
    case "app_version": return "0.2.0";
    case "open_data_dir": return null;
    case "open_url": return null;
    case "cancel_run": cancelRunLog.push(args?.sessionId ?? ""); return null;
    case "resolve_exit_request": exitAnswers.push(args?.action); return null;
    default: return null;
  }
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(baseInvoke),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, cb: (e: any) => void) => {
    eventMocks.handlers.set(name, cb);
    return vi.fn();
  }),
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
  // 批③ 搜索命中定位会调 scrollIntoView：happy-dom 可能没有 → 垫桩（避免用例因环境差异变红）
  (Element.prototype as any).scrollIntoView = (Element.prototype as any).scrollIntoView ?? (() => {});
});

import App from "../App";
import { useUi } from "../stores/ui";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { PAGE_GROUPS, PAGE_ORDER, SETTINGS_ITEMS, type PageKey } from "../features/panels/settingsRegistry";
// 清理提示的去重记录键（与启动轻提示共用一处口径；键名本身就是契约）
import { CLEANUP_NOTICE_SEEN_KEY } from "../utils/cleanupNotice";
// 旧格式历史清理（分段 JSONL 落地后的显式入口）的两个返回结构
import type { LegacyCleanupOutcome, LegacyCleanupPreview } from "../ipc/types";

/** 当前 IPC mock（用例覆盖实现后再复位） */
async function invokeMock() {
  return (await import("@tauri-apps/api/core")).invoke as any;
}

// ---------- helpers ----------
/** 整树挂载 + 开一个临时会话（有活跃会话才有 Composer；空态引导入口） */
async function mountWithSession() {
  render(<App />);
  await waitFor(() => expect(document.body.textContent ?? "").toContain("项目"), { timeout: 5000 });
  await new Promise((r) => setTimeout(r, 120));
  if (!document.querySelector(".composer")) {
    const guide = await waitFor(
      () => {
        const btn = Array.from(document.querySelectorAll(".chat-empty-guide .guide-actions button")).find((b) =>
          (b.textContent ?? "").includes("临时会话"),
        ) as HTMLButtonElement | undefined;
        expect(btn).toBeTruthy();
        return btn!;
      },
      { timeout: 5000 },
    );
    fireEvent.click(guide);
  }
  await waitFor(() => expect(document.querySelector(".composer")).toBeTruthy(), { timeout: 5000 });
  await new Promise((r) => setTimeout(r, 60));
}

/** 打开设置（左下角状态区的设置入口；aria-label = app.settings） */
async function openSettings() {
  const btn = document.querySelector('button[aria-label="设置"]') as HTMLButtonElement | null;
  expect(btn).toBeTruthy();
  fireEvent.click(btn!);
  await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy());
  await new Promise((r) => setTimeout(r, 60));
}

/** antd 两字按钮会插空格：比对前去空白 */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

/** 导航项（自建导航：[data-page] 定位，比按文本找更稳） */
function navItem(page: string): HTMLElement | null {
  return document.querySelector(`[data-testid="settings-page"] .settings-nav-item[data-page="${page}"]`);
}

/** 设置页导航项按页名文本点击（脏圆点是空 span，不影响 textContent） */
function clickNavTab(label: string) {
  const item = Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item')).find(
    (x) => (x.textContent ?? "").trim() === label,
  );
  if (!item) throw new Error(`nav item not found: ${label}`);
  fireEvent.click(item);
}

function activeNavTabText(): string {
  return document.querySelector('[data-testid="settings-page"] .settings-nav-item-active')?.textContent?.trim() ?? "";
}

/** 某页导航项是否带脏圆点 */
function navDot(page: string): boolean {
  return !!navItem(page)?.querySelector(".settings-dirty-dot");
}

/** 导航上共有几个脏圆点（操作条那枚不算） */
function navDotCount(): number {
  return document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item .settings-dirty-dot').length;
}

/** 打开设置并切到指定页（未保存改动存在时切页会弹三选，故本 helper 只用于基线态） */
async function openPage(label: string) {
  await openSettings();
  clickNavTab(label);
  await waitFor(() => expect(activeNavTabText()).toBe(label));
}

/** 按 Form.Item 标签找页内控件 */
function controlByLabel(label: string): HTMLElement {
  const item = Array.from(document.querySelectorAll('[data-testid="settings-page"] .ant-form-item')).find((fi) =>
    (fi.querySelector(".ant-form-item-label")?.textContent ?? "").includes(label),
  );
  if (!item) throw new Error(`form item not found: ${label}`);
  return item as HTMLElement;
}

function promptArea(): HTMLTextAreaElement {
  const ta = document.querySelector('[data-testid="settings-page"] .settings-pane-body textarea') as HTMLTextAreaElement | null;
  if (!ta) throw new Error("custom prompt textarea not found");
  return ta;
}

/** 制造未保存改动：工作区与智能体页的自定义提示词是 draft 字段（非即时生效项） */
function makeDirty() {
  clickNavTab("工作区与智能体");
  fireEvent.change(promptArea(), { target: { value: "改过的提示词" } });
}

/** 让位类节点归类（精确集合断言用；意外节点会以 unexpected:<class> 出现，便于定位新增容器） */
function coveredKind(el: Element): string {
  if (el.classList.contains("ant-layout-sider")) return "sider";
  if (el.classList.contains("ant-layout-content")) return "content";
  if (el.classList.contains("rb-resize-handle") && el.classList.contains("resize-nav")) return "handle-nav";
  if (el.classList.contains("rb-resize-handle") && el.classList.contains("resize-right")) return "handle-right";
  return `unexpected:${el.className}`;
}

function leaveModalText(): string {
  return document.body.textContent ?? "";
}

/** 三选拦截是否处于挂起态（设置页根节点上的状态钩子；antd 弹框关闭后仍留 DOM，不能靠可见性判定） */
function confirmPending(): boolean {
  return document.querySelector('[data-testid="settings-page"]')?.getAttribute("data-confirm-open") === "1";
}

/** 三选拦截弹框是否可见（antd Modal 关闭后仍留在 DOM，用 wrap 的 display:none 判定） */
function leaveModalVisible(): boolean {
  const wraps = Array.from(document.querySelectorAll<HTMLElement>(".ant-modal-wrap"));
  const visible = wraps.some((w) => w.style.display !== "none");
  return visible && leaveModalText().includes("有未保存的设置改动");
}

/** 让活跃 Tab 处于运行中（不改其它字段，避免破坏 ChatMessages 依赖的形状） */
function markActiveRunning() {
  expect(useRun.getState().tabs["new-1"]).toBeTruthy();
  useRun.setState((s) => {
    s.tabs["new-1"].running = true;
  });
}

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  // zustand store 是模块级单例：面板开关/脏标记/退出请求都要复位，否则用例间串味
  useUi.setState({
    settingsOpen: false,
    settingsTab: "appearance",
    settingsDirty: false,
    exitRequest: null,
    closeTabRequest: null,
    tasksOpen: false,
    statsOpen: false,
    rightBarOpen: true,
    rbTab: "info",
    notifications: [],
  });
  // 语言同样是模块级单例：本文件有一条切换语言的用例，不复位会让后续用例整树变英文
  localStorage.clear();
  useUi.setState({ language: "zh-CN" });
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
  // 会话/Tab 也是模块级单例：不清就再也回不到空态（空态引导入口是建临时会话的唯一途径）
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  cancelRunLog.length = 0;
  exitAnswers.length = 0;
  eventMocks.handlers.clear();
});

describe("设置全屏页：覆盖工作区但不影响运行中会话", () => {
  it("打开设置：设置页存在、工作区仍挂载且被 .workspace-covered 覆盖（不 display:none）", async () => {
    await mountWithSession();
    // 打开前的工作区锚点
    expect(document.querySelector(".chat-messages")).toBeTruthy();
    expect(document.querySelector(".composer")).toBeTruthy();
    expect(document.querySelector(".project-nav")).toBeTruthy();

    await openSettings();

    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy();
    // 工作区节点全部还在 DOM 里（卸载会让滚动容器/ResizeObserver 丢状态，进而干扰运行中任务）
    expect(document.querySelector(".chat-messages")).toBeTruthy();
    expect(document.querySelector(".composer")).toBeTruthy();
    expect(document.querySelector(".project-nav")).toBeTruthy();
    expect(document.querySelector(".right-bar")).toBeTruthy();
    // 让位类覆盖的是**精确集合**：Sider + Content + 两个栏宽分隔条
    // （分隔条是 Sider/Content 的兄弟节点，光靠覆盖层 z-index 挡不住键盘焦点 → 也必须进集合）
    const covered = Array.from(document.querySelectorAll(".workspace-covered"));
    expect(covered.map(coveredKind)).toEqual(["sider", "content", "handle-nav", "handle-right"]);
    expect(covered.every((el) => el.getAttribute("aria-hidden") === "true")).toBe(true);
    // 右侧栏（RightBar）在 Content 内，同样被覆盖
    expect(document.querySelector(".right-bar")?.closest(".workspace-covered")).toBeTruthy();
    // 分隔条被踢出 Tab 序：键盘 ←/→ 不能再静默改栏宽
    const handles = Array.from(document.querySelectorAll<HTMLElement>(".rb-resize-handle"));
    expect(handles.length).toBe(2);
    for (const el of handles) expect(el.tabIndex).toBe(-1);
    // 10 页导航：顺序 = PAGE_ORDER（注册表单源），分组标题三组齐备
    const navLabels = Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item')).map((x) =>
      (x.textContent ?? "").trim(),
    );
    expect(navLabels).toEqual(["界面", "模型与供应商", "网络与连接", "安全与审批", "MCP", "技能", "写入后检查与校验", "工作区与智能体", "日志", "关于"]);
    expect(
      Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-group')).map((x) =>
        (x.textContent ?? "").trim(),
      ),
    ).toEqual(["外观与模型", "安全与能力", "诊断与其他"]);
    // 导航项与注册表的 PAGE_ORDER 一一对应（防止「加了页却没登记」或反过来）
    expect(
      Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item')).map((x) =>
        x.getAttribute("data-page"),
      ),
    ).toEqual(PAGE_ORDER);
    expect(PAGE_GROUPS.flatMap((g) => g.pages)).toEqual(PAGE_ORDER);
    // 落地页 = 注册表默认页
    expect(activeNavTabText()).toBe("界面");
  });

  it("样式契约：.workspace-covered 只藏可见性，绝不 display:none；左导航自建（无 antd Tabs 残留）", () => {
    const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");
    expect(appCss).toMatch(/\.settings-shell\s*\{[^}]*position:\s*absolute/);
    expect(appCss).toMatch(/\.workspace-covered\s*\{[^}]*visibility:\s*hidden/);
    expect(appCss).toMatch(/\.workspace-covered\s*\{[^}]*pointer-events:\s*none/);
    // 硬约束：display:none 会让 ResizeObserver 测到 0 尺寸、滚动容器错乱
    expect(appCss).not.toMatch(/\.workspace-covered\s*\{[^}]*display:\s*none/);
    // 旧的弹窗内页容器已随批① 退役
    expect(appCss).not.toContain(".settings-body");
    // 批②：导航自建 → 导航列内的 antd Tabs 规则全部退场；组标题沿用 .nav-section-title 的度量
    expect(appCss).not.toContain(".settings-nav .ant-tabs");
    expect(appCss).toMatch(/\.settings-nav-group\s*\{[^}]*font-size:\s*11px[^}]*var\(--ws-dim\)/);
    expect(appCss).toMatch(/\.settings-nav-item-active\s*\{[^}]*background:\s*var\(--ws-highlight\)/);
    // 写入后检查的两列网格类收回 app.css（不再用内联 style）
    expect(appCss).toMatch(/\.postcheck-grid\s*\{[^}]*grid-template-columns:\s*repeat\(2,\s*minmax\(180px,\s*1fr\)\)/);
    // 已删除的 LSP 样式类不得残留
    expect(appCss).not.toContain(".lsp-budget-grid");
    expect(appCss).not.toContain(".validation-row");
    const pageSrc = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../features/panels/SettingsPage.tsx"), "utf8");
    expect(pageSrc).not.toContain("gridTemplateColumns");
  });

  it("打开设置不改动运行中会话的运行态与消息流，也不触发停止运行", async () => {
    await mountWithSession();
    markActiveRunning();
    const before = JSON.parse(JSON.stringify(useRun.getState().tabs["new-1"]));

    await openSettings();
    await new Promise((r) => setTimeout(r, 80));

    const after = JSON.parse(JSON.stringify(useRun.getState().tabs["new-1"]));
    expect(after.running).toBe(true);
    expect(after.items.length).toBe(before.items.length);
    expect(after.queue.length).toBe(before.queue.length);
    expect(cancelRunLog).toEqual([]);
  });

  it("「返回工作区」：设置页消失、工作区恢复（运行中的会话照常）", async () => {
    await mountWithSession();
    markActiveRunning();
    await openSettings();

    fireEvent.click(buttonByText("返回工作区"));
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());

    expect(document.querySelectorAll(".workspace-covered").length).toBe(0);
    // 分隔条回到可达状态（让位类撤掉 → tabIndex 复原）
    for (const el of document.querySelectorAll<HTMLElement>(".rb-resize-handle")) expect(el.tabIndex).toBe(0);
    expect(document.querySelector(".composer")).toBeTruthy();
    expect(useRun.getState().tabs["new-1"].running).toBe(true);
    expect(cancelRunLog).toEqual([]);
  });

  it("页内 Esc：走「返回工作区」，绝不触发停止运行中会话", async () => {
    await mountWithSession();
    markActiveRunning();
    await openSettings();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());

    expect(cancelRunLog).toEqual([]);
    expect(useRun.getState().tabs["new-1"].running).toBe(true);
  });

  it("运行中指示：数量为 0 不渲染，有运行中会话时显示数量且点即返回工作区", async () => {
    await mountWithSession();
    await openSettings();
    expect(document.querySelector(".run-indicator")).toBeFalsy();
    // 返回工作区后标记运行，再进设置
    fireEvent.click(buttonByText("返回工作区"));
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    markActiveRunning();
    await openSettings();

    const indicator = document.querySelector(".run-indicator") as HTMLElement;
    expect(indicator).toBeTruthy();
    expect(indicator.textContent ?? "").toContain("1 个会话运行中");
    fireEvent.click(indicator);
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    expect(useRun.getState().tabs["new-1"].running).toBe(true);
  });

  it("可达性契约：设置页是 dialog，打开时焦点落在导航首项", async () => {
    await mountWithSession();
    await openSettings();

    const page = document.querySelector('[data-testid="settings-page"]') as HTMLElement;
    expect(page.getAttribute("role")).toBe("dialog");
    expect(page.getAttribute("aria-modal")).toBe("true");
    // 不引入焦点陷阱库：只保证打开时焦点不在工作区（工作区已 aria-hidden）
    // 批③：焦点定位改用专用类名 .settings-nav-back（**防御性**写法：搜索框挂载时值恒为空、
    // allowClear 的清除按钮不存在，故「泛选会把焦点抢到清除键上」当前不可构造验证）
    const back = document.querySelector('[data-testid="settings-page"] .settings-nav-back') as HTMLButtonElement;
    expect(back).toBeTruthy();
    expect(document.activeElement).toBe(back);
  });

  it("运行中指示：两个会话同时运行显示 2", async () => {
    await mountWithSession();
    markActiveRunning();
    // 指示只数 run store 里 running 的 Tab：再加一个运行中 Tab 即计为 2
    useRun.setState((s) => {
      s.tabs["extra-run"] = { ...s.tabs["new-1"], running: true };
    });
    await openSettings();

    const indicator = document.querySelector(".run-indicator") as HTMLElement;
    expect(indicator).toBeTruthy();
    expect(indicator.textContent ?? "").toContain("2 个会话运行中");
  });
});

describe("设置全屏页：未保存改动的三选拦截（切页 / 返回 / Esc / 关窗）", () => {
  it("切页路径：先弹三选，留在原地则停在原页，放弃改动则跳页", async () => {
    await mountWithSession();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    clickNavTab("界面");
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    // 三选齐全
    expect(buttonByText("保存并离开")).toBeTruthy();
    expect(buttonByText("放弃改动")).toBeTruthy();
    expect(buttonByText("留在原地")).toBeTruthy();
    // 留在原地：仍在原页，未保存改动保留，弹框收起
    fireEvent.click(buttonByText("留在原地"));
    await waitFor(() => expect(confirmPending()).toBe(false));
    expect(activeNavTabText()).toBe("工作区与智能体");
    expect(document.querySelector(".settings-dirty-dot")).toBeTruthy();

    // 放弃改动：跳页且脏点消失
    clickNavTab("界面");
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    fireEvent.click(buttonByText("放弃改动"));
    await waitFor(() => expect(activeNavTabText()).toBe("界面"));
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeFalsy());
  });

  it("返回工作区路径：三选生效（保存并离开 → 落盘后离开）", async () => {
    await mountWithSession();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    fireEvent.click(buttonByText("返回工作区"));
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    fireEvent.click(buttonByText("保存并离开"));
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    // 保存成功 = 走 save_config 全量提交
    const { invoke } = await import("@tauri-apps/api/core");
    expect((invoke as any).mock.calls.some((c: any[]) => c[0] === "save_config")).toBe(true);
  });

  it("页内 Esc 路径：先弹三选，不关闭设置页也不停运行", async () => {
    await mountWithSession();
    markActiveRunning();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy();
    expect(cancelRunLog).toEqual([]);
    // 弹框开着时再按 Esc：归弹框（= 留在原地），不返回工作区
    fireEvent.keyDown(window, { key: "Escape" });
    await new Promise((r) => setTimeout(r, 80));
    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy();
    expect(cancelRunLog).toEqual([]);
  });

  it("关窗/退出路径：先由设置侧三选接管，「留在原地」＝取消这次退出", async () => {
    await mountWithSession();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    // 后端 app:exit_requested 落到 ui.exitRequest（runHandlers 的 handler 只做这一件事）
    act(() => {
      useUi.setState({ exitRequest: { running: ["new-1"] } });
    });
    await waitFor(() => expect(leaveModalVisible()).toBe(true));

    fireEvent.click(buttonByText("留在原地"));
    await waitFor(() => expect(useUi.getState().exitRequest).toBeNull());
    // 必须回后端应答（否则后端一直等在 ExitRequested 上，应用退不出去）
    expect(exitAnswers).toEqual(["cancel"]);
    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy();
  });

  it("关窗/退出路径：放弃改动后交回运行中会话确认", async () => {
    await mountWithSession();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    act(() => {
      useUi.setState({ exitRequest: { running: ["new-1"] } });
    });
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    fireEvent.click(buttonByText("放弃改动"));

    // 脏标记清零 → AppShell 的退出确认接管
    await waitFor(() => expect(leaveModalText()).toContain("仍有任务在运行"));
    expect(exitAnswers).toEqual([]);
  });

  it("顶部「取消」= 放弃全部未保存改动并返回工作区（不再二次确认）", async () => {
    await mountWithSession();
    await openSettings();
    makeDirty();
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());

    fireEvent.click(buttonByText("取消"));
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    expect(confirmPending()).toBe(false);
    expect(useUi.getState().settingsDirty).toBe(false);
  });
});

describe("设置全屏页：逐页脏标记与深链", () => {
  it("脏标记空值归一：自定义提示词 null → 输入 → 清空后不再脏，「返回工作区」也不弹三选", async () => {
    await mountWithSession();
    await openPage("工作区与智能体");

    // 原值 null，输入文字 → 脏
    fireEvent.change(promptArea(), { target: { value: "只用中文回答" } });
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());
    expect(useUi.getState().settingsDirty).toBe(true);

    // 再全部删空：回到原值（null 与 "" 归一）→ 不脏
    fireEvent.change(promptArea(), { target: { value: "" } });
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeFalsy());
    expect(useUi.getState().settingsDirty).toBe(false);

    // 此时返回工作区直接离开，不得误弹三选
    fireEvent.click(buttonByText("返回工作区"));
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    expect(leaveModalVisible()).toBe(false);
    expect(confirmPending()).toBe(false);
  });

  it("脏标记空值归一：proxy=null 时点本来就选中态的「系统代理」不算改动，换成无代理才脏", async () => {
    await mountWithSession();
    await openPage("网络与连接");

    // config.proxy = null 等价于后端 ProxyConfig::default()（mode = system）
    const cardByTitle = (title: string) =>
      Array.from(document.querySelectorAll(".proxy-mode-card")).find((el) =>
        (el.querySelector(".proxy-mode-title")?.textContent ?? "") === title,
      ) as HTMLElement;
    fireEvent.click(cardByTitle("系统代理").querySelector(".ant-radio-input") as HTMLElement);
    await new Promise((r) => setTimeout(r, 60));
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();

    // 真实改动（切到无代理）仍要亮脏点
    fireEvent.click(cardByTitle("无代理").querySelector(".ant-radio-input") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());
  });

  it("脏标记空值归一：空命令不算改动，填了命令才脏", async () => {
    await mountWithSession();
    await openPage("写入后检查与校验");

    // 默认命令为空（draft 里的 post_write_check 与后端 serde default 等价）→ 不该染脏
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();

    // 填了真实命令 → 脏
    const input = document.querySelector('[data-setting-id="post_write_check.command"] input') as HTMLElement;
    fireEvent.change(input, { target: { value: "npx eslint {file}" } });
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());
  });

  it("按页打点：只标改动页，保存后清空", async () => {
    await mountWithSession();
    await openSettings();
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();

    makeDirty();
    await waitFor(() => expect(document.querySelectorAll(".settings-dirty-dot").length).toBeGreaterThan(0));
    // 「工作区与智能体」页的导航项带点（操作条那枚另算）
    expect(navDot("agent")).toBe(true);
    expect(navDotCount()).toBe(1);

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeFalsy());
    expect(useUi.getState().settingsDirty).toBe(false);
  });

  it("即时生效项不打点：切界面语言不产生未保存状态", async () => {
    await mountWithSession();
    await openPage("界面");
    // 界面语言是页面首组，选择后仍即时生效且不产生脏点。
    const languageItem = controlByLabel("界面语言");
    expect(languageItem.querySelector(".ant-form-item-label")?.textContent).toContain("界面语言");
    expect(languageItem.querySelector(".settings-row-description")?.textContent).toBe("界面显示的语言");
    const select = languageItem.querySelector(".ant-select") as HTMLElement;
    expect(select).toBeTruthy();
    fireEvent.mouseDown(select);
    await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy(), { timeout: 3000 });
    const option = Array.from(document.querySelectorAll(".ant-select-item-option")).find(
      (o) => (o.getAttribute("title") ?? o.textContent ?? "").includes("English"),
    ) as HTMLElement;
    expect(option).toBeTruthy();
    fireEvent.click(option);
    await new Promise((r) => setTimeout(r, 80));
    expect(useUi.getState().language).toBe("en-US");
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();
  });

  it("深链：带页跳该页；无参保持当前页不重置", async () => {
    await mountWithSession();
    await openSettings();
    expect(activeNavTabText()).toBe("界面");

    // 带参（认证错误卡「打开模型设置」走同一条路径）
    act(() => {
      useUi.getState().showSettings("providers");
    });
    await waitFor(() => expect(activeNavTabText()).toBe("模型与供应商"));
    expect(document.querySelector('[data-testid="settings-page"] .settings-pane-body')?.textContent ?? "").toContain("添加供应商");

    // 无参：保持当前页（有意变更，旧实现一律回 general）
    act(() => {
      useUi.getState().showSettings();
    });
    await new Promise((r) => setTimeout(r, 60));
    expect(activeNavTabText()).toBe("模型与供应商");
    expect(useUi.getState().settingsOpen).toBe(true);
  });
});

describe("设置全屏页：逐页脏点由 PAGE_FIELDS 驱动", () => {
  it("改该页任一配置项 → 只亮该页脏点；改回原值 → 脏点消失（六页逐一验证）", async () => {
    await mountWithSession();
    await openSettings();

    // ① 模型与供应商：AI 回复语言（config.ui.ai_language）
    clickNavTab("模型与供应商");
    await waitFor(() => expect(activeNavTabText()).toBe("模型与供应商"));
    const aiLang = controlByLabel("AI 语言").querySelector("input") as HTMLInputElement;
    fireEvent.change(aiLang, { target: { value: "日本語" } });
    await waitFor(() => expect(navDot("providers")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.change(aiLang, { target: { value: "" } });
    await waitFor(() => expect(navDotCount()).toBe(0));

    // ② 网络与连接：代理模式（config.proxy，null 折默认对象）
    clickNavTab("网络与连接");
    const card = (title: string) =>
      Array.from(document.querySelectorAll(".proxy-mode-card")).find((el) =>
        (el.querySelector(".proxy-mode-title")?.textContent ?? "") === title,
      ) as HTMLElement;
    fireEvent.click(card("无代理").querySelector(".ant-radio-input") as HTMLElement);
    await waitFor(() => expect(navDot("network")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.click(card("系统代理").querySelector(".ant-radio-input") as HTMLElement);
    await waitFor(() => expect(navDotCount()).toBe(0));

    // ③ 安全与审批：危险命令确认开关（config.approval.enabled）
    clickNavTab("安全与审批");
    const approvalSwitch = document.querySelectorAll('[data-testid="settings-page"] .ant-form-item .ant-switch')[0] as HTMLElement;
    fireEvent.click(approvalSwitch);
    await waitFor(() => expect(navDot("security")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.click(approvalSwitch);
    await waitFor(() => expect(navDotCount()).toBe(0));

    // ④ MCP：条目（独立 mcp.json 的文本基线，不走 config）——拆页后脏点跟着 mcp 页走
    //   注意：空名条目会被序列化丢掉，所以要先填名字才真算改动
    clickNavTab("MCP");
    fireEvent.click(buttonByText("新建"));
    const mcpName = document.querySelector(".mcp-entry input") as HTMLInputElement;
    fireEvent.change(mcpName, { target: { value: "fs" } });
    await waitFor(() => expect(navDot("mcp")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.click(document.querySelector(".ant-modal-footer button") as HTMLElement);
    await waitFor(() => expect(navDotCount()).toBe(0));

    // ⑤ 工作区与智能体：自定义提示词（config.custom_prompt）
    clickNavTab("工作区与智能体");
    fireEvent.change(promptArea(), { target: { value: "只用中文" } });
    await waitFor(() => expect(navDot("agent")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.change(promptArea(), { target: { value: "" } });
    await waitFor(() => expect(navDotCount()).toBe(0));

    // ⑥ 日志：会话详细日志开关（config.log.session_verbose）
    clickNavTab("日志");
    const verboseSwitch = document.querySelectorAll('[data-testid="settings-page"] .ant-form-item .ant-switch')[0] as HTMLElement;
    fireEvent.click(verboseSwitch);
    await waitFor(() => expect(navDot("logs")).toBe(true));
    expect(navDotCount()).toBe(1);
    fireEvent.click(verboseSwitch);
    await waitFor(() => expect(navDotCount()).toBe(0));
  });

  it("界面与关于两页只有即时生效项：改主题/字号开关/自动更新都不产生脏点", async () => {
    await mountWithSession();
    await openSettings();

    // 界面页：切主题（localStorage ws_theme，即时生效）
    clickNavTab("界面");
    await waitFor(() => expect(activeNavTabText()).toBe("界面"));
    const darkThemeOption = Array.from(document.querySelectorAll(".settings-theme-option")).find((o) => (o.textContent ?? "").includes("暗")) as HTMLElement;
    fireEvent.click(darkThemeOption);
    await new Promise((r) => setTimeout(r, 80));
    expect(useUi.getState().theme).toBe("dark");
    expect(navDotCount()).toBe(0);

    // 关于页：切「启动时自动检查更新」（localStorage ws_auto_update，即时生效）
    clickNavTab("关于");
    await waitFor(() => expect(activeNavTabText()).toBe("关于"));
    expect(navDotCount()).toBe(0);
    const autoUpdateSwitch = document.querySelector('[data-testid="settings-page"] .ant-switch') as HTMLElement;
    fireEvent.click(autoUpdateSwitch);
    await new Promise((r) => setTimeout(r, 80));
    expect(localStorage.getItem("ws_auto_update")).toBe("false");
    expect(navDotCount()).toBe(0);
    expect(useUi.getState().settingsDirty).toBe(false);
  });
});

describe("设置页：关于（原 AboutModal 弹框迁入第 8 页）", () => {
  it("身份信息 + 版本号懒加载（进关于页拉一次 app_version）", async () => {
    await mountWithSession();
    await openPage("关于");

    expect(document.querySelector(".about-logo")).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("CodeWave");
    expect(document.body.textContent ?? "").toContain("本地优先的桌面 AI 编程 Agent");
    await waitFor(() => expect(document.querySelector(".about-version")?.textContent).toBe("0.2.0"));
    const { invoke } = await import("@tauri-apps/api/core");
    expect((invoke as any).mock.calls.some((c: any[]) => c[0] === "app_version")).toBe(true);
    // 地址栏 / 版本号不是可保存项：本页恒不脏
    expect(navDotCount()).toBe(0);
  });

  // 两条断言自退役的 about.modal.test.tsx：版本串是后端拼好的整串（含 commit 短 sha 时也不拆不改），
  // 且空 sha 不得渲染成空括号。
  it("版本串带 commit 短 sha 时原样透传（0.2.0 (a1b2c3d)）", async () => {
    await mountWithSession();
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) =>
      cmd === "app_version" ? "0.2.0 (a1b2c3d)" : baseInvoke(cmd, args),
    );
    await openPage("关于");

    await waitFor(() => expect(document.querySelector(".about-version")?.textContent).toBe("0.2.0 (a1b2c3d)"));
  });

  it("裸版本号不得渲染出空括号 ()（sha 为空时不留占位符）", async () => {
    await mountWithSession();
    await openPage("关于");

    await waitFor(() => expect(document.querySelector(".about-version")?.textContent).toBe("0.2.0"));
    expect(document.querySelector(".about-version")?.textContent ?? "").not.toMatch(/\(\s*\)/);
    // 整页也不得有「空括号」这类 sha 占位残留（等价于原弹框用例的全量断言）
    expect(document.querySelector(".settings-pane-body")?.textContent ?? "").not.toMatch(/\(\s*\)/);
  });

  it("版本命令失败：降级为占位符 ?.?.?，身份信息照常渲染", async () => {
    await mountWithSession();
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "app_version") throw new Error("boom");
      return baseInvoke(cmd, args);
    });
    await openPage("关于");

    await waitFor(() => expect(document.querySelector(".about-version")?.textContent).toBe("?.?.?"));
    expect(document.body.textContent ?? "").toContain("CodeWave");
  });

  it("版式：「检查更新」按钮跟在版本号后（同一 flex 行，兄弟锚点非嵌套）", async () => {
    await mountWithSession();
    await openPage("关于");

    await waitFor(() => expect(document.querySelector(".about-version")?.textContent).toBe("0.2.0"));
    const versionAnchor = document.querySelector('[data-setting-id="app.version"]')!;
    const checkAnchor = document.querySelector('[data-setting-id="app.check_updates"]')!;
    expect(versionAnchor).toBeTruthy();
    expect(checkAnchor).toBeTruthy();
    // 两者同属版本行的 flex 行，且按钮锚点在版本锚点之后（DOM 顺序 = 视觉顺序）
    const row = versionAnchor.parentElement!;
    expect(row.className).toContain("settings-update-row");
    expect(row).toBe(checkAnchor.parentElement);
    expect(Array.from(row.children)).toEqual([versionAnchor, checkAnchor]);
    // 兄弟而非嵌套：嵌套会让外层高亮框套住整行（搜索定位的范围与命中项不一致）
    expect(versionAnchor.contains(checkAnchor)).toBe(false);
    expect((checkAnchor.querySelector("button")?.textContent ?? "").replace(/\s/g, "").includes("检查更新")).toBe(true);
    // 「更新」行只剩自动更新开关（按钮不再在该行）
    const updatesAnchor = document.querySelector('[data-setting-id="ui.auto_update"]')!;
    expect(updatesAnchor.parentElement?.querySelector('[data-setting-id="app.check_updates"]')).toBeNull();
    // 「即时生效」改挂在标题的括号里：不再作为行尾标注（flex 行 + 标题的 margin-right:auto
    // 会把它推到最右侧，实际没人会看到），行内也不再有 .settings-instant
    expect(document.body.textContent ?? "").toContain("更新（即时生效）");
  });

  it("数据目录 / 日志目录 / 代码仓库 / 许可证四个入口走对应 IPC；失败就地提示且不离开设置页", async () => {
    await mountWithSession();
    await openPage("关于");

    fireEvent.click(buttonByText("打开数据目录"));
    await waitFor(async () =>
      expect((await invokeMock()).mock.calls.some((c: any[]) => c[0] === "open_data_dir")).toBe(true),
    );
    // 批④ 新增入口：日志目录复用 open_logs_dir（右栏「日志」Tab 的入口保留，场景不同）
    fireEvent.click(buttonByText("打开日志目录"));
    await waitFor(async () =>
      expect((await invokeMock()).mock.calls.some((c: any[]) => c[0] === "open_logs_dir")).toBe(true),
    );
    fireEvent.click(buttonByText("打开代码仓库"));
    await new Promise((r) => setTimeout(r, 60));
    const invoke = await invokeMock();
    expect(
      invoke.mock.calls.some(
        (c: any[]) => c[0] === "open_url" && JSON.stringify(c[1]) === JSON.stringify({ url: "https://github.com/Yangshifu1024/CodeWave" }),
      ),
    ).toBe(true);
    // 批④ 新增入口：开源许可证走同一仓库默认分支（main）的 LICENSE
    fireEvent.click(buttonByText("查看许可证"));
    await waitFor(async () =>
      expect(
        (await invokeMock()).mock.calls.some(
          (c: any[]) =>
            c[0] === "open_url" &&
            JSON.stringify(c[1]) === JSON.stringify({ url: "https://github.com/Yangshifu1024/CodeWave/blob/main/LICENSE" }),
        ),
      ).toBe(true),
    );

    // 失败路径：就地报错，设置页不关也不跳页（四个入口共用同一条提示路径）
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "open_data_dir" || cmd === "open_logs_dir") throw new Error("no file manager");
      return baseInvoke(cmd, args);
    });
    fireEvent.click(buttonByText("打开数据目录"));
    // 判别性断言：错误落在**触发行**内（旧实现统一堆在 Form 末尾，那样这里会红）
    const dataDirAnchor = document.querySelector('[data-setting-id="app.data_dir"]') as HTMLElement;
    await waitFor(() => expect(dataDirAnchor.querySelector(".about-error")?.textContent ?? "").toContain("no file manager"));
    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy();
    expect(activeNavTabText()).toBe("关于");
  });

  it("左下角状态区不再有「关于」入口（任务 / 统计 / 设置保留）", async () => {
    await mountWithSession();
    const footer = document.querySelector(".sider-footer") as HTMLElement;
    expect(footer).toBeTruthy();
    for (const label of ["任务", "统计", "设置"]) {
      expect(footer.querySelector(`button[aria-label="${label}"]`)).toBeTruthy();
    }
    expect(footer.querySelector('button[aria-label="关于"]')).toBeFalsy();
    expect(document.querySelectorAll(".sider-footer button").length).toBe(3);
  });

  it("macOS 菜单 menu-about 落到设置页「关于」（不再是独立弹框）", async () => {
    await mountWithSession();
    await waitFor(() => expect(eventMocks.handlers.has("menu:action")).toBe(true));

    act(() => {
      eventMocks.handlers.get("menu:action")?.({ payload: { action: "menu-about" } });
    });

    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy());
    expect(useUi.getState().settingsTab).toBe("about");
    await waitFor(() => expect(activeNavTabText()).toBe("关于"));
    expect(document.querySelector(".about-logo")).toBeTruthy();
  });
});

describe("设置页：导航 ARIA 语义与非法页 key 收口", () => {
  it("tablist 内只含 tab（组标题标 presentation）、每个 tab 指向页体、页体是 tabpanel", async () => {
    await mountWithSession();
    await openSettings();

    const list = document.querySelector('[data-testid="settings-page"] .settings-nav-list') as HTMLElement;
    expect(list.getAttribute("role")).toBe("tablist");
    expect(list.getAttribute("aria-orientation")).toBe("vertical");
    // tablist 的直接子节点里除 tab 外只允许 role=presentation 的分组标题
    const stray = Array.from(list.children).filter(
      (el) => el.getAttribute("role") !== "tab" && el.getAttribute("role") !== "presentation",
    );
    expect(stray.map((el) => el.className)).toEqual([]);
    expect(
      Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-group')).every(
        (g) => g.getAttribute("role") === "presentation",
      ),
    ).toBe(true);

    const tabs = Array.from(list.querySelectorAll<HTMLElement>('[role="tab"]'));
    expect(tabs.length).toBe(10);
    const panel = document.querySelector('[data-testid="settings-page"] .settings-pane-body') as HTMLElement;
    expect(panel.getAttribute("role")).toBe("tabpanel");
    expect(panel.id).toBe("settings-panel");
    for (const t of tabs) {
      expect(t.getAttribute("aria-controls"), `${t.getAttribute("data-page")} 未指向页体`).toBe("settings-panel");
      expect(document.getElementById(t.id), `${t.id} 不存在（aria-labelledby 无法闭环）`).toBeTruthy();
    }
    // 页体由当前选中 tab 标注（tab ↔ tabpanel 双向闭环）
    const selected = tabs.filter((t) => t.getAttribute("aria-selected") === "true");
    expect(selected.length).toBe(1);
    expect(panel.getAttribute("aria-labelledby")).toBe(selected[0].id);
    expect(selected[0].getAttribute("data-page")).toBe("appearance");
  });

  it("方向键在页行间移动焦点（只移焦点、不换页，Enter/Space 或点击才激活）", async () => {
    await mountWithSession();
    await openSettings();

    const first = navItem("appearance") as HTMLElement;
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect((document.activeElement as HTMLElement).getAttribute("data-page")).toBe("providers");
    // 焦点移动不激活：仍停在默认页（避免方向键把「未保存改动」的三选弹框意外带出来）
    expect(activeNavTabText()).toBe("界面");

    fireEvent.keyDown(document.activeElement as HTMLElement, { key: "ArrowUp" });
    expect((document.activeElement as HTMLElement).getAttribute("data-page")).toBe("appearance");

    // 两端不越界（首尾停住）
    const last = navItem("about") as HTMLElement;
    last.focus();
    fireEvent.keyDown(last, { key: "ArrowDown" });
    expect((document.activeElement as HTMLElement).getAttribute("data-page")).toBe("about");
  });

  it("非法 settingsTab（绕过 setSettingsTab 直写）：渲染处归一，导航仍高亮默认页而非全不亮", async () => {
    await mountWithSession();
    await openSettings();

    act(() => {
      useUi.setState({ settingsTab: "nope" as unknown as PageKey });
    });

    await waitFor(() => expect(activeNavTabText()).toBe("界面"));
    const selected = Array.from(document.querySelectorAll('[data-testid="settings-page"] .settings-nav-item')).filter(
      (el) => el.getAttribute("aria-selected") === "true",
    );
    expect(selected.length).toBe(1);
    // 页体也是默认页（不空白）
    expect(document.querySelector('[data-testid="settings-page"] .settings-pane-body')?.textContent ?? "").toContain("主题");
  });
});

describe("设置页：可保存字段的脏点往返（PAGE_FIELDS 逐字段守护）", () => {
  /** 改一下 → 该页恰好 1 枚脏点（其他页 0 枚）；改回原值 → 全灭 */
  async function roundTrip(page: string, mutate: () => void, revert: () => void) {
    mutate();
    await waitFor(() => expect(navDot(page), `${page} 页未亮脏点`).toBe(true));
    expect(navDotCount()).toBe(1);
    revert();
    await waitFor(() => expect(navDotCount()).toBe(0));
  }

  it("安全与审批：4 个开关各自「改→亮、改回→灭」", async () => {
    await mountWithSession();
    await openPage("安全与审批");
    // 白名单为空时不渲染白名单区块 → 本页恰好 4 个可保存开关
    // （页体顶部还有页级「显示进阶项」开关（批③），它不在 Form.Item 内，故用 .ant-form-item 收窄）
    const switches = () => Array.from(document.querySelectorAll<HTMLElement>('[data-testid="settings-page"] .ant-form-item .ant-switch'));
    expect(switches().length).toBe(4);
    for (const i of [0, 1, 2, 3]) {
      await roundTrip("security", () => fireEvent.click(switches()[i]), () => fireEvent.click(switches()[i]));
    }
  });

  it("网络与连接：允许访问内网地址开关往返", async () => {
    await mountWithSession();
    await openPage("网络与连接");
    // 代理模式三张卡片是 Radio，本页唯一的 Switch 就是内网访问
    const sw = () => document.querySelector('[data-testid="settings-page"] .settings-pane-body .ant-switch') as HTMLElement;
    expect(sw()).toBeTruthy();
    await roundTrip("network", () => fireEvent.click(sw()), () => fireEvent.click(sw()));
  });

  it("日志：日志级别 Select 往返", async () => {
    await mountWithSession();
    await openPage("日志");
    const pickLevel = async (value: string) => {
      fireEvent.mouseDown(controlByLabel("日志级别").querySelector(".ant-select") as HTMLElement);
      await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy(), { timeout: 3000 });
      const option = Array.from(document.querySelectorAll(".ant-select-item-option")).find(
        (o) => (o.getAttribute("title") ?? o.textContent ?? "") === value,
      ) as HTMLElement;
      expect(option, `日志级别选项缺失：${value}`).toBeTruthy();
      fireEvent.click(option);
    };
    await pickLevel("debug");
    await waitFor(() => expect(navDot("logs")).toBe(true));
    expect(navDotCount()).toBe(1);
    await pickLevel("info");
    await waitFor(() => expect(navDotCount()).toBe(0));
  });

  it("工作区与智能体：压缩阈值（Slider）与压缩超时（InputNumber）往返", async () => {
    await mountWithSession();
    await openPage("工作区与智能体");
    // Slider 用键盘走一步（±0.05，不依赖布局度量）：rc-slider 读的是 e.which / e.keyCode（不认 e.key），
    // 故必须显式给键码 RIGHT=39 / LEFT=37
    const handle = () => document.querySelector('[data-testid="settings-page"] .ant-slider-handle') as HTMLElement;
    expect(handle()).toBeTruthy();
    await roundTrip(
      "agent",
      () => fireEvent.keyDown(handle(), { key: "ArrowRight", keyCode: 39, which: 39 }),
      () => fireEvent.keyDown(handle(), { key: "ArrowLeft", keyCode: 37, which: 37 }),
    );
    // InputNumber：240 → 回原值 180
    const timeoutInput = () => controlByLabel("压缩请求超时").querySelector("input") as HTMLInputElement;
    await roundTrip(
      "agent",
      () => fireEvent.change(timeoutInput(), { target: { value: "240" } }),
      () => fireEvent.change(timeoutInput(), { target: { value: "180" } }),
    );
  });

  it("工作区与智能体：Shell 选择（Select）往返", async () => {
    await mountWithSession();
    // 探测列表注入一个可选 shell（默认实现返回空列表 → 只有「自动」一项，无从往返）
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "list_available_shells") {
        return [{ id: "git-bash", name: "Git Bash", path: "C:/Program Files/Git/bin/bash.exe", kind: "bash", limited: false, auto: true }];
      }
      return baseInvoke(cmd, args);
    });
    await openPage("工作区与智能体");

    // 选项文案精确匹配：「自动（默认：Git Bash）」也含 "Git Bash"，用 includes 会误点自动项（往返即失败）
    const pickShell = async (match: (text: string) => boolean) => {
      fireEvent.mouseDown(controlByLabel("Shell").querySelector(".ant-select") as HTMLElement);
      await waitFor(() => expect(document.querySelector(".ant-select-item-option")).toBeTruthy(), { timeout: 3000 });
      const option = Array.from(document.querySelectorAll(".ant-select-item-option")).find((o) =>
        match((o.textContent ?? "").trim()),
      ) as HTMLElement;
      expect(option, "Shell 选项缺失").toBeTruthy();
      fireEvent.click(option);
    };
    await pickShell((text) => text === "Git Bash");
    await waitFor(() => expect(navDot("agent")).toBe(true));
    expect(navDotCount()).toBe(1);
    await pickShell((text) => text.startsWith("自动"));
    await waitFor(() => expect(navDotCount()).toBe(0));
  });

  it("写入后检查与校验：写入后检查开关与命令往返", async () => {
    await mountWithSession();
    await openPage("写入后检查与校验");

    // 开关：点开 → 亮；点回 → 灭
    const sw = () =>
      document.querySelector('[data-setting-id="post_write_check.enabled"] .ant-switch') as HTMLElement;
    expect(sw(), "写入后检查开关缺失").toBeTruthy();
    await roundTrip("tools", () => fireEvent.click(sw()), () => fireEvent.click(sw()));

    // 命令：填命令 → 亮；清空回默认（空命令 = 未配置）→ 灭
    const cmd = () =>
      document.querySelector('[data-setting-id="post_write_check.command"] input') as HTMLInputElement;
    await roundTrip(
      "tools",
      () => fireEvent.change(cmd(), { target: { value: "npx eslint {file}" } }),
      () => fireEvent.change(cmd(), { target: { value: "" } }),
    );
  });
});

describe("设置页：搜索与进阶折叠（批③）", () => {
  /** 设置选项的锚点包裹层（data-setting-id = 注册表 id） */
  function anchor(id: string): HTMLElement | null {
    return document.querySelector<HTMLElement>(`[data-testid="settings-page"] [data-setting-id="${id}"]`);
  }

  /** 导航头部整行的搜索框 */
  function searchBox(): HTMLInputElement {
    const el = document.querySelector<HTMLInputElement>('[data-testid="settings-page"] .settings-search input');
    if (!el) throw new Error("搜索框缺失");
    return el;
  }

  /** 输入查询串并等结果列表就位（空串 / 仅空格不进入搜索态） */
  async function search(q: string) {
    fireEvent.change(searchBox(), { target: { value: q } });
    await waitFor(() => expect(!!document.querySelector(".settings-search-results")).toBe(q.trim() !== ""));
  }

  /** 结果行（独立类名 .settings-search-item：与 .settings-nav-item 分离） */
  function resultRows(): HTMLElement[] {
    return Array.from(document.querySelectorAll<HTMLElement>('[data-testid="settings-page"] .settings-search-item'));
  }

  /** 按显示名找结果行 */
  function resultRow(label: string): HTMLElement {
    const row = resultRows().find((r) => (r.textContent ?? "").includes(label));
    if (!row) throw new Error(`结果行缺失：${label}`);
    return row;
  }

  /** 播放 Esc（window 捕获链：先清空查询，清空后才回落「返回工作区」） */
  function pressEsc() {
    fireEvent.keyDown(window, { key: "Escape" });
  }

  it("搜索框整行位于「返回工作区」下方；打开设置后焦点仍在返回工作区（.settings-nav-back）", async () => {
    await mountWithSession();
    await openSettings();

    const head = document.querySelector('[data-testid="settings-page"] .settings-nav-head') as HTMLElement;
    const back = head.querySelector(".settings-nav-back") as HTMLElement;
    const box = head.querySelector(".settings-search") as HTMLElement;
    expect(back).toBeTruthy();
    expect(box).toBeTruthy();
    // 顺序：返回工作区 → （可选运行中指示）→ 搜索框；搜索框是最后一个子节点，整行由
    // .settings-search 的 flex-basis:100% 保证（无布局引擎，故样式契约另在注册表测试里断言）
    const order = Array.from(head.children);
    expect(order.indexOf(back)).toBeLessThan(order.indexOf(box));
    expect(head.lastElementChild).toBe(box);
    expect(searchBox().getAttribute("placeholder")).toBe("搜索设置项…");
    // 焦点不被搜索框抢走：搜索框挂载时值为空 → 清除 button 不存在，本断言只能验证「焦点落在返回工作区」，
    // 「泛选 button 会被清除键抢焦点」是防御性写法（当前不可构造验证）
    expect(document.activeElement).toBe(back);
  });

  it("搜索态替掉左导航 tablist：结果行独立类名、无默认选中、不自动跳页", async () => {
    await mountWithSession();
    await openSettings();
    await search("日志");

    expect(resultRows().length).toBe(3); // 日志级别 + 会话详细日志 + 关于页「日志目录」（批④ 新登记）
    // 两套列表互斥：搜索态不渲染 tablist（方向键因此不可能串味）
    expect(document.querySelector('[data-testid="settings-page"] .settings-nav-list')).toBeFalsy();
    expect(document.querySelector('[data-testid="settings-page"] [role="tablist"]')).toBeFalsy();
    const listbox = document.querySelector('[data-testid="settings-page"] .settings-search-results') as HTMLElement;
    expect(listbox.getAttribute("role")).toBe("listbox");
    expect(listbox.querySelectorAll('[role="option"]').length).toBe(resultRows().length);
    // 无默认选中：所有 option 的 aria-selected 都是 false
    expect(resultRows().every((r) => r.getAttribute("aria-selected") === "false")).toBe(true);
    expect(document.querySelectorAll('[data-testid="settings-page"] .settings-search-item-active').length).toBe(0);
    // 不自动跳页
    expect(useUi.getState().settingsTab).toBe("appearance");
    // 结果行 = 显示名 + 所属页名（次标）
    const row = resultRow("日志级别");
    expect(row.querySelector(".settings-search-item-label")?.textContent).toBe("日志级别");
    expect(row.querySelector(".settings-search-item-page")?.textContent).toBe("日志");
  });

  it("↑/↓ 只动结果列表（两端停住），未选中时 Enter 不动作，焦点始终留在搜索框", async () => {
    await mountWithSession();
    await openPage("日志");
    await search("详细");
    expect(resultRows().length).toBe(1);

    const input = searchBox();
    input.focus();
    // 未选中（-1）时 Enter 不动作
    fireEvent.keyDown(input, { key: "Enter" });
    await new Promise((r) => setTimeout(r, 60));
    expect(document.querySelector(".settings-item-hit")).toBeFalsy();

    // ↑ 从 -1 起停在第 0 项（不循环、不回绕）
    fireEvent.keyDown(input, { key: "ArrowUp" });
    expect(resultRows()[0].getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(input);
    // 已是末项：再 ↓ 两次仍停在第 0 项
    fireEvent.keyDown(input, { key: "ArrowDown" });
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(resultRows()[0].getAttribute("aria-selected")).toBe("true");
    expect(resultRows().length).toBe(1);
    expect(document.activeElement).toBe(input);

    // Enter 跳转后焦点仍在搜索框
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(anchor("log.session_verbose")?.classList.contains("settings-item-hit")).toBe(true));
    expect(document.activeElement).toBe(input);
    expect(useUi.getState().settingsTab).toBe("logs");
  });

  it("批④ 登记的关于页只读入口可被搜索命中（版本 / 数据目录 / 日志目录 / 代码仓库 / 许可证）", async () => {
    await mountWithSession();
    await openSettings();

    // 登记前关于页只能命中 2 项（自动更新 / 检查更新）；登记后五个只读条目各自可搜到，
    // 且命中后能切到关于页并给对应锚点打临时高亮（= 页内真有那一行）
    for (const [query, label, id] of [
      ["版本", "版本", "app.version"],
      ["数据目录", "数据目录", "app.data_dir"],
      ["日志目录", "日志目录", "app.logs_dir"],
      ["代码仓库", "代码仓库", "app.repo"],
      ["许可证", "开源许可证", "app.license"],
    ] as const) {
      await search(query);
      fireEvent.click(resultRow(label));
      await waitFor(() => expect(useUi.getState().settingsTab).toBe("about"));
      await waitFor(() => expect(anchor(id)?.classList.contains("settings-item-hit")).toBe(true));
    }
  });

  it("跨页命中：切到目标页并给目标项打临时高亮", async () => {
    await mountWithSession();
    await openSettings();

    await search("代理");
    fireEvent.click(resultRow("代理模式"));
    await waitFor(() => expect(useUi.getState().settingsTab).toBe("network"));
    await waitFor(() => expect(anchor("network.proxy")?.classList.contains("settings-item-hit")).toBe(true));
    // 查询串保留（可继续换结果下的另选）
    expect(searchBox().value).toBe("代理");
  });

  it("命中当前页：只定位高亮，不切页、不改导航选中态", async () => {
    await mountWithSession();
    await openPage("日志");

    await search("日志级别");
    fireEvent.click(resultRows()[0]);
    await waitFor(() => expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(true));
    // 页没换：store 的 settingsTab 与导航选中态都停在日志页
    expect(useUi.getState().settingsTab).toBe("logs");
    expect(activeNavTabText()).toBe(""); // 搜索态下页体/导航 tablist 不渲染 → 此处只看 store
    pressEsc();
    await waitFor(() => expect(activeNavTabText()).toBe("日志"));
  });

  it("临时高亮约 1.5s 后自动摘掉（不永驻）", async () => {
    await mountWithSession();
    await openPage("日志");
    await search("日志级别");
    fireEvent.click(resultRows()[0]);
    await waitFor(() => expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(true));
    await waitFor(() => expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(false), {
      timeout: 3000,
    });
  });

  it("无命中：空态文案 + 动态条目引导（供应商/模型、MCP 服务器、技能在各自页面内查找）", async () => {
    await mountWithSession();
    await openSettings();
    await search("zzzzzz");

    expect(resultRows().length).toBe(0);
    const empty = document.querySelector('[data-testid="settings-page"] .settings-search-empty') as HTMLElement;
    expect(empty).toBeTruthy();
    const text = empty.textContent ?? "";
    expect(text).toContain("没有匹配的设置项");
    expect(text).toContain("供应商 / 模型、MCP 服务器、技能等条目请在各自页面内查找");
  });

  it("Esc 第一次只清空查询（恢复导航），第二次才回落既有「返回工作区」链", async () => {
    await mountWithSession();
    await openSettings();
    await search("代理");
    expect(document.querySelector(".settings-search-results")).toBeTruthy();

    pressEsc();
    await waitFor(() => expect(document.querySelector(".settings-search-results")).toBeFalsy());
    expect(searchBox().value).toBe("");
    expect(document.querySelector('[data-testid="settings-page"] [role="tablist"]')).toBeTruthy();
    expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy(); // 未离开设置页

    pressEsc();
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
    expect(cancelRunLog).toEqual([]);
  });

  // 2026-09 移除「显示进阶项」开关，原三个进阶折叠相关用例（默认收起/偏好落 localStorage、
  // 命中被折叠的进阶项临时展开、进阶项改动亮脏点）随之删除。

  it("搜索框 ARIA：combobox 语义与 aria-activedescendant 都挂在输入框上，listbox 只作投影", async () => {
    await mountWithSession();
    await openSettings();

    const input = searchBox();
    expect(input.getAttribute("role")).toBe("combobox");
    expect(input.getAttribute("aria-label")).toBe("搜索设置项…");
    expect(input.getAttribute("aria-expanded")).toBe("false"); // 非搜索态：无结果列表
    expect(input.getAttribute("aria-activedescendant")).toBeNull();

    await search("日志");
    const listbox = document.querySelector('[data-testid="settings-page"] .settings-search-results') as HTMLElement;
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(input.getAttribute("aria-controls")).toBe(listbox.id);
    // 容器不是焦点目标，也不承载 aria-activedescendant（挂在无焦点的 listbox 上读屏不会播报）
    expect(listbox.getAttribute("role")).toBe("listbox");
    expect(listbox.getAttribute("tabindex")).toBe("-1");
    expect(listbox.getAttribute("aria-activedescendant")).toBeNull();

    fireEvent.keyDown(input, { key: "ArrowDown" });
    const active = resultRows().find((r) => r.getAttribute("aria-selected") === "true");
    expect(active).toBeTruthy();
    expect(active!.id).toContain("settings-search-opt-");
    expect(input.getAttribute("aria-activedescendant")).toBe(active!.id);
  });

  it("同页连续命中不同项：旧高亮先被摘掉（任一时刻只有一处 .settings-item-hit）", async () => {
    await mountWithSession();
    await openPage("日志");
    await search("日志");

    fireEvent.click(resultRow("日志级别"));
    await waitFor(() => expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(true));

    // 1.5s 内再命中另一项：旧定时器被 clearTimeout 后，旧元素上的类只能由新一次命中去摘
    fireEvent.click(resultRow("会话详细日志"));
    await waitFor(() => expect(anchor("log.session_verbose")?.classList.contains("settings-item-hit")).toBe(true));
    expect(document.querySelectorAll(".settings-item-hit").length).toBe(1);
    expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(false);
  });

  it("0 高度锚点退化：命中 approval.command_allowlist（白名单为空 → 锚点无高度）时高亮页体容器", async () => {
    await mountWithSession();
    // 该锚点只在安全与审批页的页体里（每次只渲染当前页）→ 先上页再搜，命中项即在当前页
    await openPage("安全与审批");
    await search("白名单");

    const anchorEl = anchor("approval.command_allowlist")!;
    expect(anchorEl).toBeTruthy();
    // happy-dom 无布局引擎：手工把该锚点伪装成真实浏览器里「白名单为空」时的 0 高度盒
    Object.defineProperty(anchorEl, "offsetHeight", { value: 0, configurable: true });
    Object.defineProperty(anchorEl, "getClientRects", { value: () => [], configurable: true });

    fireEvent.click(resultRow("命令白名单"));
    await waitFor(() =>
      expect(document.querySelector(".settings-pane-body")?.classList.contains("settings-item-hit")).toBe(true),
    );
    expect(anchorEl.classList.contains("settings-item-hit")).toBe(false);
  });

  describe("命中跳转 × 三选拦截（三条离开路径的定位归属）", () => {
    /** 网络页代理模式卡片（三张 Radio 卡片，按标题找） */
    function proxyCard(title: string): HTMLElement {
      const card = Array.from(
        document.querySelectorAll<HTMLElement>('[data-testid="settings-page"] .proxy-mode-card'),
      ).find((el) => (el.querySelector(".proxy-mode-title")?.textContent ?? "") === title);
      if (!card) throw new Error(`代理模式卡片缺失：${title}`);
      return card;
    }

    it("① 留在原地：本次定位被丢弃（之后手动切到目标页也不高亮）", async () => {
      await mountWithSession();
      await openSettings();
      makeDirty(); // 工作区与智能体页的自定义提示词 → 有未保存改动

      await search("日志级别");
      fireEvent.click(resultRow("日志级别"));
      // 命中跳转走与点击导航同一条 onTabChange：脏改动存在 → 先走三选
      await waitFor(() => expect(confirmPending()).toBe(true));
      fireEvent.click(buttonByText("留在原地"));
      await waitFor(() => expect(confirmPending()).toBe(false));
      expect(useUi.getState().settingsTab).toBe("agent");

      // 之后手动切到目标页（仍有脏改动 → 再走一次三选，这次放弃改动放行）也不该高亮
      pressEsc();
      await waitFor(() => expect(document.querySelector(".settings-search-results")).toBeFalsy());
      clickNavTab("日志");
      await waitFor(() => expect(confirmPending()).toBe(true));
      fireEvent.click(buttonByText("放弃改动"));
      // 等页体与导航真的渲染完（只读 store 会比渲染早，会掩盖定位是否真的发生）
      await waitFor(() => expect(activeNavTabText()).toBe("日志"));
      await new Promise((r) => setTimeout(r, 100));
      expect(document.querySelector(".settings-item-hit")).toBeFalsy();
    });

    it("② 保存成功：落盘后跳页并照常定位高亮", async () => {
      await mountWithSession();
      await openSettings();
      makeDirty();

      await search("日志级别");
      fireEvent.click(resultRow("日志级别"));
      await waitFor(() => expect(confirmPending()).toBe(true));
      fireEvent.click(buttonByText("保存并离开"));

      await waitFor(() => expect(useUi.getState().settingsTab).toBe("logs"));
      await waitFor(() => expect(anchor("log.level")?.classList.contains("settings-item-hit")).toBe(true));
    });

    it("③ 保存失败（代理地址非法）：定位被丢弃，手动切到目标页也不高亮", async () => {
      await mountWithSession();
      await openPage("网络与连接");
      // 自定义代理 + 非法地址：save() 校验失败 → 报错跳回网络页、不落盘
      fireEvent.click(proxyCard("自定义代理").querySelector(".ant-radio-input") as HTMLElement);
      const urlInput = await waitFor(() => {
        const el = controlByLabel("代理地址").querySelector("input") as HTMLInputElement | null;
        expect(el).toBeTruthy();
        return el!;
      });
      fireEvent.change(urlInput, { target: { value: "ftp://127.0.0.1:7890" } });
      await waitFor(() => expect(navDot("network")).toBe(true));

      await search("日志级别");
      fireEvent.click(resultRow("日志级别"));
      await waitFor(() => expect(confirmPending()).toBe(true));
      fireEvent.click(buttonByText("保存并离开"));
      // 保存被拦：留在网络页（离开动作不执行），弹框关闭
      await waitFor(() => expect(confirmPending()).toBe(false));
      expect(useUi.getState().settingsTab).toBe("network");

      // 之后切到目标页也不该高亮：定位已随保存失败一并丢弃。
      // 这里**直接经 store 切页**（act + useUi.setState），故意不走 onTabChange（走导航点击会再弹三选）。
      // 已知限度（审查返工实测，未最终定位）：把 save() 保存失败分支里的 setPendingHit(null) 删掉后，
      // 本用例**仍绿** —— 疑因保存失败后弹框关闭会再走一次「留在原地」路径（那条也清 pendingHit），
      // 使该缺陷不可观测。即本条属「行为正确但变异不可分辨」的守卫，已写入 PR 说明。
      pressEsc();
      await waitFor(() => expect(document.querySelector(".settings-search-results")).toBeFalsy());
      act(() => {
        useUi.setState({ settingsTab: "logs" });
      });
      await waitFor(() => expect(activeNavTabText()).toBe("日志"));
      // 等页体渲染完 + 给两段式定位的 effect 一次机会，再断言没有高亮
      await new Promise((r) => setTimeout(r, 100));
      expect(document.querySelector(".settings-item-hit")).toBeFalsy();
    });
  });

  it("锚点覆盖：每项在其所属页都有 data-setting-id（例外：只在编辑供应商视图出现的 active_model_id）", async () => {
    await mountWithSession();
    await openSettings();

    // 例外分三类，均已在 [docs/settings-search-and-advanced] §1.6 登记：
    //  ① 注册表项**当前视图没有锚点**：`active_model_id` 的「当前」标记只在「编辑供应商」视图的模型列表里
    //    （列表视图无此节点）→ 搜索命中该项时退化为「切页 + 高亮页体容器」；
    //  ② **动态行级锚点**（`providers.<uuid>`）：供应商行不在注册表里（数量与 id 随配置变），
    //    故不进本清单，但「有行就必须有锚点」另行断言（见下方 + 外部跳转用例组）。
    //  ③ 注册表项**依赖数据才存在**：`app.mcp_status`（服务器状态表）在「一个服务器都没配置」时整段
    //    不渲染（不显示零信息量的空表）→ 搜索命中它时退化为「切页 + 高亮页体容器」，与 ① 同类；
    //    带配置时的锚点断言见 MCP 状态表用例组。
    const EXCEPTIONS = ["active_model_id", "app.mcp_status"];
    const missing: string[] = [];
    for (const page of PAGE_ORDER) {
      fireEvent.click(navItem(page) as HTMLElement);
      await waitFor(() => expect(useUi.getState().settingsTab).toBe(page));
      for (const item of SETTINGS_ITEMS.filter((i) => i.page === page)) {
        if (EXCEPTIONS.includes(item.id)) continue;
        if (!anchor(item.id)) missing.push(`${page}/${item.id}`);
      }
    }
    expect(missing, `缺锚点：${missing.join("、")}`).toEqual([]);

    // 动态条目（供应商行）不在注册表里，故不在上面逐项核对之列：行级锚点 `providers.<uuid>` 随配置增删，
    // 这里按 fixture 的供应商断言「有行必有锚点」（它是外部跳转的落点，缺了就退化成高亮页体容器）
    fireEvent.click(navItem("providers") as HTMLElement);
    await waitFor(() => expect(useUi.getState().settingsTab).toBe("providers"));
    await waitFor(() => expect(anchor("providers.p1")).toBeTruthy());
  });
});

// ---------- 外部跳转命中动态行级锚点（额度灰行的「去设置」） ----------
// 供应商行是**动态条目**（数量与 id 都由配置决定），不在注册表里，所以外部跳转单开一条轻量锚点通道
// `providers.<uuid>`。本组守护三件事：设置页被打开且停在目标页（不自动进编辑视图）、命中类落在**行**
// 而不是页体容器 / 视图根节点、连点两次仍重新定位。真实滚动位置（scrollIntoView）交手动验证：
// happy-dom 无布局引擎，只能断言类名落在正确节点。
describe("设置页：外部命中动态行级锚点（settingsHit）", () => {
  /** 供应商行的行级锚点（fixture 的供应商 id = p1） */
  function rowAnchor(id: string): HTMLElement | null {
    return document.querySelector<HTMLElement>(`[data-testid="settings-page"] [data-setting-id="providers.${id}"]`);
  }

  /** 外部入口（额度灰行「去设置」）的等价调用：打开设置 + 切页 + 置锚点 */
  async function jumpFromOutside(anchorId: string) {
    await act(async () => {
      useUi.getState().showSettingsAt("providers", anchorId);
    });
  }

  it("打开设置页并停在模型与供应商页，且不自动进入编辑视图", async () => {
    await mountWithSession();
    expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy();

    await jumpFromOutside("providers.p1");

    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeTruthy());
    expect(useUi.getState().settingsTab).toBe("providers");
    // 导航选中态跟上（页体渲染的判定靠它）
    await waitFor(() => expect(activeNavTabText()).toBe("模型与供应商"));
    // 停在**列表**视图：编辑视图里的「供应商名称」输入框（值 = fixture 的供应商名）不该出现。
    // 注意不能用「编辑供应商」文本判定——列表视图每行的操作按钮也叫这个名字。
    await waitFor(() => expect(rowAnchor("p1")).toBeTruthy());
    const providerNameInputs = Array.from(
      document.querySelectorAll<HTMLInputElement>('[data-testid="settings-page"] input'),
    ).filter((i) => i.value === "Test Provider");
    expect(providerNameInputs.length).toBe(0);
  });

  it("设置页已打开时跳转同样生效（切页 + 定位，不重开设置页）", async () => {
    await mountWithSession();
    await openSettings(); // 默认落在界面页
    expect(activeNavTabText()).toBe("界面");

    await jumpFromOutside("providers.p1");

    await waitFor(() => expect(activeNavTabText()).toBe("模型与供应商"));
    await waitFor(() => expect(rowAnchor("p1")?.classList.contains("settings-item-hit")).toBe(true));
    expect(useUi.getState().settingsOpen).toBe(true);
  });

  it("行级锚点落在供应商行：命中类在该行，不退化到页体容器 / 视图根节点", async () => {
    await mountWithSession();
    await jumpFromOutside("providers.p1");

    await waitFor(() => expect(rowAnchor("p1")?.classList.contains("settings-item-hit")).toBe(true));
    // 该行确实是供应商行（不是占位节点）
    expect(rowAnchor("p1")?.textContent).toContain("Test Provider");
    // 行级锚点与三个视图根节点的 `providers` 是两个节点：属性选择器等值匹配，不会互撞
    const viewRoot = document.querySelector<HTMLElement>('[data-testid="settings-page"] [data-setting-id="providers"]');
    expect(viewRoot).toBeTruthy();
    expect(viewRoot!.contains(rowAnchor("p1"))).toBe(true);
    expect(viewRoot!.classList.contains("settings-item-hit")).toBe(false);
    // 退化保护：命中页体容器 = 用户观感「点了没反应」
    expect(document.querySelector(".settings-pane-body")?.classList.contains("settings-item-hit")).toBe(false);
    // 请求是一次性的：消费后 store 里不再留值（否则关掉设置再打开会莫名重放一次定位）
    expect(useUi.getState().settingsHit).toBeNull();
  });

  it("连点两次同一行（第二次是新请求）仍重新定位", async () => {
    await mountWithSession();
    await jumpFromOutside("providers.p1");
    await waitFor(() => expect(rowAnchor("p1")?.classList.contains("settings-item-hit")).toBe(true));

    // 模拟 1.5s 到点自动摘除（不必真等：类与定时器都是命令式的），再点一次同一行
    act(() => {
      rowAnchor("p1")!.classList.remove("settings-item-hit");
    });
    await jumpFromOutside("providers.p1");
    await waitFor(() => expect(rowAnchor("p1")?.classList.contains("settings-item-hit")).toBe(true));
  });
});

// ---------- 会话保留期与清理（[docs/session-cleanup](../../../docs/session-cleanup.md) §3 第 12/13/25/26/27 条） ----------
// 两条清理路径（手动「立即清理」与保存时自动清理）都必须：只认已保存的保留期、删前有预览与确认、
// 删后用返回的 id 收尾 Tab（关闭数回显在提示里）、并刷新「上次清理」只读行。
describe("设置页：会话保留期与清理", () => {
  /** 清理命令的调用记录（每个用例开头的 mockCleanupIpc 会先复位）；status = 读「上次清理」的次数 */
  const calls = { preview: [] as (number | null)[], run: 0, save: [] as any[], status: 0 };

  /** 带保留期的配置（fixtureConfig 没有 sessions 段 = 旧配置同形，这里显式补上） */
  function configWithRetention(days: number | null) {
    return { ...fixtureConfig, sessions: { retention_days: days } };
  }

  /**
   * 清理链路的 IPC mock：get_config 带保留期（**必须在 mountWithSession 之前装上**：
   *  App 启动时就拉配置）；预览 / 执行两个命令按用例给值；
   * save_config 记录入参并模拟后端语义（本次跳过清理 → 返回 null，否则返回配置里的清理结果）；
   * 「上次清理」状态只在真的清理过之后才回记录（跑之前的空态也是只读行契约的一部分）。
   */
  async function mockCleanupIpc(opts: {
    retention: number | null;
    preview?: { count: number; titles: string[]; orphan_count?: number };
    outcome?: { ids: string[]; deleted: number; failed: number };
    status?: { last_run_at: string | null; last_deleted: number; last_failed: number };
    /** true = 一撕开就读到清理记录（模拟「启动时已清理过」）；默认只在本页真的清理后才回记录 */
    statusAtMount?: boolean;
  }) {
    const emptyStatus = { last_run_at: null, last_deleted: 0, last_failed: 0 };
    calls.preview.length = 0;
    calls.save.length = 0;
    calls.run = 0;
    calls.status = 0;
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "get_config") return configWithRetention(opts.retention);
      if (cmd === "preview_session_cleanup") {
        calls.preview.push(args?.days ?? null);
        return opts.preview ?? { count: 0, titles: [], orphan_count: 0 };
      }
      if (cmd === "run_session_cleanup") {
        calls.run += 1;
        return opts.outcome ?? { ids: [], deleted: 0, failed: 0 };
      }
      if (cmd === "get_cleanup_status") {
        calls.status += 1;
        const cleaned = opts.statusAtMount || calls.run > 0 || calls.save.some((a) => !a?.skipCleanup);
        return cleaned ? opts.status ?? emptyStatus : emptyStatus;
      }
      if (cmd === "save_config") {
        calls.save.push(args);
        return args?.skipCleanup ? null : opts.outcome ?? null;
      }
      return baseInvoke(cmd, args);
    });
  }

  /** 保留期下拉（按 Form.Item 标签定位，避免抓到本页的 Shell 下拉） */
  function retentionSelect(): HTMLElement {
    return controlByLabel("会话保留期").querySelector(".ant-select") as HTMLElement;
  }

  /** 当前下拉里的档位文本（含已收起但仍在 DOM 里的下拉） */
  function retentionOptionTexts(): string[] {
    return Array.from(document.querySelectorAll(".ant-select-item-option")).map((o) => (o.textContent ?? "").trim());
  }

  /** 选一个档位：展开（首次会等下拉渲染）→ 按文本点选项（选完自动收起，下次 mouseDown 又是「展开」） */
  async function pickRetention(text: string) {
    fireEvent.mouseDown(retentionSelect());
    const option = await waitFor(() => {
      const el = Array.from(document.querySelectorAll(".ant-select-item-option")).find(
        (o) => (o.textContent ?? "").trim() === text,
      ) as HTMLElement | undefined;
      expect(el, `保留期选项缺失：${text}`).toBeTruthy();
      return el!;
    }, { timeout: 3000 });
    fireEvent.click(option);
  }

  /** 「上次清理」只读行（注册表锚点；常驻渲染） */
  function cleanupStatusRow(): HTMLElement {
    return document.querySelector('[data-setting-id="app.cleanup_status"]') as HTMLElement;
  }

  it("保留期下拉：默认「不清理」、六个档位齐备；选中后亮该页脏点、选回不清理即熄灭", async () => {
    await mockCleanupIpc({ retention: null });
    await mountWithSession();
    await openPage("工作区与智能体");

    expect(retentionSelect().textContent).toContain("不清理");
    // 一次展开既断言档位又顺手选中「7 天」：之后 mouseDown 就是「展开」而不是「收起」
    fireEvent.mouseDown(retentionSelect());
    await waitFor(() => expect(document.querySelector(".ant-select-item-option")).toBeTruthy(), { timeout: 3000 });
    expect(retentionOptionTexts()).toEqual(["不清理", "1 天", "3 天", "7 天", "14 天", "30 天"]);
    fireEvent.click(
      Array.from(document.querySelectorAll(".ant-select-item-option")).find((o) => (o.textContent ?? "").trim() === "7 天") as HTMLElement,
    );
    await waitFor(() => expect(navDot("agent")).toBe(true));
    expect(navDotCount()).toBe(1);

    await pickRetention("不清理");
    await waitFor(() => expect(navDotCount()).toBe(0));
  });

  it("「立即清理」禁用条件①：保留期为「不清理」时禁用，并注明先选择保留期", async () => {
    await mockCleanupIpc({ retention: null });
    await mountWithSession();
    await openPage("工作区与智能体");

    expect(buttonByText("立即清理").disabled).toBe(true);
    expect(controlByLabel("立即清理").textContent).toContain("先选择保留期");
    expect(calls.preview).toEqual([]); // 禁用态不可能发出预览
  });

  it("「立即清理」禁用条件②：保留期有未保存改动时禁用，并提示保存", async () => {
    await mockCleanupIpc({ retention: 7 });
    await mountWithSession();
    await openPage("工作区与智能体");

    expect(buttonByText("立即清理").disabled).toBe(false);
    await pickRetention("30 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));
    expect(buttonByText("立即清理").disabled).toBe(true);
    expect(controlByLabel("立即清理").textContent).toContain("有未保存的改动，先保存");

    // 改回已保存值 → 恢复可点
    await pickRetention("7 天");
    await waitFor(() => expect(navDotCount()).toBe(0));
    expect(buttonByText("立即清理").disabled).toBe(false);
  });

  it("保存前确认框（确认）：预览有会话 → 确认后正常保存（不带 skipCleanup），并按返回结果关 Tab 与提示", async () => {
    await mockCleanupIpc({
      retention: null,
      preview: { count: 2, titles: ["三天前的会话", "十天前的会话"] },
      outcome: { ids: ["new-1"], deleted: 1, failed: 0 },
      status: { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 1, last_failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");
    await pickRetention("7 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));
    expect(document.body.textContent ?? "").toContain("将删除 2 个会话");
    expect(document.body.textContent ?? "").toContain("三天前的会话");
    expect(calls.preview).toEqual([7]); // 预览用的是草稿里的新保留期

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.save.length).toBe(1));
    expect(calls.save[0].skipCleanup).toBe(false);
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已清理 1 个会话，关闭 1 个标签页"));
    await waitFor(() => expect(cleanupStatusRow().textContent ?? "").toContain("删除 1 个会话"));
  });

  it("保存前确认框（取消）：配置照常保存但带 skipCleanup: true，并提示本次不清理", async () => {
    await mockCleanupIpc({ retention: null, preview: { count: 3, titles: ["旧会话"] } });
    await mountWithSession();
    await openPage("工作区与智能体");
    await pickRetention("1 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));
    fireEvent.click(buttonByText("暂不清理（下次启动仍会清理）"));

    await waitFor(() => expect(calls.save.length).toBe(1));
    expect(calls.save[0].skipCleanup).toBe(true);
    expect(calls.run).toBe(0); // 取消 = 本次不执行清理命令
    await waitFor(() => expect(document.body.textContent ?? "").toContain("设置已保存，本次不清理"));
    // 保存本身成功：脏点清空
    await waitFor(() => expect(navDotCount()).toBe(0));
  });

  it("保留期未变时不弹确认框（不打扰）", async () => {
    await mockCleanupIpc({ retention: 7, preview: { count: 5, titles: ["旧会话"] } });
    await mountWithSession();
    await openPage("工作区与智能体");

    // 改别的字段（关闭压缩超时可改可不改，这里用自定义提示词）制造未保存改动，再保存
    fireEvent.change(promptArea(), { target: { value: "只用中文" } });
    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(calls.save.length).toBe(1));
    expect(calls.preview).toEqual([]); // 保留期没变 → 连预览都不发
    expect(document.body.textContent ?? "").not.toContain("保存前确认清理");
  });

  it("手动「立即清理」：预览 0 条只给轻提示，不弹确认框也不执行", async () => {
    await mockCleanupIpc({ retention: 7, preview: { count: 0, titles: [], orphan_count: 0 } });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("没有需要清理的会话"));
    expect(document.body.textContent ?? "").not.toContain("确认立即清理");
    expect(calls.preview).toEqual([7]); // 用**已保存**的保留期（当前页无未保存改动）
    expect(calls.run).toBe(0);
  });

  it("手动「立即清理」：确认后执行，关闭被删会话的 Tab 并刷新「上次清理」行", async () => {
    await mockCleanupIpc({
      retention: 7,
      preview: { count: 2, titles: ["三天前的会话", "十天前的会话"] },
      outcome: { ids: ["new-1"], deleted: 1, failed: 0 },
      status: { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 1, last_failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");
    expect(cleanupStatusRow().textContent ?? "").toContain("还没有清理记录");

    fireEvent.click(buttonByText("立即清理"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("确认立即清理？"));
    expect(document.body.textContent ?? "").toContain("三天前的会话");

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.run).toBe(1));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已清理 1 个会话，关闭 1 个标签页"));
    await waitFor(() => expect(cleanupStatusRow().textContent ?? "").toContain("删除 1 个会话"));
  });

  it("只有残留数据文件（count = 0、orphan_count > 0）：手动「立即清理」也要弹确认框，正文说的是残留文件而非删会话", async () => {
    await mockCleanupIpc({
      retention: 7,
      preview: { count: 0, titles: [], orphan_count: 3 },
      outcome: { ids: [], deleted: 0, failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理"));
    // 预览口径与执行口径对齐：会话 0 条、残留文件 3 个一样要问——只看会话数会把这一种清理静默跳过
    await waitFor(() => expect(document.body.textContent ?? "").toContain("确认立即清理？"));
    const dialog = document.body.textContent ?? "";
    expect(dialog).toContain("将清理 3 个残留数据文件");
    expect(dialog).not.toContain("将删除 0 个会话");
    expect(calls.preview).toEqual([7]);

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.run).toBe(1));
  });

  it("只有残留数据文件：保存前的确认框同样弹（两条路径同一口径），确认后照常保存", async () => {
    await mockCleanupIpc({
      retention: null,
      preview: { count: 0, titles: [], orphan_count: 2 },
      outcome: { ids: [], deleted: 0, failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");
    await pickRetention("7 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));
    expect(document.body.textContent ?? "").toContain("将清理 2 个残留数据文件");
    expect(calls.preview).toEqual([7]);

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.save.length).toBe(1));
    expect(calls.save[0].skipCleanup).toBe(false);
  });

  it("清理有失败条数：成功提示之外补一条含条数的警示，只读行同样记下失败数", async () => {
    await mockCleanupIpc({
      retention: 7,
      preview: { count: 2, titles: ["三天前的会话", "十天前的会话"] },
      outcome: { ids: ["new-1"], deleted: 1, failed: 2 },
      status: { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 1, last_failed: 2 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("确认立即清理？"));
    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.run).toBe(1));

    // 两条提示并存：成功（删了几条、关了几个标签页）+ 失败（几条没删掉）——
    // 只报成功会让用户以为全部清完了，没删掉的会话从此无人过问（披露不足）
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已清理 1 个会话，关闭 1 个标签页"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("2 个会话未能清理"));
    // 只读行的失败口径与即时警示一致
    await waitFor(() => expect(cleanupStatusRow().textContent ?? "").toContain("（2 个失败）"));
  });

  it("保存触发的清理只记了失败条数（deleted = 0）：同样报警示，不报「已清理 0 个会话」", async () => {
    await mockCleanupIpc({
      retention: null,
      preview: { count: 1, titles: ["旧会话"] },
      outcome: { ids: [], deleted: 0, failed: 2 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");
    await pickRetention("7 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));
    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.save.length).toBe(1));

    // 这一路径原本一条提示都没有（只刷新只读行）→ 用户看不到任何痕迹
    await waitFor(() => expect(document.body.textContent ?? "").toContain("2 个会话未能清理"));
    expect(document.body.textContent ?? "").not.toContain("已清理 0 个会话");
  });

  it("会话与残留数据文件都要删：确认框补一句残留条数，完成提示也带上同一个数", async () => {
    await mockCleanupIpc({
      retention: 7,
      preview: { count: 2, titles: ["三天前的会话"], orphan_count: 3 },
      outcome: { ids: ["new-1"], deleted: 1, failed: 0 },
      status: { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 1, last_failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("确认立即清理？"));
    // 会话那句照旧，另补残留文件那句；「只有残留文件」的专用说明不得在此时出现（会让人以为不删会话）
    expect(document.body.textContent ?? "").toContain("将删除 2 个会话");
    expect(document.body.textContent ?? "").toContain("另有 3 个残留数据文件");
    expect(document.body.textContent ?? "").not.toContain("将清理 3 个残留数据文件");

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.run).toBe(1));
    // 完成提示按整串断言（确认框关掉后其正文可能仍在 DOM 里，单断「另有 3 个…」分不清是谁说的）
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("已清理 1 个会话，关闭 1 个标签页（另有 3 个残留数据文件）"),
    );
  });

  it("保存前的确认框同样补残留条数：确认后完成提示带同一句（两条路径一份版式）", async () => {
    await mockCleanupIpc({
      retention: null,
      preview: { count: 1, titles: ["旧会话"], orphan_count: 2 },
      outcome: { ids: ["new-1"], deleted: 1, failed: 0 },
      status: { last_run_at: "2026-09-19T08:00:00Z", last_deleted: 1, last_failed: 0 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");
    await pickRetention("7 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));
    expect(document.body.textContent ?? "").toContain("另有 2 个残留数据文件");

    fireEvent.click(buttonByText("清理"));
    await waitFor(() => expect(calls.save.length).toBe(1));
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("已清理 1 个会话，关闭 1 个标签页（另有 2 个残留数据文件）"),
    );
  });

  it("看过「上次清理」结果就写下已提示记录：下次启动不再为同一件事提醒", async () => {
    const runAt = "2026-09-19T08:00:00Z";
    await mockCleanupIpc({
      retention: 7,
      status: { last_run_at: runAt, last_deleted: 1, last_failed: 0 },
      statusAtMount: true,
    });
    await mountWithSession();
    // AppShell 启动链路自己也读过一次这条记录（并把去重记录写了）：先等它读完、再把记录抹掉，
    // 后面断言的写入就只可能来自设置页（两道写入共用一把键，不隔离就分不清是谁写的）
    await waitFor(() => expect(calls.status).toBeGreaterThan(0));
    localStorage.removeItem(CLEANUP_NOTICE_SEEN_KEY);

    await openPage("工作区与智能体");
    await waitFor(() => expect(cleanupStatusRow().textContent ?? "").toContain("删除 1 个会话"));
    expect(localStorage.getItem(CLEANUP_NOTICE_SEEN_KEY)).toBe(runAt);
  });

  it("没有清理记录时不写「已提示」记录（没有可提示的内容）", async () => {
    await mockCleanupIpc({ retention: 7 });
    await mountWithSession();
    await openPage("工作区与智能体");

    expect(cleanupStatusRow().textContent ?? "").toContain("还没有清理记录");
    expect(localStorage.getItem(CLEANUP_NOTICE_SEEN_KEY)).toBeNull();
  });

  it("「保存并离开」复用同一条保存路径：保留期有改动时同样先弹清理确认，取消则照常离开", async () => {
    await mockCleanupIpc({ retention: null, preview: { count: 1, titles: ["旧会话"] } });
    await mountWithSession();
    await openSettings();
    clickNavTab("工作区与智能体");
    await waitFor(() => expect(activeNavTabText()).toBe("工作区与智能体"));
    await pickRetention("7 天");
    await waitFor(() => expect(navDot("agent")).toBe(true));

    fireEvent.click(buttonByText("返回工作区"));
    await waitFor(() => expect(confirmPending()).toBe(true));
    fireEvent.click(buttonByText("保存并离开"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("保存前确认清理"));

    fireEvent.click(buttonByText("暂不清理（下次启动仍会清理）"));
    await waitFor(() => expect(calls.save.length).toBe(1));
    expect(calls.save[0].skipCleanup).toBe(true);
    // 离开照常进行（取消只影响清理，不影响保存与离开）
    await waitFor(() => expect(document.querySelector('[data-testid="settings-page"]')).toBeFalsy());
  });
});

// ---------- 旧格式历史清理入口（分段 JSONL 落地后的显式入口） ----------
// 铁律：只有旧文件、没有新格式数据的会话**必须保留**（唯一副本）——界面要把保留数说出来；
// 而且「没什么可清」时必须有回应（点按钮没反应是最差的形态）。
describe("设置页：旧格式历史清理入口", () => {
  /** 两个命令的调用次数（每个用例开头的 mockLegacyIpc 会先复位） */
  const calls = { preview: 0, run: 0 };

  /**
   * 旧格式清理链路的 IPC mock：预览在挂载时就拉一次（只读行数据源）；
   * `afterRun` = 执行过后再拉预览时改成什么（默认一直用 `preview`）。
   * `previewFails` = 预览命令一直失败（命令层失败路径）。
   */
  async function mockLegacyIpc(opts: {
    preview?: LegacyCleanupPreview;
    outcome?: LegacyCleanupOutcome;
    afterRun?: LegacyCleanupPreview;
    previewFails?: boolean;
  }) {
    calls.preview = 0;
    calls.run = 0;
    const emptyPreview: LegacyCleanupPreview = { cleanable_sessions: 0, cleanable_bytes: 0, keep_sessions: 0 };
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "preview_legacy_history_cleanup") {
        calls.preview += 1;
        if (opts.previewFails) throw new Error("预览失败");
        if (calls.run > 0 && opts.afterRun) return opts.afterRun;
        return opts.preview ?? emptyPreview;
      }
      if (cmd === "run_legacy_history_cleanup") {
        calls.run += 1;
        return (
          opts.outcome ?? {
            deleted_sessions: 0,
            deleted_files: 0,
            freed_bytes: 0,
            kept_sessions: 0,
            failed: 0,
          }
        );
      }
      return baseInvoke(cmd, args);
    });
  }

  /** 旧格式历史状态行（注册表锚点；常驻渲染） */
  function legacyStatusRow(): HTMLElement {
    return document.querySelector('[data-setting-id="app.legacy_history_status"]') as HTMLElement;
  }

  /** 当前打开的确认框里，按文本找按钮（碰到别处同名的「取消」才不会点错） */
  function modalButton(titlePart: string, text: string): HTMLButtonElement {
    const modal = Array.from(document.querySelectorAll(".ant-modal")).find((m) =>
      (m.textContent ?? "").includes(titlePart),
    );
    expect(modal, `未找到标题含「${titlePart}」的确认框`).toBeTruthy();
    const btn = Array.from(modal!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
    );
    expect(btn, `确认框里未找到「${text}」按钮`).toBeTruthy();
    return btn as HTMLButtonElement;
  }

  it("一个旧文件都没有：状态行说无需清理，两个按钮都给回应（不弹确认框、不执行）", async () => {
    await mockLegacyIpc({});
    await mountWithSession();
    await openPage("工作区与智能体");

    await waitFor(() => expect(calls.preview).toBe(1)); // 挂载时拉一次（只读行数据源）
    expect(legacyStatusRow().textContent ?? "").toContain("没有旧格式历史，无需清理");

    fireEvent.click(buttonByText("预览可回收"));
    await waitFor(() => expect(calls.preview).toBe(2));
    await waitFor(() => expect(document.querySelectorAll(".ant-message-notice").length).toBeGreaterThan(0));

    fireEvent.click(buttonByText("立即清理旧格式历史"));
    await waitFor(() => expect(calls.preview).toBe(3));
    expect(calls.run).toBe(0); // 无事可做不发执行命令
    expect(document.body.textContent ?? "").not.toContain("清理旧格式历史？");
  });

  it("预览：可回收条数与体积、必须保留的条数都写进状态行", async () => {
    await mockLegacyIpc({ preview: { cleanable_sessions: 4, cleanable_bytes: 1572864, keep_sessions: 2 } });
    await mountWithSession();
    await openPage("工作区与智能体");

    await waitFor(() => expect(calls.preview).toBe(1));
    const row = legacyStatusRow().textContent ?? "";
    expect(row).toContain("可回收 4 个会话的旧格式历史（约 1.5 MB）");
    expect(row).toContain("另有 2 个会话必须保留（无新格式数据）");

    fireEvent.click(buttonByText("预览可回收"));
    await waitFor(() => expect(calls.preview).toBe(2));
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("可回收 4 个会话的旧格式历史，约 1.5 MB"),
    );
  });

  it("只有必须保留的那类：状态行说清「已保留 N 个」，点执行也给同一句回应", async () => {
    await mockLegacyIpc({ preview: { cleanable_sessions: 0, cleanable_bytes: 0, keep_sessions: 3 } });
    await mountWithSession();
    await openPage("工作区与智能体");

    await waitFor(() => expect(calls.preview).toBe(1));
    expect(legacyStatusRow().textContent ?? "").toContain("暂无可回收的旧格式历史（3 个会话只有旧格式数据，已保留）");

    fireEvent.click(buttonByText("立即清理旧格式历史"));
    await waitFor(() => expect(calls.preview).toBe(2));
    expect(calls.run).toBe(0);
    expect(document.body.textContent ?? "").not.toContain("清理旧格式历史？");
  });

  it("执行：确认框写清删除条数 / 体积与要保留的条数，完成后报回收统计与保留提示，状态行刷新", async () => {
    await mockLegacyIpc({
      preview: { cleanable_sessions: 4, cleanable_bytes: 1572864, keep_sessions: 2 },
      outcome: { deleted_sessions: 4, deleted_files: 4, freed_bytes: 1572864, kept_sessions: 2, failed: 0 },
      afterRun: { cleanable_sessions: 0, cleanable_bytes: 0, keep_sessions: 2 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理旧格式历史"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("清理旧格式历史？"));
    expect(document.body.textContent ?? "").toContain("将删除 4 个会话的旧格式历史文件，释放约 1.5 MB");
    expect(document.body.textContent ?? "").toContain("另有 2 个会话的旧格式历史会保留（它们是唯一副本）。");

    fireEvent.click(modalButton("清理旧格式历史？", "清理旧格式历史"));
    await waitFor(() => expect(calls.run).toBe(1));
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("已清理 4 个会话的旧格式历史（4 个文件，释放 1.5 MB）"),
    );
    await waitFor(() => expect(document.body.textContent ?? "").toContain("2 个会话因无新格式数据已保留"));
    // 执行后再拉一次预览：只读行不会留着过期的可回收数
    await waitFor(() => expect(calls.preview).toBe(3));
    await waitFor(() =>
      expect(legacyStatusRow().textContent ?? "").toContain("暂无可回收的旧格式历史（2 个会话只有旧格式数据，已保留）"),
    );
  });

  it("取消确认框：不发执行命令，状态行保持原样", async () => {
    await mockLegacyIpc({ preview: { cleanable_sessions: 1, cleanable_bytes: 2048, keep_sessions: 0 } });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理旧格式历史"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("清理旧格式历史？"));
    fireEvent.click(modalButton("清理旧格式历史？", "取消"));

    // 取消后只等一拍：antd 弹框的离场动画在 happy-dom 里不会走完（勿断言 wrap 隐藏），
    // 要断的是「没执行」这个事实
    await new Promise((r) => setTimeout(r, 50));
    expect(calls.run).toBe(0);
    expect(legacyStatusRow().textContent ?? "").toContain("约 2.0 KB");
    expect(document.body.textContent ?? "").not.toContain("已清理");
  });

  it("有失败条数：完成提示之外补一条警示（没说清「没删掉几个」会让用户以为全清完了）", async () => {
    await mockLegacyIpc({
      preview: { cleanable_sessions: 2, cleanable_bytes: 4096, keep_sessions: 0 },
      outcome: { deleted_sessions: 1, deleted_files: 1, freed_bytes: 2048, kept_sessions: 0, failed: 1 },
    });
    await mountWithSession();
    await openPage("工作区与智能体");

    fireEvent.click(buttonByText("立即清理旧格式历史"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("清理旧格式历史？"));
    fireEvent.click(modalButton("清理旧格式历史？", "清理旧格式历史"));

    await waitFor(() => expect(calls.run).toBe(1));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已清理 1 个会话的旧格式历史"));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("1 个旧格式历史文件未能删除"));
  });

  it("预览失败：不弹确认框也不执行，状态行回退到「未取到统计」", async () => {
    await mockLegacyIpc({ previewFails: true });
    await mountWithSession();
    await openPage("工作区与智能体");

    await waitFor(() => expect(calls.preview).toBe(1));
    expect(legacyStatusRow().textContent ?? "").toContain("未取到旧格式历史的统计");

    fireEvent.click(buttonByText("预览可回收"));
    await waitFor(() => expect(calls.preview).toBe(2));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("旧格式历史预览失败，本次不清理"));
    expect(calls.run).toBe(0);
  });
});
