// 设置全屏页容器契约（[docs/settings-fullscreen-shell](../../../docs/settings-fullscreen-shell.md)）：
// 覆盖式全屏页必须**不影响正在运行的会话**——工作区全程挂载、只藏可见性、Esc 绝不停运行、
// 离开前有未保存改动时四条路径（切页 / 返回工作区 / 页内 Esc / 关窗退出）共用同一份三选拦截。
// mock 结构对齐 app.smoke.test.tsx（整树 App 挂载是唯一能验证「工作区仍挂载 + Esc 走全局键」的方式）。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup, act } from "@testing-library/react";
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
  validation: { python: true, rust: true, typescript: true, go: true, json: true },
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

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string, args?: any) => {
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
      case "cancel_run": cancelRunLog.push(args?.sessionId ?? ""); return null;
      case "resolve_exit_request": exitAnswers.push(args?.action); return null;
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
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

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

/** 设置页导航项按文本点击（脏圆点是空 span，不影响 textContent） */
function clickNavTab(label: string) {
  const tab = Array.from(document.querySelectorAll('[data-testid="settings-page"] .ant-tabs-tab')).find(
    (x) => (x.textContent ?? "").trim() === label,
  );
  if (!tab) throw new Error(`nav tab not found: ${label}`);
  fireEvent.click(tab);
}

function activeNavTabText(): string {
  return document.querySelector('[data-testid="settings-page"] .ant-tabs-tab-active')?.textContent?.trim() ?? "";
}

/** 制造未保存改动：通用页的自定义提示词是 draft 字段（非即时生效项） */
function makeDirty() {
  const ta = document.querySelector('[data-testid="settings-page"] .settings-pane-body textarea') as HTMLTextAreaElement | null;
  if (!ta) throw new Error("custom prompt textarea not found");
  fireEvent.change(ta, { target: { value: "改过的提示词" } });
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

afterEach(() => {
  cleanup();
  // zustand store 是模块级单例：面板开关/脏标记/退出请求都要复位，否则用例间串味
  useUi.setState({
    settingsOpen: false,
    settingsTab: "general",
    settingsDirty: false,
    exitRequest: null,
    closeTabRequest: null,
    tasksOpen: false,
    statsOpen: false,
    aboutOpen: false,
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
    s.lspGuide = {};
  });
  // 会话/Tab 也是模块级单例：不清就再也回不到空态（空态引导入口是建临时会话的唯一途径）
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  cancelRunLog.length = 0;
  exitAnswers.length = 0;
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
    // 7 页导航
    const navLabels = Array.from(document.querySelectorAll('[data-testid="settings-page"] .ant-tabs-tab')).map((x) =>
      (x.textContent ?? "").trim(),
    );
    expect(navLabels).toEqual(["通用", "外观", "供应商", "安全", "网络", "MCP", "技能"]);
  });

  it("样式契约：.workspace-covered 只藏可见性，绝不 display:none", () => {
    const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");
    expect(appCss).toMatch(/\.settings-shell\s*\{[^}]*position:\s*absolute/);
    expect(appCss).toMatch(/\.workspace-covered\s*\{[^}]*visibility:\s*hidden/);
    expect(appCss).toMatch(/\.workspace-covered\s*\{[^}]*pointer-events:\s*none/);
    // 硬约束：display:none 会让 ResizeObserver 测到 0 尺寸、滚动容器错乱
    expect(appCss).not.toMatch(/\.workspace-covered\s*\{[^}]*display:\s*none/);
    // 旧的弹窗内页容器已随本批退役
    expect(appCss).not.toContain(".settings-body");
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
    expect(document.activeElement).toBe(
      document.querySelector('[data-testid="settings-page"] .settings-nav-head button'),
    );
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

    clickNavTab("外观");
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    // 三选齐全
    expect(buttonByText("保存并离开")).toBeTruthy();
    expect(buttonByText("放弃改动")).toBeTruthy();
    expect(buttonByText("留在原地")).toBeTruthy();
    // 留在原地：仍在原页，未保存改动保留，弹框收起
    fireEvent.click(buttonByText("留在原地"));
    await waitFor(() => expect(confirmPending()).toBe(false));
    expect(activeNavTabText()).toBe("通用");
    expect(document.querySelector(".settings-dirty-dot")).toBeTruthy();

    // 放弃改动：跳页且脏点消失
    clickNavTab("外观");
    await waitFor(() => expect(leaveModalVisible()).toBe(true));
    fireEvent.click(buttonByText("放弃改动"));
    await waitFor(() => expect(activeNavTabText()).toBe("外观"));
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
    await openSettings();
    const prompt = () =>
      document.querySelector('[data-testid="settings-page"] .settings-pane-body textarea') as HTMLTextAreaElement;

    // 原值 null，输入文字 → 脏
    fireEvent.change(prompt(), { target: { value: "只用中文回答" } });
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());
    expect(useUi.getState().settingsDirty).toBe(true);

    // 再全部删空：回到原值（null 与 "" 归一）→ 不脏
    fireEvent.change(prompt(), { target: { value: "" } });
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
    await openSettings();
    clickNavTab("网络");
    await waitFor(() => expect(activeNavTabText()).toBe("网络"));

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

  it("脏标记空值归一：新增的空 SDK 根行不算改动，填了路径才脏", async () => {
    await mountWithSession();
    await openSettings();
    clickNavTab("安全");
    await waitFor(() => expect(activeNavTabText()).toBe("安全"));

    // 「添加目录」只补一个空行（保存时会被丢掉）→ 不该染脏
    // （同时守住 validation 段的缺省项：draft 里的 lsp 段与后端 serde default 等价）
    fireEvent.click(buttonByText("添加目录"));
    await new Promise((r) => setTimeout(r, 60));
    expect(document.querySelectorAll(".lsp-root-row").length).toBe(1);
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();

    // 填了真实路径 → 脏
    fireEvent.change(document.querySelector(".lsp-root-row input") as HTMLElement, { target: { value: "D:/Sdk" } });
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeTruthy());
  });

  it("按页打点：只标改动页，保存后清空", async () => {
    await mountWithSession();
    await openSettings();
    expect(document.querySelector(".settings-dirty-dot")).toBeFalsy();

    makeDirty();
    await waitFor(() => expect(document.querySelectorAll(".settings-dirty-dot").length).toBeGreaterThan(0));
    // 只有「通用」页的导航项带点（操作条那枚另算）
    const navDot = document.querySelector('[data-testid="settings-page"] .ant-tabs-tab-active .settings-dirty-dot');
    expect(navDot).toBeTruthy();
    expect(document.querySelectorAll('[data-testid="settings-page"] .ant-tabs-tab .settings-dirty-dot').length).toBe(1);

    fireEvent.click(buttonByText("保存"));
    await waitFor(() => expect(document.querySelector(".settings-dirty-dot")).toBeFalsy());
    expect(useUi.getState().settingsDirty).toBe(false);
  });

  it("即时生效项不打点：切界面语言不产生未保存状态", async () => {
    await mountWithSession();
    await openSettings();
    const select = document.querySelector('[data-testid="settings-page"] .settings-pane-body .ant-select') as HTMLElement;
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

  it("深链：带页签跳该页；无参保持当前页不重置", async () => {
    await mountWithSession();
    await openSettings();
    expect(activeNavTabText()).toBe("通用");

    // 带参（认证错误卡「打开模型设置」/ LSP 引导卡走同一条路径）
    act(() => {
      useUi.getState().showSettings("providers");
    });
    await waitFor(() => expect(activeNavTabText()).toBe("供应商"));
    expect(document.querySelector('[data-testid="settings-page"] .settings-pane-body')?.textContent ?? "").toContain("添加供应商");

    // 无参：保持当前页（有意变更，旧实现一律回 general）
    act(() => {
      useUi.getState().showSettings();
    });
    await new Promise((r) => setTimeout(r, 60));
    expect(activeNavTabText()).toBe("供应商");
    expect(useUi.getState().settingsOpen).toBe(true);
  });
});
