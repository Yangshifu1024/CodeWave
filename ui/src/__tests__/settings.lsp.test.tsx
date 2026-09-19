// LSP 语义校验设置（六语言行：语言名 | 开关 | 命令覆盖 | 状态徽标；全局预算；发现区；重新探测）。
// standalone-mount 模式同 settings.network.test.tsx：同源 AntApp + 显式 i18n 初始化 + afterEach 重置面板态。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：否则 App.useApp() message 是空壳
import "../i18n"; // Component mounted standalone must init i18next explicitly
import SettingsModal from "../features/panels/SettingsModal";
import { useUi } from "../stores/ui";
import { useSettings } from "../stores/settings";
import { DEFAULT_LSP_SETTINGS } from "../ipc/types";
import type { ConfigState, LspServerStatus } from "../ipc/types";

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
    validation: {
      python: true,
      rust: true,
      typescript: true,
      go: true,
      json: true,
      dart: true,
      java: false,
      lsp: { ...DEFAULT_LSP_SETTINGS, commands: { ...DEFAULT_LSP_SETTINGS.commands } },
    },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    ...overrides,
  };
}

/** 三态徽标数据源：typescript 已找到（带版本）/ rust 启用但未找到 / java 默认关闭；其余语言不给状态（不显示徽标） */
function statusFixture(): LspServerStatus[] {
  return [
    { language: "typescript", enabled: true, found: true, source: "npx", command: "npx -y typescript-language-server --stdio", version: "1.2.3", detail: "", install: null },
    { language: "rust", enabled: true, found: false, source: "", command: "", version: null, detail: "未在 PATH 找到 rust-analyzer", install: { kind: "installable", command: "rustup component add rust-analyzer", docs_url: null, prerequisite: null } },
    { language: "java", enabled: false, found: false, source: "", command: "", version: null, detail: "Java 语义校验默认关闭", install: { kind: "confirm_enable", command: null, docs_url: null, prerequisite: "需 JDK 21+" } },
  ];
}

let savedConfigs: ConfigState[] = [];
/** lsp_status 是否注入失败（静默降级用例） */
let lspStatusFails = false;
let redetectResult: LspServerStatus[] = statusFixture();

async function baseInvoke(cmd: string, _args?: any) {
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": savedConfigs.push(JSON.parse(JSON.stringify(_args?.config))); return null;
    case "get_mcp_config": return "{}";
    case "mcp_status": return [];
    case "list_skills": return [];
    case "list_available_shells": return [];
    case "resolve_proxy": return null;
    case "lsp_status":
      if (lspStatusFails) throw new Error("lsp_status unavailable");
      return statusFixture();
    case "lsp_redetect": return redetectResult;
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

/** 某次 invoke 是否以指定命令 + 参数调用过 */
async function invokedWith(cmd: string, args?: any): Promise<boolean> {
  const mock = await invokeMock();
  return mock.mock.calls.some((c: any[]) => c[0] === cmd && (args === undefined || JSON.stringify(c[1]) === JSON.stringify(args)));
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  useUi.setState({ settingsOpen: false, settingsTab: "general" });
  useSettings.setState({ config: null, loaded: false });
  savedConfigs = [];
  lspStatusFails = false;
  redetectResult = statusFixture();
});

/** antd inserts a space inside two-character buttons: find buttons by whitespace-stripped text */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

/** 语言行（data-lang = 后端 Lang::id()） */
function rowOf(lang: string): HTMLElement {
  const el = document.querySelector(`.validation-row[data-lang="${lang}"]`) as HTMLElement | null;
  if (!el) throw new Error(`validation row not found: ${lang}`);
  return el;
}

/** 行内命令覆盖输入框（行内唯一 input；Switch 是 button） */
function cmdInput(lang: string): HTMLInputElement {
  const el = rowOf(lang).querySelector("input") as HTMLInputElement | null;
  if (!el) throw new Error(`command input not found: ${lang}`);
  return el;
}

function statusText(lang: string): string {
  return rowOf(lang).querySelector(".validation-status")?.textContent ?? "";
}

async function clickSaveAndSettle() {
  fireEvent.click(buttonByText("保存"));
  await waitFor(() => expect(savedConfigs.length).toBe(1));
  await waitFor(() => {});
}

async function openSecurityTab(config: ConfigState = makeConfig()) {
  useSettings.setState({ config, loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "security" });
  render(
    <AntApp>
      <SettingsModal />
    </AntApp>,
  );
  await waitFor(() => expect(document.querySelector('.validation-row[data-lang="typescript"]')).toBeTruthy());
}

