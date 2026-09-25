// Composer 工具条速率段（[docs/composer-token-rate](../../docs/composer-token-rate.md)）：
// 上下文/命中挪到进度圈 hover 的 Popover 后，速率独立住在 .toolbar-info 直接子级。
// 「在跑」点随 running 显隐而终值保留；无数据（无 runMetrics / 本轮还没拿到耗时）整段不渲染。
// Composer 独立挂载（与 composer.fileref.test.tsx 同一套环境种子）。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, act, cleanup, waitFor } from "@testing-library/react";
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
import type { RunMetrics } from "../stores/run.types";

const MODEL_BASE = {
  id: "m1", model: "test-model", max_tokens: 32768, context_window: 128000,
  reasoning_effort: null, vision: true, video: false,
};

/** 本轮 1820 output / 100 s 生成 / 3 步 / 首步 TTFT 420 ms / 工具等待 5 s → 18.2 tok/s */
const METRICS: RunMetrics = { output: 1820, genMs: 100_000, steps: 3, ttftMs: 420, toolMs: 5000 };

/** 环境种子：breakdown + usage（openai 语义下命中 500/1000 = 50%）+ 可选的 runMetrics。 */
function seed(opts: {
  breakdown?: boolean;
  usage?: { input: number; cacheRead: number };
  metrics?: RunMetrics;
  running?: boolean;
} = {}) {
  const { breakdown = false, usage, metrics, running = false } = opts;
  useSettings.setState({
    config: {
      schema_version: 2,
      providers: [{
        id: "p1", name: "Test Provider", api_format: "openai_chat" as const,
        base_url: "https://api.example.com/v1", keys: ["***abcd"],
        models: [{ ...MODEL_BASE }], headers: [],
      }],
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
      items: [], running, streamGen: 0, ask: null,
      breakdown: breakdown
        ? {
            system_tokens: 1000, history_tokens: 2000, tool_results_tokens: 500,
            tool_schema_tokens: 500, total_tokens: 76800, context_window: 128000, ratio: 0.6,
          }
        : null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
      usage: usage ? { input: usage.input, output: 10, cacheRead: usage.cacheRead, cacheWrite: 0 } : undefined,
      ...(metrics ? { runMetrics: metrics } : {}),
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

const toolbarInfo = () => document.querySelector(".toolbar-info") as HTMLElement;
const rateEl = () => document.querySelector(".composer-toolbar .toolbar-info .ctx-rate") as HTMLElement | null;

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
});

describe("Composer 工具条速率段（搬到 .toolbar-info 直接子级）", () => {
  it("有 breakdown + 命中 + 速率：速率渲染在 .toolbar-info 下，文本逐字不变", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    const rate = rateEl();
    expect(rate).toBeTruthy();
    expect(rate?.parentElement?.className).toBe("toolbar-info");
    expect(rate?.textContent).toContain("18.2 tok/s");
    // 全局唯一（速率段不在 popover 内重复）
    expect(document.querySelectorAll(".composer-toolbar .ctx-rate").length).toBe(1);
  });

  it("tooltip 五项：本轮平均速率 / 首步 TTFT（含思考）/ 输出 tokens / 生成耗时（不含重试等待）/ 工具等待合计", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    const title = rateEl()?.getAttribute("title") ?? "";
    const lines = title.split("\n");
    expect(lines).toHaveLength(5);
    expect(title).toContain("本轮生成速率: 18.2 tok/s");
    expect(title).toContain("首步 TTFT（首个输出增量，含思考）: 420 ms");
    expect(title).toContain("输出 tokens: 1.8k");
    expect(title).toContain("生成耗时（不含重试等待）: 100.0 s");
    expect(title).toContain("工具等待合计: 5.0 s");
  });

  it("单步且没等过工具：tooltip 不出「工具等待」行", () => {
    seed({
      breakdown: true,
      usage: { input: 1000, cacheRead: 500 },
      metrics: { output: 200, genMs: 10_000, steps: 1, ttftMs: 300, toolMs: 0 },
    });
    render(<AntApp><Composer /></AntApp>);

    const title = rateEl()?.getAttribute("title") ?? "";
    expect(title.split("\n")).toHaveLength(4);
    expect(title).not.toContain("工具等待");
    expect(rateEl()?.textContent).toContain("20.0 tok/s");
  });

  it("运行中数字旁有「在跑」点：结束后点消失而终值保留", async () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS, running: true });
    render(<AntApp><Composer /></AntApp>);

    expect(document.querySelector(".composer-toolbar .toolbar-info .rate-dot")).toBeTruthy();
    expect(rateEl()?.textContent).toContain("18.2 tok/s");

    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"].running = false;
      });
    });
    await waitFor(() => expect(document.querySelector(".composer-toolbar .toolbar-info .rate-dot")).toBeFalsy());
    expect(rateEl()?.textContent).toContain("18.2 tok/s"); // 终值保留（不随 running 一起消失）
  });

  it("无 breakdown（空态）时 .toolbar-info 为空，不出现 tok/s", () => {
    seed(); // 无 breakdown / 无 usage / 无 runMetrics
    const { unmount } = render(<AntApp><Composer /></AntApp>);
    expect(rateEl()).toBeFalsy();
    expect(toolbarInfo()?.textContent?.trim() ?? "").toBe("");
    unmount();

    // 有明细与命中段、但本轮还没收到带耗时的 usage 帧（genMs = 0）→ 仍然不渲染速率段
    seed({
      breakdown: true,
      usage: { input: 1000, cacheRead: 500 },
      metrics: { output: 0, genMs: 0, steps: 0, ttftMs: null, toolMs: 0 },
    });
    render(<AntApp><Composer /></AntApp>);
    expect(rateEl()).toBeFalsy();
  });

  it("本轮已有耗时但无输出（分子为 0）时也不显示 0 tok/s", () => {
    seed({
      breakdown: true,
      usage: { input: 1000, cacheRead: 500 },
      metrics: { output: 0, genMs: 8000, steps: 1, ttftMs: 200, toolMs: 0 },
    });
    render(<AntApp><Composer /></AntApp>);
    expect(rateEl()).toBeFalsy();
  });
});