// TokenStatsModal 摘要行（[docs/arithmetic-fixes-batch](../../../docs/arithmetic-fixes-batch.md)）：
// 「最常用模型」必须展示用户配置的 wire 模型名（ProviderModel.model），而不是 by_model 的内部 model_id；
// 已删/未知模型无显示名时回退截断 id，保证仍可辨认。
import { describe, it, expect, vi, beforeAll, afterEach } from "vitest";
import { render, screen, cleanup, waitFor } from "@testing-library/react";
import { App as AntApp } from "antd";
import "../i18n"; // Modal mounted standalone must init i18next explicitly

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

vi.mock("../ipc/client", () => ({
  ipc: {
    getTokenStats: vi.fn(async () => []),
  },
}));

import TokenStatsModal from "../features/panels/TokenStatsModal";
import { ipc } from "../ipc/client";
import { useSettings } from "../stores/settings";
import { useUi } from "../stores/ui";
import type { ConfigState, DailyStats, ProviderConfig } from "../ipc/types";

const AGG = { input: 1000, output: 500, cache_read: 0, cache_write: 0, runs: 1 };

/** 聚合记录的完整形态：四把耗时字段可选（旧记录没有）——[docs/composer-token-rate] */
type Agg = typeof AGG & { gen_ms?: number; ttft_ms?: number; ttft_count?: number; steps?: number };

/** 单日统计：`by_model` 供「最常用模型」用，`by_kind` 供总览三项的同域聚合用（可分别指定；
 *  不传 by_kind 即旧文件形态——总览据此走 `—`）。 */
function makeDays(entries: Record<string, Agg>, byKind?: Record<string, Agg>): DailyStats[] {
  return [
    {
      date: "2026-09-15",
      by_model: entries,
      by_workspace: {},
      ...(byKind ? { by_kind: byKind } : {}),
      total: AGG,
    },
  ];
}

function makeConfig(providers: ProviderConfig[]): ConfigState {
  return {
    schema_version: 2,
    providers,
    active_model_id: providers[0]?.models[0]?.id ?? null,
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
  };
}

function makeProvider(models: ProviderConfig["models"]): ProviderConfig {
  return {
    id: "p1",
    name: "Test Provider",
    api_format: "openai_chat",
    base_url: "https://api.example.com/v1",
    keys: ["***abcd"],
    models,
    headers: [],
  };
}

const MODEL = {
  id: "eb8dcd57-1234-4abc-9def-0123456789ab",
  model: "glm-4.7",
  max_tokens: 32768,
  context_window: 128000,
  reasoning_effort: null,
  vision: true,
  video: false,
};

beforeAll(() => {
  vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays({ [MODEL.id]: AGG }));
});

afterEach(() => {
  cleanup();
  useSettings.setState({ config: null, loaded: false });
  useUi.setState({ statsOpen: false });
});

function renderModal() {
  return render(
    <AntApp>
      <TokenStatsModal />
    </AntApp>,
  );
}

describe("TokenStatsModal 摘要行", () => {
  it("最常用模型展示 wire 模型名，不展示内部 model_id", async () => {
    useSettings.setState({ config: makeConfig([makeProvider([MODEL])]), loaded: true });
    renderModal();

    await waitFor(() => {
      expect(screen.getByText(/最常用模型/)).toBeTruthy();
    });
    const line = screen.getByText(/最常用模型/).textContent ?? "";
    expect(line).toContain("glm-4.7");
    expect(line).not.toContain("eb8dcd57");
  });

  it("模型已删/未知时回退为截断 id", async () => {
    useSettings.setState({ config: makeConfig([makeProvider([])]), loaded: true });
    renderModal();

    await waitFor(() => {
      expect(screen.getByText(/最常用模型/)).toBeTruthy();
    });
    const line = screen.getByText(/最常用模型/).textContent ?? "";
    expect(line).toContain("eb8dcd57…");
    expect(line).not.toContain("glm-4.7");
  });
});

