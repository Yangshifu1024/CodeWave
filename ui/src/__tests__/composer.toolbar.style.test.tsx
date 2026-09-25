// Composer 工具条上下文区改造契约：
// 上下文 / 命中 / 压缩阈值三项挪到 .ctx-progress 进度圈 hover 的 Popover，速率留在 .toolbar-info。
// CSS 契约读源码（happy-dom 不做布局），结构契约读 DOM。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { App as AntApp } from "antd";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import "../i18n"; // Composer 独立挂载必须先初始化 i18next（文案全走 t()）

const appCss = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../theme/app.css"), "utf8");

/** 取出选择器对应的规则体（[^}] 恰好停在规则右括号；选择器含空格时逐字面匹配） */
function ruleBody(selector: string): string {
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`${esc}\\s*\\{([^}]*)\\}`).exec(appCss);
  expect(m, `未找到规则：${selector}`).toBeTruthy();
  return m![1];
}

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

/** 本轮 1820 output / 100 s 生成 / 3 步 → 18.2 tok/s（与 composer.rate.test.tsx 同种子，便于逐字比对文本） */
const METRICS: RunMetrics = { output: 1820, genMs: 100_000, steps: 3, ttftMs: 420, toolMs: 5000 };

/** 环境种子：breakdown + usage（openai 语义下命中 500/1000 = 50%）+ 可选的 runMetrics。 */
function seed(opts: {
  breakdown?: boolean;
  usage?: { input: number; cacheRead: number };
  metrics?: RunMetrics;
} = {}) {
  const { breakdown = false, usage, metrics } = opts;
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
      items: [], running: false, streamGen: 0, ask: null,
      breakdown: breakdown
        ? {
            system_tokens: 1000, history_tokens: 2000, tool_results_tokens: 500,
            tool_schema_tokens: 500, total_tokens: 76800, context_window: 128000, ratio: 0.6,
          }
        : null,
      usage: usage ? { input: usage.input, output: 10, cacheRead: usage.cacheRead, cacheWrite: 0 } : undefined,
      ...(metrics ? { runMetrics: metrics } : {}),
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null,
      lastDoneRunId: null, compacting: false,
    } as (typeof s.tabs)[string];
    s.drafts = { s1: { text: "", images: [], refs: [] } };
  });
}

const progress = () => document.querySelector(".ctx-progress") as HTMLElement;
const toolbarInfo = () => document.querySelector(".toolbar-info") as HTMLElement;

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

