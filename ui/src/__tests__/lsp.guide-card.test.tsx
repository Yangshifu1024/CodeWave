// LSP 引导卡（`lsp:server_missing`）三景 + 打扰控制：installable 调 lsp_install；manual 展示前置条件与官方地址（走 open_url）；
// confirm_enable 调 lsp_enable；「忽略」后同 (language, project_id) 不再弹，同语言重复事件只留一张卡。
// 落点验证：卡渲染在 ChatMessages（消息列表尾部）内，不依赖任何 tool 段。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd"; // App.useApp() 的 message 需与组件同源
import "../i18n"; // ChatMessages mounted standalone must init i18next explicitly
import ChatMessages from "../features/chat/ChatMessages";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";

async function baseInvoke(cmd: string, _args?: any) {
  switch (cmd) {
    case "lsp_install": return "已安装 rust-analyzer";
    case "lsp_enable": return null;
    case "lsp_redetect": return [];
    case "open_url": return null;
    default: return null;
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

/** 经真实事件路由落卡（bindGlobalHandlers 的唯一注册点，等价 AppShell 收帧链路） */
function emitServerMissing(payload: Record<string, unknown>) {
  useRun.getState().bindGlobalHandlers()["lsp:server_missing"](payload);
}

function hintsOf(session: string) {
  return useRun.getState().lspGuide[session];
}

/** antd inserts a space inside two-character buttons: find buttons by whitespace-stripped text */
function buttonByText(text: string): HTMLButtonElement {
  const btn = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === text,
  );
  if (!btn) throw new Error(`button not found: ${text}`);
  return btn as HTMLButtonElement;
}

const RUST_MISSING = {
  language: "rust",
  project_id: "p1",
  kind: "installable",
  server: "rust-analyzer",
  command: "rustup component add rust-analyzer",
  docs_url: null,
  prerequisite: null,
  reason: "未在 PATH 找到 rust-analyzer",
};

const JAVA_MANUAL = {
  language: "java",
  project_id: "p1",
  kind: "manual",
  server: "jdtls",
  command: null,
  docs_url: "https://github.com/eclipse-jdtls/eclipse.jdt.ls",
  prerequisite: "需 JDK 21+",
  reason: "jdtls 无跨平台官方安装方式",
};

const JAVA_CONFIRM = {
  language: "java",
  project_id: "p1",
  kind: "confirm_enable",
  server: "jdtls",
  command: null,
  docs_url: null,
  prerequisite: "需 JDK 21+",
  reason: "jdtls 未安装：Java 语义校验不会自动启动 server",
};

describe("LSP 引导卡（lsp:server_missing 三景）", () => {
  beforeEach(() => {
    useUi.setState({ settingsOpen: false, settingsTab: "appearance" });
    useRun.setState({ tabs: {}, drafts: {}, lspGuide: {} });
    useSessions.setState({
      tabs: [{ key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1", projectId: "p1", createdAt: "2026-09-01T00:00:00Z", prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null } }],
      activeKey: "s1",
      projects: [],
    });
  });

  afterEach(async () => {
    cleanup();
    (await invokeMock()).mockImplementation(baseInvoke);
  });

  it("installable：未找到提示 + 安装命令；点「安装」调 lsp_install 并自收", async () => {
    render(
      <AntApp>
        <ChatMessages />
      </AntApp>,
    );
    emitServerMissing(RUST_MISSING);
    await waitFor(() => expect(screen.getByText("未找到 rust-analyzer")).toBeTruthy());
    expect(screen.getByText(/安装命令：rustup component add rust-analyzer/)).toBeTruthy();
    fireEvent.click(buttonByText("安装"));
    await waitFor(async () => expect(await invokedWith("lsp_install", { language: "rust" })).toBe(true));
    // 装完即自收 + 登记去重键（重探测一并触发）
    await waitFor(() => expect(screen.queryByText("未找到 rust-analyzer")).toBeNull());
    expect(hintsOf("s1").hints.length).toBe(0);
    expect(hintsOf("s1").dismissed).toContain("rust:p1");
    expect(await invokedWith("lsp_redetect")).toBe(true);
  });

  it("manual：前置条件 + 官方地址按钮走 open_url（不用 <a href> 直跳）", async () => {
    render(
      <AntApp>
        <ChatMessages />
      </AntApp>,
    );
    emitServerMissing(JAVA_MANUAL);
    await waitFor(() => expect(screen.getByText("需要手动安装 jdtls")).toBeTruthy());
    expect(screen.getByText("前置条件：需 JDK 21+")).toBeTruthy();
    expect(document.querySelector("a[href^='https://github.com/eclipse-jdtls']")).toBeNull();
    fireEvent.click(buttonByText("官方安装说明"));
    await waitFor(async () =>
      expect(await invokedWith("open_url", { url: "https://github.com/eclipse-jdtls/eclipse.jdt.ls" })).toBe(true),
    );
    expect(screen.getByText("需要手动安装 jdtls")).toBeTruthy(); // 打开文档不自动消失
  });

  it("confirm_enable：写明代价；点「启用」调 lsp_enable", async () => {
    render(
      <AntApp>
        <ChatMessages />
      </AntApp>,
    );
    emitServerMissing(JAVA_CONFIRM);
    await waitFor(() => expect(screen.getByText("Java 语义校验默认关闭")).toBeTruthy());
    expect(screen.getByText(/启用后会启动 jdtls 并解析依赖树，可能耗时数分钟、占用 GB 级内存/)).toBeTruthy();
    fireEvent.click(buttonByText("启用"));
    await waitFor(async () => expect(await invokedWith("lsp_enable", { language: "java" })).toBe(true));
    await waitFor(() => expect(screen.queryByText("Java 语义校验默认关闭")).toBeNull());
  });

  it("忽略后同 (language, project_id) 不再弹；同语言重复事件只留一张卡", async () => {
    render(
      <AntApp>
        <ChatMessages />
      </AntApp>,
    );
    emitServerMissing(RUST_MISSING);
    emitServerMissing(RUST_MISSING); // 重复事件：同会话同语言只留一张
    await waitFor(() => expect(screen.getAllByText("未找到 rust-analyzer").length).toBe(1));
    fireEvent.click(buttonByText("忽略"));
    await waitFor(() => expect(screen.queryByText("未找到 rust-analyzer")).toBeNull());
    emitServerMissing(RUST_MISSING); // 忽略过的不再弹
    await new Promise((r) => setTimeout(r, 30));
    expect(screen.queryByText("未找到 rust-analyzer")).toBeNull();
    // 另一种语言（不同去重键）仍可弹
    emitServerMissing({ ...RUST_MISSING, language: "python", project_id: null, server: "pyright", command: "npx -y pyright-langserver --stdio" });
    await waitFor(() => expect(screen.getByText("未找到 pyright")).toBeTruthy());
  });

  it("临时会话（project_id = null）与项目会话去重键互不干扰", async () => {
    render(
      <AntApp>
        <ChatMessages />
      </AntApp>,
    );
    emitServerMissing({ ...RUST_MISSING, project_id: null, server: "rust-analyzer" });
    await waitFor(() => expect(screen.getByText("未找到 rust-analyzer")).toBeTruthy());
    fireEvent.click(buttonByText("忽略"));
    emitServerMissing(RUST_MISSING); // 项目会话（p1）键不同 → 允许再弹
    await waitFor(() => expect(screen.getByText("未找到 rust-analyzer")).toBeTruthy());
    expect(hintsOf("s1").dismissed).toEqual(["rust:"]);
  });
});
