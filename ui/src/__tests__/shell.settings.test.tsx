// Shell settings tests: general-tab Shell select fed by list_available_shells, persisted via config.shell.selection.
// SettingsModal is mounted standalone (like providers.panel.test.tsx): useUi controls settingsOpen/settingsTab.
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context，
// 导致 App.useApp() 拿到空壳 message（message.error is not a function）
import "../i18n"; // Component mounted standalone must init i18next explicitly (no global entry outside App.tsx)
import SettingsModal from "../features/panels/SettingsModal";
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

const fixtureShells = [
  { id: "git_bash", name: "Git Bash", path: "C:/Program Files/Git/bin/bash.exe", kind: "posix", limited: false, auto: true },
  { id: "powershell", name: "PowerShell", path: "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe", kind: "powershell", limited: false, auto: false },
  { id: "cmd", name: "CMD", path: "C:/Windows/System32/cmd.exe", kind: "cmd", limited: true, auto: false },
  { id: "wsl", name: "WSL", path: null, kind: "wsl", limited: true, auto: false },
];

let savedConfigs: ConfigState[] = [];

// Base mock: mirror the backend contract (commands used by SettingsModal resolve with well-typed values)
async function baseInvoke(cmd: string, _args?: any) {
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": savedConfigs.push(JSON.parse(JSON.stringify(_args?.config))); return null;
    case "list_available_shells": return JSON.parse(JSON.stringify(fixtureShells));
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "list_skills": return [];
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
  // restore the base implementation (mockImplementation in tests would otherwise leak)
  (await invokeMock()).mockImplementation(baseInvoke);
  // zustand module-level singletons persist across tests: reset panel toggles + settings mirror
  useUi.setState({ settingsOpen: false, settingsTab: "general" });
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

/** Save and settle ALL post-save microtasks (store.set → antd App message) in-act before unmounting */
async function clickSaveAndSettle() {
  fireEvent.click(buttonByText("保存"));
  await waitFor(() => expect(savedConfigs.length).toBe(1));
  await waitFor(() => {});
}

/** Open the standalone modal and wait for the draft (config mirror) to load */
async function openSettings() {
  useSettings.setState({ config: makeConfig(), loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "general" });
  render(
    <AntApp>
      <SettingsModal />
    </AntApp>,
  );
  await waitFor(() => expect(screen.getByText("Shell")).toBeTruthy());
}

/** The shell Select is the second select in the general pane (first = language) */
function shellSelect(): HTMLElement {
  return document.querySelectorAll(".ant-modal .ant-select")[1] as HTMLElement;
}

/** Open the shell dropdown (antd Select needs mousedown, not click) and pick an option by text */
async function pickShellOption(text: string) {
  fireEvent.mouseDown(shellSelect());
  await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy());
  const option = Array.from(document.querySelectorAll(".ant-select-item-option")).find((o) =>
    (o.textContent ?? "").includes(text),
  ) as HTMLElement;
  expect(option).toBeTruthy();
  fireEvent.click(option);
  await new Promise((r) => setTimeout(r, 80)); // let the change event settle
}

describe("settings store：shell 默认值", () => {
  it("load 后默认配置 shell.selection === null（自动）", async () => {
    useSettings.setState({ config: null, loaded: false });
    await useSettings.getState().load();
    expect(useSettings.getState().config?.shell?.selection).toBeNull();
    expect(useSettings.getState().config?.shell).toEqual({ selection: null });
  });
});

