// 设置页 MCP 页（自「工具与集成」拆出）：服务器状态表 + 配置区共存。
// 状态数据面（mcp_status 命令 / mcp:status 事件 / useUi.mcpStatus）早已存在，本页是它**唯一**的渲染方，
// 所以本文件同时守护三件容易做错的事：
//   ① 名单取「配置 ∪ 状态」并集——只取状态会在保存配置后（后端 stop_all、且无会话不重连）得到空表，
//      看起来像「没配置服务器」；只取配置会漏掉「配置里已删、管理端仍持有连接」的服务器；
//   ② 无配置服务器时整段不渲染（空表会把「没配」与「没连」显示成同一个样子）；
//   ③ 刷新按钮只重读状态、**不会重连**（手动重连会打断其他会话正在跑的 MCP 调用，属非目标）。
// 挂载方式与 settings.skills.test.tsx 同源（standalone + useUi 控制开关）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup } from "@testing-library/react";
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
    post_write_check: { enabled: false, command: "", timeout_seconds: 30, tail_chars: 3000 },
    ui: { font_size: 15, accent: "cyan", language: "zh-CN" },
    custom_prompt: null,
    disabled_skills: [],
    log: { level: "info", session_verbose: false },
    shell: { selection: null },
    ...overrides,
  };
}

/** 两个服务器的 mcp.json（一个 stdio、一个 streamable_http） */
const MCP_CONFIG = JSON.stringify({
  mcpServers: {
    fs: { command: "npx", args: ["-y", "@modelcontextprotocol/server-fs"] },
    web: { transport: "streamable_http", url: "https://example.com/mcp" },
  },
});

let calls: string[] = [];
/** mcp_status 的返回值（每个用例自行设置；元素形态与 ipc/client.ts 的 mcpStatus 一致） */
let statusReply: { name: string; state: unknown; tools: number }[] = [];
/** mcp_list_config 的 json 字段（默认两个服务器；用例可改成 "{}" / 非法文本测空态与兜底模式） */
let configReply = MCP_CONFIG;

async function baseInvoke(cmd: string, args?: any) {
  calls.push(args ? `${cmd}:${JSON.stringify(args)}` : cmd);
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": return null;
    case "list_available_shells": return [];
    case "mcp_list_config":
      return { scope: "global", path: "C:/u/.codewave/mcp.json", json: configReply, servers: [], effective: [], issues: [] };
    case "mcp_save_config": return { saved: true, issues: [] };
    case "mcp_status": return JSON.parse(JSON.stringify(statusReply));
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

afterEach(async () => {
  cleanup();
  (await invokeMock()).mockImplementation(baseInvoke);
  useUi.setState({ settingsOpen: false, settingsTab: "appearance", mcpStatus: [], settingsHit: null });
  useSettings.setState({ config: null, loaded: false });
  calls = [];
  statusReply = [];
  configReply = MCP_CONFIG;
});

/** 状态调用次数（刷新按钮「只重读、不重连」与「进页 refetch」都靠它断言） */
function statusCalls(): number {
  return calls.filter((c) => c === "mcp_status").length;
}

function statusTable(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[data-testid="settings-page"] [data-setting-id="app.mcp_status"]');
}

/**
 * 状态表的服务器行（表头行共用同一个类名，故按类名排除）。
 * 三列分别取文本：行内是并列的 span（JSX 会吃掉元素间的空白），拼 textContent 拼不出分隔符。
 */
function statusRows(): { name: string; state: string; tools: string }[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-testid="settings-page"] .mcp-status-row'))
    .filter((r) => !r.classList.contains("mcp-status-row-head"))
    .map((r) => ({
      name: r.querySelector(".mcp-status-name")?.textContent ?? "",
      state: (r.querySelector(".mcp-status-state")?.textContent ?? "").trim(),
      tools: r.querySelector(".mcp-status-tools")?.textContent ?? "",
    }));
}

function buttonByText(text: string): HTMLElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLElement;
}

async function openMcpTab() {
  useSettings.setState({ config: makeConfig(), loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "mcp" });
  render(
    <AntApp>
      <SettingsPage />
    </AntApp>,
  );
  await waitFor(() => expect(document.querySelector('[data-setting-id="mcp.servers"]')).toBeTruthy());
}

