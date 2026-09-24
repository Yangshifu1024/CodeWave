// Composer 工具条信息段折行契约（[docs/composer-toolbar-context-hit-rate](../../../docs/composer-toolbar-context-hit-rate.md) §9）：
// 根因：信息段此前是 nowrap 且没有裁剪边界，左右栏拖宽后这串小字会**溢出绘制**到右侧模型选择器上、完全不可读。
// 取舍：改为在 <wbr> 处折行（完整可见，不截断）；原子段与分隔符都 nowrap；残余溢出由 .ctx-label 的 overflow:hidden 兜住。
// happy-dom 不做布局 → 与 composer.rate.style.test.ts / ask.style.test.ts 同路：CSS 契约读源码，结构契约读 DOM。
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

const label = () => document.querySelector(".ctx-label") as HTMLElement;
const wbrCount = () => label().querySelectorAll("wbr").length;

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

describe("Composer 工具条信息段折行样式契约", () => {
  it("容器不动：控件行不换行，卡片与工具条都不裁切（裁切会切掉流光与 focus 环）", () => {
    expect(ruleBody(".composer-toolbar")).not.toMatch(/flex-wrap/);
    // 卡片是 BorderBeam 负 inset 流光的定位上下文、工具条内按钮有 focus 环：两者加 overflow:hidden 都会切边
    expect(ruleBody(".composer .composer-card")).not.toMatch(/overflow/);
    expect(ruleBody(".composer-toolbar")).not.toMatch(/overflow/);
  });

  it(".ctx-label 去掉 nowrap，拿到收缩能力与裁剪边界（溢出不再画到模型选择器上）", () => {
    const body = ruleBody(".composer-toolbar .ctx-label");
    expect(body).not.toMatch(/white-space:\s*nowrap/);
    expect(body).toMatch(/min-width:\s*0/);
    expect(body).toMatch(/overflow:\s*hidden/);
  });

  it("原子段与分隔符都 nowrap：折行只发生在 <wbr> 处，段内不拆", () => {
    expect(ruleBody(".composer-toolbar .ctx-label .ctx-seg")).toMatch(/white-space:\s*nowrap/);
    expect(ruleBody(".composer-toolbar .ctx-label .ctx-sep")).toMatch(/white-space:\s*nowrap/);
    expect(ruleBody(".composer-toolbar .ctx-label .ctx-hit")).toMatch(/white-space:\s*nowrap/);
    expect(ruleBody(".composer-toolbar .ctx-label .ctx-rate")).toMatch(/white-space:\s*nowrap/);
  });

  it("加固：任何以 .ctx-label 结尾的选择器都不得重新声明 nowrap（含 media query 内）", () => {
    // 去注释后逐条规则扫描：ruleBody 只取首个匹配，抓不到后来追加的高特异性规则
    const bare = appCss.replace(/\/\*[\s\S]*?\*\//g, "");
    const rules = [...bare.matchAll(/([^{}]+)\{([^}]*)\}/g)].filter((m) => /\.ctx-label\s*$/.test(m[1]));
    expect(rules.length).toBeGreaterThan(0);
    for (const [, sel, body] of rules) {
      expect(body, `选择器 ${sel.trim()} 重新引入了 nowrap`).not.toMatch(/white-space:\s*nowrap/);
    }
  });
});

describe("Composer 工具条信息段折行结构契约", () => {
  it("信息段仍是 .toolbar-info 的子节点，且 <wbr> 恰好两个、分别在命中段与速率段之前", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    const el = label();
    expect(el.parentElement?.className).toBe("toolbar-info");

    const brs = Array.from(el.querySelectorAll("wbr"));
    expect(brs).toHaveLength(2);
    // querySelectorAll 按文档序返回 → 用下标比较先后
    const all = Array.from(el.querySelectorAll("*"));
    const idx = (node: Element) => all.indexOf(node);
    const hit = el.querySelector(".ctx-hit") as Element;
    const rate = el.querySelector(".ctx-rate") as Element;
    expect(idx(brs[0])).toBeLessThan(idx(hit)); // 命中段前断行
    expect(idx(brs[1])).toBeGreaterThan(idx(hit));
    expect(idx(brs[1])).toBeLessThan(idx(rate)); // 速率段前断行
  });

  it("文本逐字不变：<wbr> 不产生字符，分隔符仍留在断点之前", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 }, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    expect(label().textContent).toBe("上下文 60%（76.8k / 128k · 阈 60%） · 命中 50% · 18.2 tok/s");
  });

  it("半段：只有命中段时恰一个断点，且不残留尾随分隔符", () => {
    seed({ breakdown: true, usage: { input: 1000, cacheRead: 500 } });
    render(<AntApp><Composer /></AntApp>);

    expect(label().textContent).toBe("上下文 60%（76.8k / 128k · 阈 60%） · 命中 50%");
    expect(wbrCount()).toBe(1);
  });

  it("半段：只有速率段时恰一个断点，且不残留尾随分隔符", () => {
    seed({ breakdown: true, metrics: METRICS });
    render(<AntApp><Composer /></AntApp>);

    expect(label().textContent).toBe("上下文 60%（76.8k / 128k · 阈 60%） · 18.2 tok/s");
    expect(wbrCount()).toBe(1);
  });

  it("空态不引入断点：仍是「上下文 —」且没有 <wbr>", () => {
    seed();
    render(<AntApp><Composer /></AntApp>);

    expect(label().textContent?.trim()).toBe("上下文 —");
    expect(wbrCount()).toBe(0);
  });
});
