// 模型摊平工具（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：providers 嵌套 models → 菜单/守卫消费的摊平视图。
// wire id 即显示名；顺序 = provider 顺序 × 各 provider 内模型顺序（模型菜单分组遵循此序）。
import type { ConfigState, FlatModel } from "../ipc/types";

/** 摊平全部 provider 的模型列表（ConfigState → FlatModel[]） */
export function flattenModels(config: ConfigState | null | undefined): FlatModel[] {
  const out: FlatModel[] = [];
  for (const p of config?.providers ?? []) {
    for (const m of p.models) {
      out.push({
        id: m.id,
        providerId: p.id,
        providerName: p.name,
        model: m.model,
        vision: m.vision,
        video: m.video,
      });
    }
  }
  return out;
}

/** 按 id 查找摊平模型，未命中返回 null。 */
export function findModel(
  config: ConfigState | null | undefined,
  id: string | null | undefined,
): FlatModel | null {
  if (!id) return null;
  return flattenModels(config).find((m) => m.id === id) ?? null;
}

/**
 * 当前生效模型所属 provider 的 id（额度段 IPC 入参）。
 * `modelId` 为会话级覆盖；为空则回落到全局 `active_model_id`；未配置返回 null。
 */
export function activeProviderIdOf(
  config: ConfigState | null | undefined,
  modelId: string | null | undefined,
): string | null {
  const target = modelId ?? config?.active_model_id ?? null;
  return findModel(config, target)?.providerId ?? null;
}

/**
 * 缓存计费语义（[docs/prompt-caching-hardening](../../../docs/prompt-caching-hardening.md)）：
 * `anthropic_messages` 的 `usage.input` **不含**缓存部分（真输入 = input + cache_read + cache_write）；
 * `openai_chat` / `openai_responses` 的 `input` **已包含** `cached_tokens`（cache_read 是 input 的子集）。
 * 两套口径的「命中率分母」不同，必须按协议区分——混淆会让 openai 系命中率系统性偏低。
 */
export type CacheSemantics = "anthropic" | "openai";

/** 协议 → 缓存语义（**分母口径的唯一事实源**：Composer 工具条与统计弹窗共用，勿在第二处重写公式）。 */
export function cacheSemanticsOfFormat(apiFormat: string | null | undefined): CacheSemantics {
  return apiFormat === "anthropic_messages" ? "anthropic" : "openai";
}

/** 生效模型（会话覆盖优先，语义同 `activeProviderIdOf`）所属 provider 的缓存语义。
 *  模型或 provider 解析不到时返回 null——调用方据此**不显示命中率**，宁可缺省也不猜口径。 */
export function cacheSemanticsOf(
  config: ConfigState | null | undefined,
  modelId: string | null | undefined,
): CacheSemantics | null {
  const target = modelId ?? config?.active_model_id ?? null;
  const model = findModel(config, target);
  if (!model) return null;
  const provider = config?.providers.find((p) => p.id === model.providerId);
  return provider ? cacheSemanticsOfFormat(provider.api_format) : null;
}

/** 命中率分母（按语义）：anthropic = input + cache_read + cache_write；openai = input。 */
export function cacheDenominator(
  counters: { input: number; cacheRead: number; cacheWrite: number },
  sem: CacheSemantics,
): number {
  return sem === "anthropic"
    ? counters.input + counters.cacheRead + counters.cacheWrite
    : counters.input;
}