describe("设置页 MCP 页：服务器状态表", () => {
  it("有配置时渲染状态表：名称 / 状态 / 工具数 + 全局语义说明 + app.mcp_status 锚点", async () => {
    // fs 已连接（12 把工具）、web 连接失败、cfg-only 只在配置里（= 未连接）
    statusReply = [
      { name: "fs", state: "ready", tools: 12 },
      { name: "web", state: { error: "spawn npx ENOENT" }, tools: 0 },
      { name: "cfg-only", state: undefined, tools: 0 },
    ];
    configReply = JSON.stringify({ mcpServers: { fs: {}, web: {}, "cfg-only": {} } });
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "已连接", tools: "12" },
      { name: "web", state: "连接失败", tools: "—" },
      { name: "cfg-only", state: "未连接", tools: "—" },
    ]);
    // 表头三列（名称列复用 mcpName 文案）
    expect(statusTable()?.querySelector(".mcp-status-row-head")?.textContent).toContain("工具数");
    // 说明文案承担「未连接是正常态」的解释职责
    expect(statusTable()?.textContent).toContain("连接在打开会话时建立");
  });

  it("状态记录为空（没有会话驱动连接）时全部显示「未连接」——不是「没配置」", async () => {
    statusReply = [];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "未连接", tools: "—" },
      { name: "web", state: "未连接", tools: "—" },
    ]);
  });

  it("状态里多出配置里已删的服务器也要列出来（否则状态凭空消失）", async () => {
    statusReply = [{ name: "ghost", state: "ready", tools: 3 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "未连接", tools: "—" },
      { name: "web", state: "未连接", tools: "—" },
      { name: "ghost", state: "已连接", tools: "3" },
    ]);
  });

  it("连接失败行：点击整行展开完整错误（含 stderr），再点收起", async () => {
    statusReply = [{ name: "fs", state: { error: "Error: spawn npx ENOENT\n  at child_process" }, tools: 0 }];
    await openMcpTab();

    const row = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-testid="settings-page"] .mcp-status-row-clickable');
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    // 收起态：只有状态文字，没有错误块
    expect(row.getAttribute("aria-expanded")).toBe("false");
    expect(document.querySelector(".mcp-status-error")).toBeFalsy();

    fireEvent.click(row);
    await waitFor(() => expect(document.querySelector(".mcp-status-error")).toBeTruthy());
    expect(document.querySelector(".mcp-status-error")?.textContent).toContain("spawn npx ENOENT");
    expect(row.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(row);
    await waitFor(() => expect(document.querySelector(".mcp-status-error")).toBeFalsy());
    expect(row.getAttribute("aria-expanded")).toBe("false");
  });

  it("「未连接」行不可点（只有连接失败才有错误可展开）", async () => {
    statusReply = [];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(document.querySelector('[data-testid="settings-page"] .mcp-status-row-clickable')).toBeFalsy();
  });

  it("连接中（starting）：显示「连接中」+ spinner，且该行不可点（无错误可展开）", async () => {
    statusReply = [{ name: "fs", state: "starting", tools: 0 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "连接中", tools: "—" },
      { name: "web", state: "未连接", tools: "—" },
    ]);
    expect(document.querySelector('[data-testid="settings-page"] .mcp-status-state .ant-spin')).toBeTruthy();
    expect(document.querySelector('[data-testid="settings-page"] .mcp-status-row-clickable')).toBeFalsy();
  });

  it("刷新按钮：带「不会重新连接」的 aria-label，点击只重读 mcp_status，绝不触发 connect_mcp", async () => {
    statusReply = [{ name: "fs", state: "ready", tools: 1 }];
    await openMcpTab();

    const btn = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('button[aria-label="刷新状态（不会重新连接）"]');
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    const before = statusCalls();
    const beforeConnect = calls.filter((c) => c.startsWith("connect_mcp")).length;
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    fireEvent.click(btn);

    await waitFor(() => expect(statusCalls()).toBeGreaterThan(before));
    await waitFor(() => expect(statusRows()[0]).toEqual({ name: "fs", state: "已连接", tools: "2" }));
    // 刷新只重读状态：前后 connect_mcp 调用数必须一致（用计数而非「全仓为空」，否则断言恒真）
    expect(calls.filter((c) => c.startsWith("connect_mcp")).length).toBe(beforeConnect);
  });

  it("切到 MCP 页时重读一次状态（mcp:status 事件只在 connect_mcp 后触发，久留会看到陈旧状态）", async () => {
    statusReply = [];
    useSettings.setState({ config: makeConfig(), loaded: true });
    // 先停在「界面」页：挂载时拉过一次状态，之后切页应再拉一次
    useUi.setState({ settingsOpen: true, settingsTab: "appearance" });
    render(
      <AntApp>
        <SettingsPage />
      </AntApp>,
    );
    await waitFor(() => expect(statusCalls()).toBe(1));

    fireEvent.click(document.querySelector('[data-page="mcp"]') as HTMLElement);
    await waitFor(() => expect(useUi.getState().settingsTab).toBe("mcp"));
    await waitFor(() => expect(statusCalls()).toBe(2));
  });

  it("没有配置服务器、也没有状态记录：整段不渲染（只留配置区与添加引导）", async () => {
    configReply = JSON.stringify({ mcpServers: {} });
    statusReply = [];
    await openMcpTab();

    expect(statusTable()).toBeFalsy();
    expect(document.body.textContent ?? "").toContain("添加服务器");
  });

  it("没配置服务器但有状态记录（管理端仍持有连接）：照样列出该行，不静默吞掉状态", async () => {
    configReply = JSON.stringify({ mcpServers: {} });
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([{ name: "fs", state: "已连接", tools: "2" }]);
  });

  it("mcp.json 解析失败改用文本兜底编辑：原文可见、保存直存原文（不被空配置覆盖）", async () => {
    configReply = "not-json";
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    await openMcpTab();

    // 兜底模式：显示说明文案 + JSON 原文（可直接改），而不是把解析不了的配置吞掉当「没有服务器」
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已回退为原始编辑模式"));
    const ta = await waitFor(() => {
      const el = document.querySelector<HTMLTextAreaElement>('[data-testid="settings-page"] textarea');
      expect(el).toBeTruthy();
      return el as HTMLTextAreaElement;
    });
    expect(ta.value).toBe("not-json");

    // 保存走原文直存。回归点：先前这一路径拿到的是空结构化列表，会把用户的 mcp.json 覆盖成 {"mcpServers":{}}
    fireEvent.click(buttonByText("保存并重连"));
    await waitFor(() => expect(calls.some((c) => c.startsWith("mcp_save_config"))).toBe(true));
    expect(calls.find((c) => c.startsWith("mcp_save_config"))).toContain("not-json");

    // 状态表：拿不到配置名单那一半按空处理 → 只列管理端已知的服务器
    expect(statusRows()).toEqual([{ name: "fs", state: "已连接", tools: "2" }]);
  });
});
