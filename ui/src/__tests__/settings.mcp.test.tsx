// 设置页 MCP 页（自「工具与集成」拆出）：服务器卡片 + 配置区共存。
// 状态数据面（mcp_status 命令 / mcp:status 事件 / useUi.mcpStatus）早已存在，本页是它**唯一**的渲染方，
// 所以本文件同时守护三件容易做错的事：
//   ① 当前作用域配置决定卡片名单——只取状态会在保存配置后（后端 stop_all、且无会话不重连）得到空区，
//      看起来像「没配置服务器」；原始配置回退模式则由状态记录提供可见服务器；
//   ② 无配置服务器时保留新建入口与空态；
//   ③ 刷新按钮只重读状态、**不会重连**（手动重连会打断其他会话正在跑的 MCP 调用，属非目标）。
// 挂载方式与 settings.skills.test.tsx 同源（standalone + useUi 控制开关）。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // 必须与组件同源（主入口）：es/app 子路径会产生另一个 context
import "../i18n";
import SettingsPage from "../features/panels/SettingsPage";
import { useUi } from "../stores/ui";
import { useSettings } from "../stores/settings";
import { useSessions } from "../stores/sessions";
import { parseMcpDoc } from "../utils/mcpConfig";
import type { ConfigState, McpServerView } from "../ipc/types";

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
let statusReply: { name: string; state: unknown; tools: number; scope?: "global" | "project"; tool_details?: { name: string; description: string }[] }[] = [];
let effectiveReply: Array<Pick<McpServerView, "name" | "source" | "overridden">> = [];
/** mcp_list_config 的 json 字段（默认两个服务器；用例可改成 "{}" / 非法文本测空态与兜底模式） */
let configReply = MCP_CONFIG;
let saveAllowed = true;
let projectLoadFails = false;
/** mcp_test 的返回值（临时测试连接：不改动正式状态） */
let testReply: { ok: boolean; tools: number; error: null | { message: string } } = {
  ok: true,
  tools: 2,
  error: null,
};

async function baseInvoke(cmd: string, args?: any) {
  calls.push(args ? `${cmd}:${JSON.stringify(args)}` : cmd);
  switch (cmd) {
    case "get_config": return makeConfig();
    case "save_config": return null;
    case "list_available_shells": return [];
    case "mcp_list_config":
      if (args?.scope === "project" && projectLoadFails) throw new Error("read failed");
      return {
        scope: args?.scope ?? "global",
        path: args?.scope === "project" ? "C:/proj/.codewave/mcp.json" : "C:/u/.codewave/mcp.json",
        json: configReply,
        servers: [],
        effective: effectiveReply,
        issues: [],
      };
    case "mcp_save_config": return { saved: saveAllowed, issues: [] };
    case "mcp_test": return testReply;
    case "mcp_snapshot":
      return { session: "s1", servers: JSON.parse(JSON.stringify(statusReply.map((s) => ({ scope: "global", ...s })))) };
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
  saveAllowed = true;
  projectLoadFails = false;
  statusReply = [];
  effectiveReply = [];
  configReply = MCP_CONFIG;
  testReply = { ok: true, tools: 2, error: null };
  useSessions.setState({ activeKey: null });
});

/** 状态调用次数（刷新按钮「只重读、不重连」与「进页 refetch」都靠它断言） */
function statusCalls(): number {
  // mcp_snapshot 带参数（sessionId），记录形如 `mcp_snapshot:{...}`，故按前缀计数
  return calls.filter((c) => c.startsWith("mcp_snapshot")).length;
}

function statusTable(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[data-testid="settings-page"] [data-setting-id="app.mcp_status"]');
}

/** 卡片中的服务器名称、状态与工具计数。 */
function statusRows(): { name: string; state: string; tools: string }[] {
  return Array.from(document.querySelectorAll<HTMLElement>('[data-testid="settings-page"] .mcp-server-card'))
    .map((r) => ({
      name: r.querySelector(".mcp-server-name")?.textContent ?? "",
      state: (r.querySelector(".ant-tag")?.textContent ?? "").trim(),
      tools: Array.from(r.querySelectorAll(".mcp-server-meta span")).find((s) => /工具|过滤/.test(s.textContent ?? ""))?.textContent?.replace(" 个工具", "") ?? "—",
    }));
}

function scopeTabByText(text: string): HTMLElement {
  const el = Array.from(
    document.querySelectorAll<HTMLElement>(".mcp-scope .ant-tabs-tab"),
  ).find((n) => (n.textContent ?? "").trim() === text);
  if (!el) throw new Error(`scope tab not found: ${text}`);
  return el;
}