describe("SettingsModal 安全页签：LSP 语义校验", () => {
  it("六语言按后端行序渲染（typescript→dart），JSON 另起一行；java 默认关闭", async () => {
    await openSecurityTab();
    const langs = Array.from(document.querySelectorAll(".validation-row")).map((el) => (el as HTMLElement).dataset.lang);
    expect(langs).toEqual(["typescript", "rust", "python", "go", "java", "dart", "json"]);
    expect(rowOf("java").textContent).toContain("Java");
    expect(rowOf("json").textContent).toContain("内置解析");
    expect(screen.getByRole("switch", { name: "Java" }).getAttribute("aria-checked")).toBe("false");
    expect(screen.getByRole("switch", { name: "Rust" }).getAttribute("aria-checked")).toBe("true");
    // Java 代价提示常驻（启用会启动 jdtls 解析依赖树）
    expect(document.querySelector('[data-testid="lsp-java-cost"]')?.textContent).toContain("jdtls");
  });

  it("开关写回 config.validation.<lang>（java 打开后落盘为 true）", async () => {
    await openSecurityTab();
    fireEvent.click(screen.getByRole("switch", { name: "Java" }));
    fireEvent.click(screen.getByRole("switch", { name: "Dart / Flutter" }));
    await clickSaveAndSettle();
    expect(savedConfigs[0].validation.java).toBe(true);
    expect(savedConfigs[0].validation.dart).toBe(false);
    // 既有语言开关不受影响
    expect(savedConfigs[0].validation.typescript).toBe(true);
  });

  it("命令覆盖落盘到 config.validation.lsp.commands.<lang>（保存时 trim）", async () => {
    await openSecurityTab();
    expect(cmdInput("rust").placeholder).toBe("留空 = 自动探测");
    fireEvent.change(cmdInput("rust"), { target: { value: "  rust-analyzer --stdio  " } });
    await clickSaveAndSettle();
    expect(savedConfigs[0].validation.lsp?.commands.rust).toBe("rust-analyzer --stdio");
    expect(savedConfigs[0].validation.lsp?.commands.typescript).toBe("");
  });

  it("状态徽标三态：已找到（带版本）/ 未找到（警示）/ 已关闭", async () => {
    await openSecurityTab();
    await waitFor(() => expect(statusText("typescript")).toBe("已找到 v1.2.3"));
    expect(statusText("rust")).toBe("未找到");
    expect(rowOf("rust").querySelector(".validation-status")?.className).toContain("warn");
    expect(statusText("java")).toBe("已关闭");
    // 未给状态的语言不显示徽标（探测失败语言不撒谎）
    expect(statusText("go")).toBe("");
  });

  it("lsp_status 失败时静默降级：面板照常渲染，徽标留空", async () => {
    lspStatusFails = true;
    await openSecurityTab();
    await waitFor(() => expect(statusText("rust")).toBe(""));
    expect(document.querySelectorAll(".validation-row").length).toBe(7);
  });

  it("「重新探测」调 lsp_redetect 并刷新徽标", async () => {
    await openSecurityTab();
    await waitFor(() => expect(statusText("rust")).toBe("未找到"));
    redetectResult = [
      { language: "rust", enabled: true, found: true, source: "path", command: "rust-analyzer", version: "0.3.2000", detail: "", install: null },
    ];
    fireEvent.click(buttonByText("重新探测"));
    await waitFor(() => expect(statusText("rust")).toBe("已找到 v0.3.2000"));
    expect(await invokedWith("lsp_redetect")).toBe(true);
  });

  it("预算与发现区落盘：sync_window_ms 改值、额外 SDK 根 trim 后保存（空行丢弃）", async () => {
    await openSecurityTab();
    // 预算区第一个 InputNumber = 写后等待诊断（毫秒）
    fireEvent.change(screen.getAllByRole("spinbutton")[0], { target: { value: "3000" } });
    fireEvent.click(buttonByText("添加目录"));
    const roots = document.querySelectorAll(".lsp-root-row input");
    expect(roots.length).toBe(1);
    fireEvent.change(roots[0], { target: { value: "  D:\\Sdk  " } });
    // 多加一行留空 → 保存时应被丢弃
    fireEvent.click(buttonByText("添加目录"));
    await clickSaveAndSettle();
    const lsp = savedConfigs[0].validation.lsp!;
    expect(lsp.sync_window_ms).toBe(3000);
    expect(lsp.extra_roots).toEqual(["D:\\Sdk"]);
    expect(lsp.java_home).toBe("");
    // 其余预算字段原样保留（后端默认值）
    expect(lsp.max_servers).toBe(DEFAULT_LSP_SETTINGS.max_servers);
    expect(lsp.dedupe_limit).toBe(DEFAULT_LSP_SETTINGS.dedupe_limit);
  });
});
