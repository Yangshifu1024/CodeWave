// Network settings tests: proxy mode radio cards (none/system/manual), custom URL validation,
// system-proxy echo. Same standalone-mount pattern as shell.settings.test.tsx.
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：否则 App.useApp() message 是空壳
import "../i18n"; // Component mounted standalone must init i18next explicitly
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

let savedConfigs: ConfigState[] = [];

async function baseInvoke(cmd: string, _args?: any) {
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": savedConfigs.push(JSON.parse(JSON.stringify(_args?.config))); return null;
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "list_skills": return [];
    case "list_available_shells": return [];
    case "resolve_proxy": return "http://127.0.0.1:9999";
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

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  useUi.setState({ settingsOpen: false, settingsTab: "appearance" });
  useSettings.setState({ config: null, loaded: false });
  savedConfigs = [];
});

/** antd inserts a space inside two-character buttons: find buttons by whitespace-stripped text */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

async function clickSaveAndSettle() {
  fireEvent.click(buttonByText("保存"));
  await waitFor(() => expect(savedConfigs.length).toBe(1));
  await waitFor(() => {});
}

async function openNetworkTab(config: ConfigState = makeConfig()) {
  useSettings.setState({ config, loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "network" });
  render(
    <AntApp>
      <SettingsPage />
    </AntApp>,
  );
  await waitFor(() => expect(screen.getByText("代理模式")).toBeTruthy());
}

/** 按标题找模式卡片 */
function cardByTitle(title: string): HTMLElement {
  const card = Array.from(document.querySelectorAll(".proxy-mode-card")).find(
    (c) => c.querySelector(".proxy-mode-title")?.textContent === title,
  );
  if (!card) throw new Error(`proxy card not found: ${title}`);
  return card as HTMLElement;
}

/** 自定义代理地址输入框（placeholder 唯一含 7890） */
async function proxyInput(): Promise<HTMLInputElement> {
  return waitFor(() => {
    const el = document.querySelector("input[placeholder*='7890']") as HTMLInputElement | null;
    expect(el).toBeTruthy();
    return el!;
  });
}

describe("设置页网络页签：代理模式", () => {
  it("proxy=null（从未配置）默认选中「系统代理」，显示探测回显", async () => {
    await openNetworkTab();
    expect(cardByTitle("系统代理").className).toContain("active");
    expect(cardByTitle("无代理").className).not.toContain("active");
    expect(document.body.textContent ?? "").toContain("当前检测：http://127.0.0.1:9999");
  });

  it("切「无代理」保存 → proxy = { mode: none, url: '' }", async () => {
    await openNetworkTab();
    fireEvent.click(cardByTitle("无代理").querySelector(".ant-radio-input") as HTMLElement);
    await clickSaveAndSettle();
    expect(savedConfigs[0].proxy).toEqual({ mode: "none", url: "" });
  });

  it("自定义代理：填合法 URL 保存 → trim 后的 { mode: manual, url }", async () => {
    await openNetworkTab();
    fireEvent.click(cardByTitle("自定义代理").querySelector(".ant-radio-input") as HTMLElement);
    const input = await proxyInput();
    fireEvent.change(input, { target: { value: "  http://127.0.0.1:7890  " } });
    await clickSaveAndSettle();
    expect(savedConfigs[0].proxy).toEqual({ mode: "manual", url: "http://127.0.0.1:7890" });
  });

  it("非法前缀（ftp://）→ 即时红字，保存被拦不落盘", async () => {
    await openNetworkTab();
    fireEvent.click(cardByTitle("自定义代理").querySelector(".ant-radio-input") as HTMLElement);
    const input = await proxyInput();
    fireEvent.change(input, { target: { value: "ftp://1.2.3.4" } });
    // 即时校验：Form.Item help 红字
    await waitFor(() =>
      expect(document.querySelector(".ant-form-item-explain-error")?.textContent).toContain("代理地址格式无效"),
    );
    fireEvent.click(buttonByText("保存"));
    await new Promise((r) => setTimeout(r, 80)); // 若误放行，save_config 会入队
    expect(savedConfigs.length).toBe(0);
  });

  it("来回切换模式草稿保留已填地址", async () => {
    await openNetworkTab();
    fireEvent.click(cardByTitle("自定义代理").querySelector(".ant-radio-input") as HTMLElement);
    const input = await proxyInput();
    fireEvent.change(input, { target: { value: "socks5://10.0.0.1:1080" } });
    // 切走收起输入框
    fireEvent.click(cardByTitle("无代理").querySelector(".ant-radio-input") as HTMLElement);
    expect(document.querySelector("input[placeholder*='7890']")).toBeNull();
    // 切回回显草稿
    fireEvent.click(cardByTitle("自定义代理").querySelector(".ant-radio-input") as HTMLElement);
    expect((await proxyInput()).value).toBe("socks5://10.0.0.1:1080");
  });
});
