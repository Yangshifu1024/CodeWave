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
 * 当前生效模型所属 provider 的 `base_url`（额度段「当前会话提供商置顶」的唯一依据）。
 * 只认 base_url 域名，**不看模型 wire id 前缀**——OpenCode Go 上也跑 DeepSeek 模型
 * （[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
 * `modelId` 为会话级覆盖；为空则回落到全局 `active_model_id`；未配置返回 null。
 */
export function activeBaseUrlOf(
  config: ConfigState | null | undefined,
  modelId: string | null | undefined,
): string | null {
  const target = modelId ?? config?.active_model_id ?? null;
  const model = findModel(config, target);
  if (!model) return null;
  return config?.providers.find((p) => p.id === model.providerId)?.base_url ?? null;
}
