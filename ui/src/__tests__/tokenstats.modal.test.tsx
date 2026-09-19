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

/** 单日统计：只有 by_model 参与摘要行的「最常用模型」计算 */
function makeDays(entries: Record<string, typeof AGG>): DailyStats[] {
  return [
    {
      date: "2026-09-15",
      by_model: entries,
      by_workspace: {},
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
