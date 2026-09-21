// 文件引用入口（[docs/composer-file-ref-chips](../../../docs/composer-file-ref-chips.md)）：
// 附件按钮选文件 → 非图片进草稿 refs 并以 chip 展示（正文里不再出现路径）；图片走附件通道；
// 项目外路径先弹放行确认框；发送前一刻才把引用合成 `@路径` 追加到正文末尾。
// Composer 独立挂载（不依赖 AppShell），与 composer.paste 同一套环境种子。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, act, within } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Composer 独立挂载必须先初始化 i18next（文案全走 t()）

const ipcMock = vi.hoisted(() => ({
  selectDocumentFiles: vi.fn(async () => [] as string[]),
  checkExternalPath: vi.fn(async (_sid: string, _p: string) => ({ inside: true, dir: "", ref: "" })),
  allowExternalDir: vi.fn(async () => [] as string[]),
  readWorkspaceFileBase64: vi.fn(async () => ({ path: "", size: 0, content: "" })),
  listSkills: vi.fn(async () => []),
  searchWorkspacePaths: vi.fn(async (_sid: string, _q: string, _limit?: number) => [] as string[]),
  compactSession: vi.fn(async () => null),
  startChat: vi.fn(async (_sid: string, _text: string, _images: { mime: string; data: string }[], _ch?: unknown) => "ok"),
  cancelRun: vi.fn(async () => null),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import Composer from "../features/chat/Composer";
import { useSessions } from "../stores/sessions";
import { useRun } from "../stores/run";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

function seedEnv() {
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [
        {
          id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
          base_url: "https://api.example.com/v1", keys: ["***abcd"],
          models: [{ ...MODEL_BASE }],
          headers: [],
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
    },
    loaded: true,
  });
  useSessions.setState({
    tabs: [{
      key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "冒烟会话",
      projectId: null, createdAt: "2026-08-30T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: "s1",
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null,
      compacting: false,
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

/** 打开「+」菜单并点「添加附件」；选文件的行为由 ipc.selectDocumentFiles 的 mock 决定。 */
async function clickAttach() {
  fireEvent.click(document.querySelector(".toolbar-left .ant-btn") as HTMLElement);
  const item = await waitFor(() => {
    const el = Array.from(document.querySelectorAll(".ant-dropdown-menu-item")).find((o) =>
      (o.textContent ?? "").includes("附件"),
    );
    expect(el).toBeTruthy();
    return el as HTMLElement;
  });
  fireEvent.click(item);
}

const draftText = () => useRun.getState().drafts["s1"]?.text ?? "";
const draftRefs = () => useRun.getState().drafts["s1"]?.refs ?? [];
const refChips = () => Array.from(document.querySelectorAll(".ref-chip")) as HTMLElement[];

/** 按去空白后的文本找按钮：antd 会在两个字的按钮里插空格（「取 消」）。 */
function buttonByText(label: string): HTMLElement {
  const el = Array.from(document.querySelectorAll("button")).find(
    (b) => (b.textContent ?? "").replace(/\s/g, "") === label,
  );
  expect(el, `按钮「${label}」未找到`).toBeTruthy();
  return el as HTMLElement;
}

/** 在输入框里写正文（chip 不在 textarea 里，正文只放人写的内容）。
 *  用 placeholder 定位：antd TextArea 的 autoSize 会在 DOM 里另放一个测量用 textarea，
 *  直接 querySelector("textarea") 可能拿到那个非受控的替身。 */
function typeBody(v: string) {
  const ta = screen.getByPlaceholderText(/CodeWave/) as HTMLTextAreaElement;
  fireEvent.change(ta, { target: { value: v } });
}

beforeAll(() => {
  Element.prototype.scrollTo = (Element.prototype as any).scrollTo ?? (() => {});
});

afterEach(() => {
  cleanup();
  useUi.setState({ settingsOpen: false, tasksOpen: false, statsOpen: false, rbTab: "info" });
  useSessions.setState({ tabs: [], activeKey: null, projects: [], sessions: [] });
  useSettings.setState({ config: null, loaded: false });
  useRun.setState({ tabs: {}, drafts: {} });
  vi.clearAllMocks();
  ipcMock.selectDocumentFiles.mockResolvedValue([]);
  ipcMock.checkExternalPath.mockImplementation(async () => ({ inside: true, dir: "", ref: "" }));
});

describe("Composer 文件引用入口（chip）", () => {
  it("选中非图片文件：正文不出现路径，改以引用 chip 展示", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/report.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "report.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(refChips().length).toBe(1));
    const chip = refChips()[0];
    expect(chip.getAttribute("title")).toBe("report.xlsx"); // 悬停看完整引用路径
    expect(within(chip).getByText("report.xlsx")).toBeTruthy(); // 只显示文件名
    expect(within(chip).getByRole("button")).toBeTruthy(); // × 可移除
    expect(draftRefs()).toEqual(["report.xlsx"]);
    expect(draftText()).toBe(""); // 正文不被路径污染（改造前的痛点）
    expect(document.querySelector(".attach-chip img")).toBeFalsy(); // 不产生图片缩略图
  });

  it("发送时把引用合成 `@路径` 追加到正文末尾（模型所见与改造前一致），发送后清空", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/report.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "report.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(draftRefs()).toEqual(["report.xlsx"]));
    typeBody("看一下");
    fireEvent.click(document.querySelector(".send-btn") as HTMLElement);

    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalled());
    expect(ipcMock.startChat.mock.calls[0][1]).toBe("看一下 @report.xlsx");
    await waitFor(() => expect(draftRefs()).toEqual([]));
    expect(draftText()).toBe("");
  });

  it("只有引用 chip、没有正文时也能发送", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/report.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "report.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(refChips().length).toBe(1));
    const btn = document.querySelector(".send-btn") as HTMLButtonElement;
    expect(btn.disabled).toBe(false); // hasDraft 把 refs 计入
    fireEvent.click(btn);
    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalled());
    expect(ipcMock.startChat.mock.calls[0][1]).toBe("@report.xlsx");
  });

  it("点 chip 的 × 只移除该引用，正文与其它 chip 不受影响", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/a.xlsx", "/tmp/ws/b.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: true, dir: "/tmp/ws", ref: "a.xlsx" })
      .mockResolvedValueOnce({ inside: true, dir: "/tmp/ws", ref: "b.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(refChips().length).toBe(2));
    typeBody("正文里的 a.xlsx 字样");
    fireEvent.click(within(refChips()[0]).getByRole("button"));

    await waitFor(() => expect(draftRefs()).toEqual(["b.xlsx"]));
    expect(draftText()).toBe("正文里的 a.xlsx 字样"); // 正文一个字都不动
  });

  it("重复选同一文件只保留一个 chip", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/a.xlsx", "/tmp/ws/a.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "a.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(refChips().length).toBe(1));
    expect(draftRefs()).toEqual(["a.xlsx"]);
  });

  it("图片与文件引用同一区域：图片缩略图在前、文件 chip 在后", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/chart.png", "/tmp/ws/report.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: true, dir: "/tmp/ws", ref: "chart.png" })
      .mockResolvedValueOnce({ inside: true, dir: "/tmp/ws", ref: "report.xlsx" });
    ipcMock.readWorkspaceFileBase64.mockResolvedValue({ path: "chart.png", size: 3, content: "aGk=" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(refChips().length).toBe(1));
    const row = document.querySelector(".composer-attachments") as HTMLElement;
    const chips = Array.from(row.querySelectorAll(".attach-chip")) as HTMLElement[];
    expect(chips.length).toBe(2);
    expect(chips[0].querySelector("img")).toBeTruthy(); // 图片在前
    expect(chips[1].classList.contains("ref-chip")).toBe(true); // 文件在后
  });

  it("队列条目「编辑」回填：文本里的 @路径 解析回引用 chip，正文只剩人写的部分", async () => {
    seedEnv();
    render(<AntApp><Composer /></AntApp>);
    // 队列条目文本是发送时合成过的（含末尾 `@路径`），模拟点「编辑」
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"]!.draftFromQueue = { text: "看一下 @/elsewhere/a.xlsx", images: [] };
      });
    });

    await waitFor(() => expect(draftRefs()).toEqual(["/elsewhere/a.xlsx"]));
    expect(draftText()).toBe("看一下");
    expect(refChips().length).toBe(1);
    expect(refChips()[0].getAttribute("title")).toBe("/elsewhere/a.xlsx");
    // 再发送：合成回与入队时逐字节一致的文本
    fireEvent.click(document.querySelector(".send-btn") as HTMLElement);
    await waitFor(() => expect(ipcMock.startChat).toHaveBeenCalled());
    expect(ipcMock.startChat.mock.calls[0][1]).toBe("看一下 @/elsewhere/a.xlsx");
  });

  it("@ 提及选中文件：同样进引用 chip，正文不留路径", async () => {
    seedEnv();
    ipcMock.searchWorkspacePaths.mockResolvedValue(["src/a.ts"]);
    render(<AntApp><Composer /></AntApp>);

    typeBody("@a");
    const item = await waitFor(() => {
      const el = Array.from(document.querySelectorAll(".menu-item")).find((o) =>
        (o.textContent ?? "").includes("src/a.ts"),
      );
      expect(el).toBeTruthy();
      return el as HTMLElement;
    });
    fireEvent.click(item);

    await waitFor(() => expect(draftRefs()).toEqual(["src/a.ts"]));
    expect(draftText()).toBe(""); // 正在输入的 `@a` 片段被抹掉，正文不含路径
    expect(refChips()[0].getAttribute("title")).toBe("src/a.ts");
  });

  it("用户消息「修改」（ws:composer-fill）：旧引用被覆盖，正文里的 @路径 抽回 chip", async () => {
    seedEnv();
    // 草稿里挂着一个未发送的旧引用
    useRun.setState((s) => {
      s.drafts = { s1: { text: "旧草稿", images: [], refs: ["stale.xlsx"] } };
    });
    render(<AntApp><Composer /></AntApp>);
    await waitFor(() => expect(refChips().length).toBe(1));

    // 覆盖成一条之前发送过的消息（末尾带合成过的引用）
    act(() => {
      window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text: "看一下 @a.xlsx" } }));
    });
    await waitFor(() => expect(draftText()).toBe("看一下"));
    expect(draftRefs()).toEqual(["a.xlsx"]); // stale.xlsx 被覆盖，不会偷跟着发出去

    // 覆盖成一条无引用的消息：引用清空（fill 是覆盖语义）
    act(() => {
      window.dispatchEvent(new CustomEvent("ws:composer-fill", { detail: { text: "纯文本" } }));
    });
    await waitFor(() => expect(draftRefs()).toEqual([]));
    expect(draftText()).toBe("纯文本");
    expect(refChips().length).toBe(0);
  });

  it("选中图片路径：读成附件缩略图，不进引用", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/tmp/ws/chart.png"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: true, dir: "/tmp/ws", ref: "chart.png" });
    ipcMock.readWorkspaceFileBase64.mockResolvedValue({ path: "chart.png", size: 3, content: "aGk=" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.querySelectorAll(".attach-chip").length).toBe(1));
    expect(draftText()).toBe("");
    expect(draftRefs()).toEqual([]);
  });

  it("项目外文件：先弹放行确认框；选「仅此一次」→ 不写入项目设置并生成引用 chip", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" })
      .mockResolvedValue({ inside: true, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    // 确认框出现，且列出了所在目录
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    fireEvent.click(screen.getByText("仅此一次"));

    await waitFor(() => expect(draftRefs()).toEqual(["/elsewhere/budget.xlsx"]));
    expect(draftText()).toBe("");
    expect(ipcMock.allowExternalDir).toHaveBeenCalledWith("s1", "/elsewhere", false);
  });

  it("项目外文件选「始终允许这个目录」：persist=true 写进项目设置", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath
      .mockResolvedValueOnce({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" })
      .mockResolvedValue({ inside: true, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    fireEvent.click(screen.getByText("始终允许这个目录"));

    await waitFor(() => expect(ipcMock.allowExternalDir).toHaveBeenCalledWith("s1", "/elsewhere", true));
    await waitFor(() => expect(draftRefs()).toEqual(["/elsewhere/budget.xlsx"]));
  });

  it("在放行确认框里取消：不放行也不生成引用", async () => {
    seedEnv();
    ipcMock.selectDocumentFiles.mockResolvedValue(["/elsewhere/budget.xlsx"]);
    ipcMock.checkExternalPath.mockResolvedValue({ inside: false, dir: "/elsewhere", ref: "/elsewhere/budget.xlsx" });
    render(<AntApp><Composer /></AntApp>);

    await clickAttach();
    await waitFor(() => expect(document.body.textContent ?? "").toContain("/elsewhere"));
    // antd 会在两个字的按钮里插空格（「取 消」），所以按去空白后的文本找
    fireEvent.click(buttonByText("取消"));

    await new Promise((r) => setTimeout(r, 30));
    expect(ipcMock.allowExternalDir).not.toHaveBeenCalled();
    expect(draftText()).toBe("");
    expect(draftRefs()).toEqual([]);
  });
});