describe("SettingsModal general tab：Shell 选择", () => {
  it("打开面板拉取探测列表：选项数量正确，limited 项有（有限支持）标注，path 进 option title", async () => {
    await openSettings();
    // 默认选中「自动」，auto label 附探测列表首个（非 limited）默认 shell 名
    expect(document.body.textContent ?? "").toContain("自动（默认：Git Bash）");
    fireEvent.mouseDown(shellSelect());
    await waitFor(() => expect(document.querySelector(".ant-select-dropdown")).toBeTruthy());
    const options = Array.from(document.querySelectorAll(".ant-select-item-option"));
    expect(options).toHaveLength(5); // auto + git_bash + powershell + cmd + wsl
    expect(options[1].textContent).toContain("Git Bash");
    expect(options[2].textContent).toContain("PowerShell");
    expect(options[3].textContent).toContain("CMD（有限支持）");
    expect(options[4].textContent).toContain("WSL（有限支持）");
    expect(options[3].getAttribute("title") ?? "").toContain("cmd.exe");
  });

  it("选中 PowerShell → draft 更新 → 保存参数含 shell.selection = powershell", async () => {
    await openSettings();
    await pickShellOption("PowerShell");
    expect(shellSelect().textContent).toContain("PowerShell");
    await clickSaveAndSettle();
    expect(savedConfigs[0].shell?.selection).toBe("powershell");
  });

  it("从固定 shell 切回自动 → 保存参数 shell.selection = null", async () => {
    useSettings.setState({ config: makeConfig({ shell: { selection: "powershell" } }), loaded: true });
    useUi.setState({ settingsOpen: true, settingsTab: "general" });
    render(
      <AntApp>
        <SettingsModal />
      </AntApp>,
    );
    await waitFor(() => expect(screen.getByText("Shell")).toBeTruthy());
    await pickShellOption("自动（默认：Git Bash）");
    await clickSaveAndSettle();
    expect(savedConfigs[0].shell?.selection).toBeNull();
  });

  it("探测接口 reject：仍渲染「自动（推荐）」且显示失败提示", async () => {
    const invoke = await invokeMock();
    invoke.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "list_available_shells") throw new Error("detect failed");
      return baseInvoke(cmd, args);
    });
    await openSettings();
    expect(document.body.textContent ?? "").toContain("自动（推荐）");
    expect(document.body.textContent ?? "").toContain("Shell 探测失败，仅显示自动选项");
    // 失败态无固定 selection：不出现「已卸载」警告；路径回显不渲染
    expect(document.body.textContent ?? "").not.toContain("所选 shell 未检测到");
    expect(document.body.textContent ?? "").not.toContain("bash.exe");
  });

  it("所选 shell 已卸载（探测列表不含 selection）：保留选项 + 警告提示", async () => {
    useSettings.setState({ config: makeConfig({ shell: { selection: "zsh" } }), loaded: true });
    useUi.setState({ settingsOpen: true, settingsTab: "general" });
    render(
      <AntApp>
        <SettingsModal />
      </AntApp>,
    );
    await waitFor(() => expect(screen.getByText("Shell")).toBeTruthy());
    // 当前值直接显示已卸载的 shell id（选项保留不删除）
    expect(shellSelect().textContent).toContain("zsh");
    expect(document.body.textContent ?? "").toContain("所选 shell 未检测到，执行时将回退到自动探测的 shell");
    // 已卸载态不回显路径（探测列表无对应项）
    expect(document.body.textContent ?? "").not.toContain("bash.exe");
  });

  describe("Shell 路径回显", () => {
    it("auto 默认回显探测默认 shell 绝对路径；切 PowerShell/WSL 跟随更新", async () => {
      await openSettings();
      // auto：与「自动（默认：Git Bash）」同源，回显 auto 项绝对路径
      expect(document.body.textContent ?? "").toContain("C:/Program Files/Git/bin/bash.exe");
      await pickShellOption("PowerShell");
      expect(document.body.textContent ?? "").toContain(
        "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
      );
      await pickShellOption("WSL");
      // path=null → 占位文案，不残留上一个 shell 的路径
      expect(document.body.textContent ?? "").toContain("该 shell 无固定可执行文件路径");
      expect(document.body.textContent ?? "").not.toContain("powershell.exe");
    });

    it("回显元素带 title 悬停完整路径（code + mono token）", async () => {
      await openSettings();
      const code = document.querySelector(".ant-modal .ant-form-item code[title]") as HTMLElement | null;
      expect(code).toBeTruthy();
      expect(code!.getAttribute("title")).toBe("C:/Program Files/Git/bin/bash.exe");
      expect(code!.textContent).toContain("bash.exe");
    });
  });
});