// 总览三项（[docs/composer-token-rate](../../docs/composer-token-rate.md)）：加权且**分子分母同域**——
// 只在带耗时数据的记录子集内聚合（否则「分子含全部 output、分母只含部分耗时」会算出虚高速率）。
// 聚合域取 **by_kind**（来源桶）：by_model 把 sub/compact/title/task 与 main 混进同一个 model 桶，
// 桶级 `gen_ms > 0` 过滤剔不掉桶内那部分不带计时的子集（见下一个用例）。
describe("TokenStatsModal 总览三项（速率 / 均步耗时 / 平均 TTFT）", () => {
  /** 总览三项所在的那一行（唯一包含「平均生成速率」的摘要行） */
  async function overviewLine(): Promise<string> {
    const el = await waitFor(() => screen.getByText(/平均生成速率/));
    return el.textContent ?? "";
  }

  it("加权计算，且不带计时的来源桶整条排除（分子分母同域）", async () => {
    // 旧记录（早于耗时字段）带 100 万 output 却没有任何耗时字段：若把它计入分子，速率会飆到 ~16677 tok/s
    const LEGACY = { input: 10, output: 1_000_000, cache_read: 0, cache_write: 0, runs: 1 };
    useSettings.setState({ config: makeConfig([makeProvider([MODEL])]), loaded: true });
    vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays(
      {
        [MODEL.id]: { ...AGG, output: 600, gen_ms: 60_000, ttft_ms: 800, ttft_count: 2, steps: 3 },
        "legacy-model": LEGACY,
      },
      { main: { ...AGG, output: 600, gen_ms: 60_000, ttft_ms: 800, ttft_count: 2, steps: 3 } },
    ));
    renderModal();

    const text = await overviewLine();
    expect(text).toContain("10.0 tok/s"); // 600 tokens / 60 s
    expect(text).not.toContain("1667"); // 未把旧记录的 output 计入分子
    expect(text).toContain("20.0 s"); // 60_000 ms / 3 步
    expect(text).toContain("400 ms"); // 800 ms / 2 个 TTFT 样本
    expect(text).not.toMatch(/NaN|Infinity/);
    // 非目标回归：全弹窗只有总览这一处 tok/s（分组维度不加速度列）
    expect((document.body.textContent ?? "").split("tok/s").length - 1).toBe(1);
  });

  it("同一 model 下混 kind：同 model 桶里的 sub（无计时）不得进分子（by_model 桶级过滤剔不掉它）", async () => {
    // 现实形态：by_model 把 main 与 sub 的 output 合进同一个 model 桶（此处 1200 + 100 万），而 sub 的记录不带 gen_ms。
    // 若按 model 桶过滤，桶级 `gen_ms > 0` 成立而 output 里含 sub 的 100 万 → 虚高到 ~16687 tok/s（需求 §7「聚合虚高」）。
    const MAIN = { ...AGG, output: 1200, gen_ms: 60_000, ttft_ms: 800, ttft_count: 2, steps: 3 };
    const SUB = { input: 10, output: 1_000_000, cache_read: 0, cache_write: 0, runs: 1 };
    useSettings.setState({ config: makeConfig([makeProvider([MODEL])]), loaded: true });
    vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays(
      { [MODEL.id]: { ...MAIN, output: 1_001_200 } },
      { main: MAIN, sub: SUB },
    ));
    renderModal();

    const text = await overviewLine();
    expect(text).toContain("20.0 tok/s"); // 1200 tokens / 60 s —— 只用 main 桶
    expect(text).not.toContain("1668"); // 未把 sub 的 100 万 output 当作分子
    expect(text).toContain("20.0 s"); // 均步耗时同样只看 main 桶
    expect(text).toContain("400 ms"); // 800 ms / 2 个 TTFT 样本
    expect(text).not.toMatch(/NaN|Infinity/);
    expect((document.body.textContent ?? "").split("tok/s").length - 1).toBe(1);
  });

  it("全缺耗时字段（旧数据：连 by_kind 都没有）→ 三项都是 —，不出 NaN / Infinity", async () => {
    useSettings.setState({ config: makeConfig([makeProvider([MODEL])]), loaded: true });
    vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays({ [MODEL.id]: AGG }));
    const { unmount } = renderModal();

    const text = await overviewLine();
    expect(text.match(/—/g)).toHaveLength(3);
    expect(text).not.toMatch(/NaN|Infinity/);
    expect(text).not.toContain("tok/s");
    unmount();

    // 有 by_kind 但每个来源桶都不带耗时（例：当天只有子代理/命名记录）→ 同样三项都是 —，绝不把无耗时的 output 当分子
    vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays(
      { [MODEL.id]: AGG },
      { sub: { ...AGG, output: 900_000 }, title: { ...AGG, output: 40 } },
    ));
    renderModal();
    const text2 = await overviewLine();
    expect(text2.match(/—/g)).toHaveLength(3);
    expect(text2).not.toContain("tok/s");
  });

  it("有耗时但步数/样本数为 0 → 对应项单独显示 —（分母不为 0 的项照常出值）", async () => {
    useSettings.setState({ config: makeConfig([makeProvider([MODEL])]), loaded: true });
    const TIMED = { ...AGG, output: 1200, gen_ms: 60_000, ttft_ms: 0, ttft_count: 0, steps: 0 };
    vi.mocked(ipc.getTokenStats).mockResolvedValue(makeDays({ [MODEL.id]: TIMED }, { main: TIMED }));
    renderModal();

    const text = await overviewLine();
    expect(text).toContain("20.0 tok/s"); // 1200 / 60 s
    expect(text.match(/—/g)).toHaveLength(2); // 均步耗时与平均 TTFT 无样本
    expect(text).not.toMatch(/NaN|Infinity/);
  });
});

