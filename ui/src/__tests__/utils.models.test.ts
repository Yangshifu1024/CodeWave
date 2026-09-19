// flattenModels / findModel: provider→model flattening consumed by the model menu and guards ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md), [docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect } from "vitest";
import { flattenModels, findModel } from "../utils/models";
import type { ConfigState } from "../ipc/types";

function cfg(): ConfigState {
  return {
    schema_version: 2,
    providers: [
      {
        id: "p1",
        name: "Provider One",
        api_format: "openai_chat",
        base_url: "https://a.example.com",
        keys: ["***aaaa"],
        models: [
          { id: "m1", model: "model-a", max_tokens: 4096, context_window: 128000, reasoning_effort: null, vision: true, video: false },
          { id: "m2", model: "model-b", max_tokens: 32768, context_window: 200000, reasoning_effort: null, vision: false, video: false },
        ],
      },
      {
        id: "p2",
        name: "Provider Two",
        api_format: "anthropic",
        base_url: "https://b.example.com",
        keys: ["***bbbb"],
        models: [
          { id: "m3", model: "model-c", max_tokens: 4096, context_window: 100000, reasoning_effort: null, vision: true, video: true },
        ],
      },
    ],
    active_model_id: "m2",
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
  } as unknown as ConfigState;
}

describe("utils/models flattenModels", () => {
  it("flattens providers × models in provider-then-model order", () => {
    const flat = flattenModels(cfg());
    expect(flat.map((m) => m.id)).toEqual(["m1", "m2", "m3"]);
    expect(flat[2]).toMatchObject({ providerId: "p2", providerName: "Provider Two", model: "model-c", vision: true, video: true });
  });

  it("returns empty array for null/undefined config and providers-free config", () => {
    expect(flattenModels(null)).toEqual([]);
    expect(flattenModels(undefined)).toEqual([]);
    expect(flattenModels({ ...cfg(), providers: [] })).toEqual([]);
  });
});

describe("utils/models findModel", () => {
  it("finds a model across providers by wire id", () => {
    expect(findModel(cfg(), "m3")?.providerName).toBe("Provider Two");
    expect(findModel(cfg(), "m1")?.model).toBe("model-a");
  });

  it("returns null for null/undefined/empty id and unknown id", () => {
    expect(findModel(cfg(), null)).toBeNull();
    expect(findModel(cfg(), undefined)).toBeNull();
    expect(findModel(cfg(), "")).toBeNull();
    expect(findModel(cfg(), "nope")).toBeNull();
  });
});
