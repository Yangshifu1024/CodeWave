// flattenModels / findModel: provider→model flattening consumed by the model menu and guards ([docs/provider-management-refactor](../../../docs/provider-management-refactor.md), [docs/oss-prep-batch](../../../docs/oss-prep-batch.md) coverage batch)
import { describe, it, expect } from "vitest";
import { cacheDenominator, cacheSemanticsOf, cacheSemanticsOfFormat, flattenModels, findModel } from "../utils/models";
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

// 兼容命中率分母的协议口径（[docs/prompt-caching-hardening](../../../docs/prompt-caching-hardening.md)）：
// 唯一事实源，Composer 工具条与统计弹窗共用
function cfgSem(apiFormat: "anthropic_messages" | "openai_chat"): ConfigState {
  return {
    ...cfg(),
    providers: [
      { ...cfg().providers[0], api_format: apiFormat, models: [{ ...cfg().providers[0].models[0], id: "mx" }] },
    ],
    active_model_id: "mx",
  } as ConfigState;
}

describe("utils/models 缓存计费语义", () => {
  it("cacheSemanticsOfFormat：anthropic_messages → anthropic，其余（含空/未知）→ openai", () => {
    expect(cacheSemanticsOfFormat("anthropic_messages")).toBe("anthropic");
    expect(cacheSemanticsOfFormat("openai_chat")).toBe("openai");
    expect(cacheSemanticsOfFormat("openai_responses")).toBe("openai");
    expect(cacheSemanticsOfFormat("anthropic")).toBe("openai"); // 旧写法不由本层兜底（wire 只认 anthropic_messages）
    expect(cacheSemanticsOfFormat(null)).toBe("openai");
    expect(cacheSemanticsOfFormat(undefined)).toBe("openai");
  });

  it("cacheSemanticsOf：按生效模型（会话覆盖优先）反查供应商协议", () => {
    expect(cacheSemanticsOf(cfgSem("anthropic_messages"), null)).toBe("anthropic");
    expect(cacheSemanticsOf(cfgSem("openai_chat"), null)).toBe("openai");
    expect(cacheSemanticsOf(cfgSem("openai_chat"), "mx")).toBe("openai"); // 会话级覆盖
  });

  it("cacheSemanticsOf：解析不到（无模型 / 未知 id / 无 config）→ null（调用方不显示命中率）", () => {
    expect(cacheSemanticsOf(cfgSem("openai_chat"), "nope")).toBeNull();
    expect(cacheSemanticsOf({ ...cfg(), providers: [], active_model_id: null }, null)).toBeNull();
    expect(cacheSemanticsOf(null, "mx")).toBeNull();
    expect(cacheSemanticsOf(undefined, undefined)).toBeNull();
  });

  it("cacheDenominator：anthropic 含 cache_write，openai 只看 input", () => {
    const c = { input: 200, cacheRead: 700, cacheWrite: 100 };
    expect(cacheDenominator(c, "anthropic")).toBe(1000);
    expect(cacheDenominator(c, "openai")).toBe(200);
  });
});
