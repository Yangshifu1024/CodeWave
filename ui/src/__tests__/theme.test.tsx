// 主题三档（跟随系统/亮色/暗色）契约测试：store 持久化 + effectiveDark 派生单源 + html.dark 同步 + 设置页切换即时生效。
// mock 结构对齐 app.smoke.test.tsx；matchMedia 垫片可编程控制 OS 亮暗（setup.ts 的静态垫片对本文件不够用）。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, fireEvent, cleanup, waitFor, act } from "@testing-library/react";

// ---------- Tauri IPC mock（App 挂载最小依赖面） ----------
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
  approval: { enabled: true, confirm_outside_create: true, confirm_git_push: true, auto_confirm: false },
  validation: { python: true, rust: true, typescript: true, go: true, json: true },
  ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
  custom_prompt: null,
  disabled_skills: [],
  log: { level: "info", session_verbose: false },
};

const fixtureSession = {
  id: "s1", title: "主题测试会话", workspace: "/tmp/ws", model_id: "m1",
  created_at: "2026-08-30T00:00:00Z", updated_at: "2026-08-30T00:00:00Z", message_count: 0,
};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => {
    switch (cmd) {
      case "ping": return "pong";
      case "activate_and_show": return null;
      case "get_config": return fixtureConfig;
      case "list_sessions": return [fixtureSession];
      case "list_projects": return [];
      case "load_session": return [];
      case "git_status": return { repo: false, entries: [] };
      case "git_user_info": return { name: "测试用户", email: "tester@example.com" };
      case "get_token_breakdown":
        return { system_tokens: 1, history_tokens: 2, tool_results_tokens: 0, tool_schema_tokens: 3, total_tokens: 6, context_window: 128000, ratio: 0.01 };
      case "get_mcp_config": return "{}";
      case "mcp_status": return [];
      case "list_skills": return [];
      case "list_agents": return [];
      case "search_workspace_paths": return [];
      case "list_workspace_dir": return { entries: [] };
      case "get_token_stats": return [];
      case "get_session_prefs": return { approval_mode: "plan", model_id: null, reasoning_effort: null };
      case "set_session_prefs": return null;
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

// ---------- 可控 matchMedia（模拟 OS 亮暗与实时切换） ----------
let osDark = false;
let mqListeners: ((e: { matches: boolean }) => void)[] = [];
function setOsDark(dark: boolean) {
  osDark = dark;
  for (const fn of [...mqListeners]) fn({ matches: dark });
}

beforeAll(() => {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string) => ({
      get matches() {
        return osDark;
      },
      media: query,
      onchange: null,
      addEventListener: (_t: string, fn: (e: { matches: boolean }) => void) => mqListeners.push(fn),
      removeEventListener: (_t: string, fn: (e: { matches: boolean }) => void) => {
        mqListeners = mqListeners.filter((x) => x !== fn);
      },
      addListener: () => {},
      removeListener: () => {},
      dispatchEvent: () => false,
    }),
  });
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

import App from "../App";
import { useUi, readStoredTheme } from "../stores/ui";
import { useSettings } from "../stores/settings";

async function mountApp() {
  render(<App />);
  await waitFor(() => expect(document.body.textContent ?? "").toContain("项目"), { timeout: 5000 });
  await new Promise((r) => setTimeout(r, 50));
}

afterEach(() => {
  cleanup();
  // zustand store 是模块级单例：复位主题与面板开关，防止用例间残留
  useUi.setState({ theme: "system", settingsOpen: false, tasksOpen: false, statsOpen: false, rightBarOpen: true, rbTab: "info" });
  localStorage.clear();
  osDark = false;
  mqListeners = [];
});

describe("主题偏好 store（localStorage ws_theme）", () => {
  it("默认跟随系统；setTheme 即时写入 store 并持久化", () => {
    expect(readStoredTheme()).toBe("system");
    expect(useUi.getState().theme).toBe("system");
    useUi.getState().setTheme("dark");
    expect(useUi.getState().theme).toBe("dark");
    expect(localStorage.getItem("ws_theme")).toBe("dark");
    expect(readStoredTheme()).toBe("dark");
  });

  it("localStorage 非法值/缺失值回退跟随系统", () => {
    localStorage.setItem("ws_theme", "blue");
    expect(readStoredTheme()).toBe("system");
    localStorage.removeItem("ws_theme");
    expect(readStoredTheme()).toBe("system");
  });
});

describe("effectiveDark 派生与 html.dark 同步", () => {
  it("跟随系统：OS 亮→暗实时切换，dark class 即时出现", async () => {
    await mountApp();
    expect(document.documentElement.classList.contains("dark")).toBe(false);
    act(() => setOsDark(true));
    await waitFor(() => expect(document.documentElement.classList.contains("dark")).toBe(true));
  });

  it("固定亮色：OS 切暗界面保持亮色", async () => {
    useUi.setState({ theme: "light" });
    await mountApp();
    act(() => setOsDark(true));
    await new Promise((r) => setTimeout(r, 50));
    expect(document.documentElement.classList.contains("dark")).toBe(false);
  });

  it("固定暗色：OS 为亮仍立即暗色", async () => {
    useUi.setState({ theme: "dark" });
    await mountApp();
    expect(document.documentElement.classList.contains("dark")).toBe(true);
  });
});

describe("设置 → 外观：主题选择即时生效", () => {
  it("Select 切到暗色后 store/localStorage/html.dark 三处同步", async () => {
    await mountApp();
    // seed 配置后 store 直驱打开设置弹窗并落在「外观」页签（既有惯例，见 shell.settings.test；
    // SettingsPage 的 Tabs 依赖 draft=useSettings.config，未 seed 时仅渲染空页）
    // 设置页全屏覆盖层已不再有 Modal 外壳（docs/settings-fullscreen-shell）：选择器锚 .settings-shell
    useSettings.setState({ config: JSON.parse(JSON.stringify(fixtureConfig)), loaded: true });
    useUi.setState({ settingsOpen: true, settingsTab: "appearance" });
    // 外观页签唯一的 Select 即主题三档（字体项是 Input）；antd 6.6 需对 .ant-select 根元素 mouseDown 展开
    const themeSelect = await waitFor(
      () => {
        const el = document.querySelector(".settings-shell .ant-select") as HTMLElement | null;
        expect(el).toBeTruthy();
        return el as HTMLElement;
      },
      { timeout: 3000 },
    );
    fireEvent.mouseDown(themeSelect);
    await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy(), { timeout: 3000 });
    const option = await waitFor(
      () => {
        const opts = Array.from(document.querySelectorAll(".ant-select-item-option"));
        const found = opts.find((o) => o.textContent?.trim() === "暗色");
        expect(found).toBeTruthy();
        return found as HTMLElement;
      },
      { timeout: 3000 },
    );
    fireEvent.click(option);
    await waitFor(() => expect(document.documentElement.classList.contains("dark")).toBe(true));
    expect(useUi.getState().theme).toBe("dark");
    expect(localStorage.getItem("ws_theme")).toBe("dark");
  });
});