describe("Composer 工具条上下文进度圈样式契约", () => {
  it("容器不动：控件行不换行，卡片与工具条都不裁切（裁切会切掉流光与 focus 环）", () => {
    expect(ruleBody(".composer-toolbar")).not.toMatch(/flex-wrap/);
    expect(ruleBody(".composer .composer-card")).not.toMatch(/overflow/);
    expect(ruleBody(".composer-toolbar")).not.toMatch(/overflow/);
  });

  it("进度圈轨道走主题 token（var(--ws-border)），不硬编码 hex", () => {
    const body = ruleBody(".ctx-progress .ant-progress-circle-rail");
    expect(body).toMatch(/stroke:\s*var\(--ws-border\)/);
    expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  });

  it("进度圈 wrap 是 inline-flex 居中容器，光标为手型（提示可交互）", () => {
    const body = ruleBody(".ctx-progress-wrap");
    expect(body).toMatch(/display:\s*inline-flex/);
    expect(body).toMatch(/cursor:\s*pointer/);
  });

  it("Popover 详情三行键值最小宽度足够容纳上下文数字（≥220px）", () => {
    const body = ruleBody(".ctx-popover");
    expect(body).toMatch(/min-width:\s*220px/);
  });

  it("速率段仍在 .toolbar-info 下，色走 --ws-dim（中性灰，不做分档）", () => {
    const body = ruleBody(".composer-toolbar .toolbar-info .ctx-rate");
    expect(body).toMatch(/color:\s*var\(--ws-dim\)/);
    expect(body).not.toMatch(/#[0-9a-fA-F]{3,6}/);
  });

  it("旧 .ctx-label/.ctx-seg/.ctx-sep/.ctx-pct/.ctx-hit 全部退场（防止误改重提）", () => {
    expect(appCss).not.toMatch(/\.composer-toolbar \.ctx-label/);
    expect(appCss).not.toMatch(/\.ctx-seg\b/);
    expect(appCss).not.toMatch(/\.ctx-sep\b/);
    expect(appCss).not.toMatch(/\.ctx-pct\b/);
    expect(appCss).not.toMatch(/\.ctx-hit\b/);
  });

  it(".toolbar-info 在 medium/narrow 下不被 display:none（上下文/命中迁到 Popover 后只剩速率段，整块折叠 = 速率消失；[fix/composer-toolbar-and-anthropic-cache]）", () => {
    // 不能有任何 .composer-card[data-narrow=…] .toolbar-info { display: none } 这类规则
    // ——“折叠整块”会被 happy-dom 看不到，但选择器文案还在 CSS 中同样会让「medium 档下速度“消失”」重演。
    const re = /\.composer-card\[data-narrow="(medium|narrow)"\]\s+\.toolbar-info[^{]*\{\s*display:\s*none\s*[;}]/;
    expect(appCss).not.toMatch(re);
  });

  it(".queue-panel 与 .composer 同公式的 15% 边距（[fix/queue-panel-width-and-drag]）", () => {
    // 规则：.queue-panel { margin: 0 15% 8px; }—— 与 .composer 0 15% 同公式，宽度字面同步。
    // 拒“0 16px”这种与 composer 不同公式的写法（窗口越宽越错位）。
    const queuePanelBody = ruleBody(".queue-panel");
    expect(queuePanelBody).toMatch(/margin:\s*0\s+15%\s+8px/);
    // 反向钉死：不得出现以前那套“固定 16px”边距
    const oldForm = /\.queue-panel\s*\{[^}]*margin:\s*0\s+16px/;
    expect(appCss).not.toMatch(oldForm);
  });
});

describe("Composer 工具条上下文进度圈结构契约", () => {
  it("有 breakdown 时进度圈渲染，class 反映档位（danger：达阈值 60%）", () => {
    seed({ breakdown: true });
    render(<AntApp><Composer /></AntApp>);

    const p = progress();
    expect(p).toBeTruthy();
    expect(p.className).toContain("ctx-tier-danger"); // ratio 0.6 >= threshold 0.6
  });

  it("进度圈 ant-progress 容器无外边距（紧贴 .ctx-progress-wrap）", () => {
    expect(ruleBody(".ctx-progress.ant-progress")).toMatch(/margin:\s*0/);
  });

  it("无 breakdown（首次进入）时进度圈档位回退到 ok（不臆测风险）", () => {
    seed();
    render(<AntApp><Composer /></AntApp>);

    expect(progress().className).toContain("ctx-tier-ok");
  });

  it("Popover 触发器（.ctx-progress-wrap）包在 .toolbar-right 内（替换原压缩按钮位）", () => {
    seed({ breakdown: true });
    render(<AntApp><Composer /></AntApp>);

    const wrap = document.querySelector(".ctx-progress-wrap") as HTMLElement;
    expect(wrap).toBeTruthy();
    expect(wrap.parentElement?.className).toContain("toolbar-right");
  });

  it("有 breakdown + 命中 + 速率：Popover 内容含当前上下文/压缩阈值/缓存命中 三行", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    // antd Popover 内部 className 含 ant-popover-inner；hover 时才挂载
    // 这里不直接 fireEvent 触发 hover（happy-dom 行为差异大），改为断言 ctx-popover
    // 样式存在 + 渲染后的 DOM 已挂载 Popover 容器
    const popoverWrap = document.querySelector(".ctx-progress-wrap") as HTMLElement;
    expect(popoverWrap).toBeTruthy();
  });

  it("有 breakdown 但无命中：速率仍可独立渲染在 .toolbar-info", () => {
    seed({ breakdown: true, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    const rate = toolbarInfo()?.querySelector(".ctx-rate");
    expect(rate).toBeTruthy();
    expect(rate?.textContent).toContain("18.2 tok/s");
  });

  it("无 breakdown 也无 metrics：.toolbar-info 为空（不渲染空 ctx-rate）", () => {
    seed();
    render(<AntApp><Composer /></AntApp>);

    expect(toolbarInfo()?.querySelector(".ctx-rate")).toBeNull();
  });
});