function buttonByText(text: string): HTMLElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLElement;
}

async function openMcpTab(edit = false, editIndex = 0) {
  // 状态是**会话级**的（连接池按会话可见集 keyed）：没有活跃会话就没有可读的连接状态，
  // 故这里先立一个活跃会话，再打开 MCP 页。
  useSessions.setState({ activeKey: "s1" });
  useSettings.setState({ config: makeConfig(), loaded: true });
  useUi.setState({ settingsOpen: true, settingsTab: "mcp" });
  render(
    <AntApp>
      <SettingsPage />
    </AntApp>,
  );
  await waitFor(() => expect(document.querySelector('[data-setting-id="app.mcp_status"]')).toBeTruthy());
  if (edit && document.querySelector('.mcp-server-card')) {
    const editButtons = Array.from(document.querySelectorAll<HTMLElement>(".mcp-server-card button")).filter((button) => button.textContent?.replace(/\s/g, "") === "编辑配置");
    fireEvent.click(editButtons[editIndex]);
    await waitFor(() => expect(document.querySelector(".mcp-entry")).toBeTruthy());
  }
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
    expect(statusTable()?.textContent).toContain("工具数");
    // 次要说明移入提示，状态表保留核心数据。
    expect(statusTable()?.textContent).not.toContain("连接在打开会话时建立");
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

  it("状态里多出配置里已删的服务器时不显示陈旧行", async () => {
    statusReply = [{ name: "ghost", state: "ready", tools: 3 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "未连接", tools: "—" },
      { name: "web", state: "未连接", tools: "—" },
    ]);
  });

  it("连接失败行：点击整行展开完整错误（含 stderr），再点收起", async () => {
    statusReply = [{ name: "fs", state: { error: "Error: spawn npx ENOENT\n  at child_process" }, tools: 0 }];
    await openMcpTab();

    const row = await waitFor(() => {
      const el = document.querySelector<HTMLElement>('[data-testid="settings-page"] .mcp-server-diagnostic button');
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
    expect(document.querySelector('[data-testid="settings-page"] .mcp-server-diagnostic')).toBeFalsy();
  });

  it("连接中（starting）：显示「连接中」+ spinner，且该行不可点（无错误可展开）", async () => {
    statusReply = [{ name: "fs", state: "starting", tools: 0 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([
      { name: "fs", state: "连接中", tools: "—" },
      { name: "web", state: "未连接", tools: "—" },
    ]);
    expect(document.querySelector('[data-testid="settings-page"] .mcp-server-title .ant-spin')).toBeTruthy();
    expect(document.querySelector('[data-testid="settings-page"] .mcp-server-diagnostic')).toBeFalsy();
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
    const beforeConnect = calls.filter((c) => c.startsWith("mcp_connect")).length;
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    fireEvent.click(btn);

    await waitFor(() => expect(statusCalls()).toBeGreaterThan(before));
    await waitFor(() => expect(statusRows()[0]).toEqual({ name: "fs", state: "已连接", tools: "2" }));
    // 刷新只重读状态：前后 connect_mcp 调用数必须一致（用计数而非「全仓为空」，否则断言恒真）
    expect(calls.filter((c) => c.startsWith("mcp_connect")).length).toBe(beforeConnect);
  });

  it("切到 MCP 页时重读一次状态（mcp:status 事件只在 connect_mcp 后触发，久留会看到陈旧状态）", async () => {
    statusReply = [];
    // 状态是会话级的：没有活跃会话就不会去读（也就没有「陈旧状态」可谈）
    useSessions.setState({ activeKey: "s1" });
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

  it("没有配置服务器、也没有状态记录：保留新建入口", async () => {
    configReply = JSON.stringify({ mcpServers: {} });
    statusReply = [];
    await openMcpTab();

    expect(statusTable()).toBeTruthy();
    expect(document.body.textContent ?? "").toContain("新建");
  });

  it("没配置服务器但有状态记录时不显示陈旧行", async () => {
    configReply = JSON.stringify({ mcpServers: {} });
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    await openMcpTab();

    await waitFor(() => expect(statusTable()).toBeTruthy());
    expect(statusRows()).toEqual([]);
  });

  it("mcp.json 解析失败改用文本兜底编辑：原文可见、保存直存原文（不被空配置覆盖）", async () => {
    configReply = "not-json";
    statusReply = [{ name: "fs", state: "ready", tools: 2 }];
    await openMcpTab();

    // 兜底模式：显示说明文案 + JSON 原文（可直接改），而不是把解析不了的配置吞掉当「没有服务器」
    await waitFor(() => expect(document.body.textContent ?? "").toContain("已回退为原始编辑模式"));
    const ta = await waitFor(() => {
      const el = document.querySelector<HTMLTextAreaElement>('.mcp-json');
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

describe("设置页 MCP 页：作用域切换与临时测试连接", () => {
  it("默认只显示配置概览，编辑保存后收起表单且不展示变量值", async () => {
    configReply = JSON.stringify({ mcpServers: { fs: { command: "npx", env: { TOKEN: "private-value" } } } });
    await openMcpTab();
    expect(statusRows().some((row) => row.name === "fs")).toBe(true);
    expect(document.querySelector(".mcp-entry")).toBeFalsy();
    expect(document.body.textContent).not.toContain("private-value");
    fireEvent.click(buttonByText("编辑配置"));
    expect(document.querySelector(".mcp-entry")).toBeTruthy();
    fireEvent.click(buttonByText("保存并重连"));
    await waitFor(() => expect(document.querySelector(".mcp-entry")).toBeFalsy());
    expect(statusRows().some((row) => row.name === "fs")).toBe(true);
  });

  it("保存被拒时保留编辑表单和输入", async () => {
    await openMcpTab(true);
    const nameInput = document.querySelector<HTMLInputElement>(".mcp-entry input")!;
    fireEvent.change(nameInput, { target: { value: "renamed" } });
    saveAllowed = false;
    fireEvent.click(buttonByText("保存并重连"));
    await waitFor(() => expect(calls.some((c) => c.startsWith("mcp_save_config"))).toBe(true));
    expect(document.querySelector(".mcp-entry")).toBeTruthy();
    expect(document.querySelector<HTMLInputElement>(".mcp-entry input")?.value).toBe("renamed");
  });

  it("默认编辑全局层，提示回显该层 mcp.json 路径", async () => {
    await openMcpTab();
    await waitFor(() => expect(document.querySelector(".mcp-scope-info")?.getAttribute("aria-label")).toContain("C:/u/.codewave/mcp.json"));
    expect(calls.some((c) => c.includes('"scope":"global"'))).toBe(true);
  });

  it("切到项目层会重新拉该层配置（两层都可编辑；同名项目级胜出由后端合并）", async () => {
    await openMcpTab();
    fireEvent.click(scopeTabByText("项目级"));
    await waitFor(() =>
      expect(calls.some((c) => c.includes('"scope":"project"'))).toBe(true),
    );
    await waitFor(() => expect(document.querySelector(".mcp-scope-info")?.getAttribute("aria-label")).toContain("C:/proj/.codewave/mcp.json"));
  });

  it("同名服务器按作用域展示各自的状态与工具说明", async () => {
    statusReply = [
      { name: "fs", scope: "global", state: "ready", tools: 1, tool_details: [{ name: "global_read", description: "全局说明" }] },
      { name: "fs", scope: "project", state: "ready", tools: 1, tool_details: [{ name: "project_read", description: "项目说明" }] },
    ];
    await openMcpTab();
    await waitFor(() => expect(statusTable()?.textContent).toContain("全局说明"));
    expect(statusTable()?.textContent).not.toContain("项目说明");
    fireEvent.click(scopeTabByText("项目级"));
    await waitFor(() => expect(statusTable()?.textContent).toContain("项目说明"));
    expect(statusTable()?.textContent).not.toContain("全局说明");
  });

  it("全局服务器被项目级同名配置覆盖时，卡片不能误操作项目连接", async () => {
    effectiveReply = [{ name: "fs", source: "project", overridden: "global" }];
    statusReply = [
      { name: "fs", scope: "global", state: "stopped", tools: 0 },
      { name: "fs", scope: "project", state: "ready", tools: 1 },
    ];
    await openMcpTab();
    const globalCard = Array.from(document.querySelectorAll<HTMLElement>(".mcp-server-card")).find((card) => card.querySelector(".mcp-server-name")?.textContent === "fs")!;
    expect(globalCard.textContent).toContain("非当前会话生效配置");
    const reconnect = Array.from(globalCard.querySelectorAll<HTMLButtonElement>("button")).find((b) => b.textContent?.includes("重连"))!;
    expect(reconnect.disabled).toBe(true);
    fireEvent.click(reconnect);
    expect(calls.some((c) => c.startsWith("mcp_reconnect"))).toBe(false);

    fireEvent.click(scopeTabByText("项目级"));
    await waitFor(() => expect(statusRows()[0]?.state).toBe("已连接"));
    const projectCard = document.querySelector<HTMLElement>(".mcp-server-card")!;
    expect(projectCard.textContent).not.toContain("非当前会话生效配置");
    const disconnect = Array.from(projectCard.querySelectorAll<HTMLButtonElement>("button")).find((b) => b.textContent?.includes("断开"))!;
    expect(disconnect.disabled).toBe(false);
  });

  it("作用域配置读取失败时保留当前列表和作用域", async () => {
    await openMcpTab();
    projectLoadFails = true;
    fireEvent.click(scopeTabByText("项目级"));
    await waitFor(() => expect(calls.some((c) => c.includes('"scope":"project"'))).toBe(true));
    expect(statusRows().some((row) => row.name === "fs")).toBe(true);
    expect(document.querySelector(".mcp-scope .ant-tabs-tab-active")?.textContent).toContain("用户级");
  });

  it("有未保存改动时禁用作用域切换（避免切层丢掉草稿）", async () => {
    await openMcpTab(true);
    // 改一个字段 → 脏
    const nameInput = document.querySelector<HTMLInputElement>(".mcp-entry input")!;
    fireEvent.change(nameInput, { target: { value: "fs-renamed" } });
    await waitFor(() => {
      const tab = scopeTabByText("项目级");
      expect(tab.classList.contains("ant-tabs-tab-disabled")).toBe(true);
    });
    expect(document.body.textContent).toContain("保存或放弃后才能切换作用域");
  });

  it("测试连接：调 mcp_test（带作用域与名称）并就地显示结果，绝不触发 connect_mcp", async () => {
    await openMcpTab(true);
    const beforeConnect = calls.filter((c) => c.startsWith("mcp_connect")).length;
    fireEvent.click(buttonByText("测试"));

    await waitFor(() => expect(calls.some((c) => c.startsWith("mcp_test"))).toBe(true));
    const call = calls.find((c) => c.startsWith("mcp_test"))!;
    expect(call).toContain('"scope":"global"');
    expect(call).toContain('"name":"fs"');
    await waitFor(() =>
      expect(document.body.textContent).toContain(
        "临时测试：握手成功，发现 2 个工具（未建立正式连接）",
      ),
    );
    // 测试连接不改动正式状态
    expect(calls.filter((c) => c.startsWith("mcp_connect")).length).toBe(beforeConnect);
  });

  it("测试连接失败：就地显示失败原因", async () => {
    testReply = { ok: false, tools: 0, error: { message: "握手超时（30s）" } };
    await openMcpTab(true);
    fireEvent.click(buttonByText("测试"));
    await waitFor(() =>
      expect(document.body.textContent).toContain("临时测试失败：握手超时（30s）"),
    );
  });
});

describe("设置页 MCP 页：args / env / headers 表格", () => {
  /** 换行：表格（textarea 值列）能表达，旧的「一行一条」文本框做不到 */
  const NL = String.fromCharCode(10);
  const CFG = JSON.stringify({
    mcpServers: {
      fs: {
        command: "npx",
        args: ["-y", "D:/我的 项目/dir"],
        env: { A: " padded ", B: "x=y" },
      },
      web: { url: "https://example.com/mcp", headers: { Authorization: "Bearer abc" } },
    },
  });

  /** 某张 server 卡片里的第 n 张表格（stdio：0=参数 1=环境变量；http：0=请求头） */
  function tableOf(_entryIdx: number, tableIdx: number): HTMLElement {
    const entry = document.querySelectorAll(".mcp-entry")[0];
    const t = entry?.querySelectorAll(".mcp-table")[tableIdx];
    if (!t) throw new Error("table not found");
    return t as HTMLElement;
  }

  function rowOf(t: HTMLElement, i: number): Element {
    const row = t.querySelectorAll(".mcp-table-row")[i];
    if (!row) throw new Error("row not found: " + i);
    return row;
  }

  /** 键列（键值表的键 / 请求头名称）——是 `input` */
  function keyCell(row: Element): HTMLInputElement {
    return row.querySelector("input") as HTMLInputElement;
  }

  /** 值列——参数表是 `input`，键值表是单行 `textarea`（textarea 才存得住换行） */
  function valueCell(row: Element): HTMLInputElement | HTMLTextAreaElement {
    return (row.querySelector("textarea") ?? row.querySelector("input")) as
      | HTMLInputElement
      | HTMLTextAreaElement;
  }

  /** 「＋ 添加」按钮 = 表格里最后一个按钮（避开按文本找时的空白差异） */
  function addBtn(t: HTMLElement): HTMLElement {
    const btns = t.querySelectorAll("button");
    return btns[btns.length - 1] as HTMLElement;
  }

  /** 点保存并把落盘的 JSON 解析回草稿（断言端到端无损） */
  async function saveAndParse() {
    fireEvent.click(buttonByText("保存并重连"));
    await waitFor(() => expect(calls.some((c) => c.startsWith("mcp_save_config"))).toBe(true));
    const call = calls.find((c) => c.startsWith("mcp_save_config"))!;
    const args = JSON.parse(call.slice("mcp_save_config:".length));
    const doc = parseMcpDoc(args.json);
    expect(doc).not.toBeNull();
    return doc!;
  }

  it("参数表：一行一个参数，含空格的路径原样保留", async () => {
    configReply = CFG;
    await openMcpTab(true);
    const t = tableOf(0, 0);
    expect(t.querySelectorAll(".mcp-table-row").length).toBe(2);
    expect(valueCell(rowOf(t, 0)).value).toBe("-y");
    expect(valueCell(rowOf(t, 1)).value).toBe("D:/我的 项目/dir");
  });

  it("参数表：可添加 / 删除行，保存后落到 JSON", async () => {
    configReply = CFG;
    await openMcpTab(true);
    const t = tableOf(0, 0);
    fireEvent.click(addBtn(t));
    expect(t.querySelectorAll(".mcp-table-row").length).toBe(3);
    fireEvent.change(valueCell(rowOf(t, 2)), { target: { value: "--verbose" } });
    // 删掉第一行（-y）
    fireEvent.click(rowOf(t, 0).querySelector("button")!);
    expect(t.querySelectorAll(".mcp-table-row").length).toBe(2);

    const doc = await saveAndParse();
    const fs = doc.servers.find((s) => s.name === "fs")!;
    expect(fs.args.map((a) => a.value)).toEqual(["D:/我的 项目/dir", "--verbose"]);
  });

  it("环境变量表：值不裁剪、含 = 原样保留", async () => {
    configReply = CFG;
    await openMcpTab(true);
    const t = tableOf(0, 1);
    expect(keyCell(rowOf(t, 0)).value).toBe("A");
    expect(valueCell(rowOf(t, 0)).value).toBe(" padded ");
    expect(keyCell(rowOf(t, 1)).value).toBe("B");
    expect(valueCell(rowOf(t, 1)).value).toBe("x=y");
  });

  it("环境变量值含换行也能表达（旧的「一行一条」文本框做不到）", async () => {
    configReply = CFG;
    await openMcpTab(true);
    const t = tableOf(0, 1);
    fireEvent.change(valueCell(rowOf(t, 0)), { target: { value: "a" + NL + "b" } });

    const doc = await saveAndParse();
    const fs = doc.servers.find((s) => s.name === "fs")!;
    expect(fs.env.find((r) => r.key === "A")!.value).toBe("a" + NL + "b");
  });

  it("键为空的行在保存时丢弃（点了＋没填的行）", async () => {
    configReply = CFG;
    await openMcpTab(true);
    const t = tableOf(0, 1);
    fireEvent.click(addBtn(t));
    expect(t.querySelectorAll(".mcp-table-row").length).toBe(3);

    const doc = await saveAndParse();
    const fs = doc.servers.find((s) => s.name === "fs")!;
    expect(fs.env.map((r) => r.key)).toEqual(["A", "B"]);
  });

  it("http 分支渲染请求头表格，可增删改并落到 JSON", async () => {
    configReply = CFG;
    await openMcpTab(true, 1);
    const t = tableOf(1, 0);
    expect(keyCell(rowOf(t, 0)).value).toBe("Authorization");
    expect(valueCell(rowOf(t, 0)).value).toBe("Bearer abc");

    fireEvent.click(addBtn(t));
    fireEvent.change(keyCell(rowOf(t, 1)), { target: { value: "X-Trace" } });
    fireEvent.change(valueCell(rowOf(t, 1)), { target: { value: "1" } });
    fireEvent.click(rowOf(t, 0).querySelector("button")!);

    const doc = await saveAndParse();
    const web = doc.servers.find((s) => s.name === "web")!;
    expect(web.headers).toEqual([{ key: "X-Trace", value: "1" }]);
  });
});
