// Composer 工具条速率段与统计面板总览三项的纯派生口径（[docs/composer-token-rate](../../../../docs/composer-token-rate.md)）。
// 只做「数值 → 展示值」的换算，不依赖 React / store / i18n：工具条（`Composer.tsx`）与统计面板
// （`features/panels/TokenStatsModal.tsx`）共用同一份实现——同一口径写两份必然漂移
// （token 缩写 `M`/`k` 此前只存在于 modal，现在同样收敛到本文件）。

/** 生成速率的输入形状：`TabRunState.runMetrics`（本轮）与统计面板总览的加权聚合结果都满足它。
 *  字段名与 store 侧逐字对齐（`genMs` = Σ 各步生成耗时，`toolMs` = Σ 工具卡耗时，均不进速率分母）。 */
export interface RateInput {
  output: number;
  genMs: number;
  steps: number;
  /** 首个输出增量（含思考）延迟；无数据为 null */
  ttftMs?: number | null;
  /** 工具等待合计（仅 tooltip 展示，不参与速率/均步耗时） */
  toolMs?: number;
}

/** 平均生成速率（tok/s）= Σoutput ÷ (Σ各步生成耗时 / 1000)。
 *  分母 ≤0（缺字段 / `duration_ms` 为 0 / 负值）或分子 ≤0 一律返回 **null**（= 无数据）：
 *  绝不返回 0 / NaN / Infinity，显示端据此整段隐藏（AC-9）。
 *  工具耗时不在分母里——它是等待，不是生成，换工具卡耗时不影响本值（AC-5）。 */
export function tokPerSec(m: RateInput | null | undefined): number | null {
  const output = m?.output ?? 0;
  const genMs = m?.genMs ?? 0;
  if (!Number.isFinite(output) || !Number.isFinite(genMs)) return null;
  if (genMs <= 0 || output <= 0) return null;
  return output / (genMs / 1000);
}

/** 均步耗时（ms）= Σ生成耗时 ÷ 步数；步数 ≤0 → null（不臆造「每步 0 ms」）。 */
export function avgStepMs(m: RateInput | null | undefined): number | null {
  const genMs = m?.genMs ?? 0;
  const steps = m?.steps ?? 0;
  if (!Number.isFinite(genMs) || !Number.isFinite(steps)) return null;
  if (genMs <= 0 || steps <= 0) return null;
  return genMs / steps;
}

/** 速率展示值：<10 两位小数 / 10–99 一位小数 / ≥100 取整；`0 < n < 0.01` 显示 `<0.01`。
 *  非有限值与负值返回空串（调用方本就不该在此分支渲染——`tokPerSec` 已把这两种情形归为 null）。 */
export function formatRate(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "";
  if (n === 0) return "0.00";
  if (n < 0.01) return "<0.01";
  if (n < 10) return n.toFixed(2);
  if (n < 100) return n.toFixed(1);
  return String(Math.round(n));
}

/** 耗时展示值：<1s 用整毫秒（`420 ms`），≥1s 用秒并保留一位（`12.3 s`）；无效值返回空串。 */
export function formatMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

/** token 数缩写：M / k 分级（口径与统计面板既有 `fmt()` 一致，现统一由本函数承担）。 */
export function formatInt(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}
