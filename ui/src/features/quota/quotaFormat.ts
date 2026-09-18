// 额度段的纯格式化逻辑（无 React 依赖，单测友好）
// （[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
import type { QuotaEntry, QuotaSnapshot } from "../../ipc/types";

/** 风险分级：<5% 红、<20% 橙、其余中性（项目「色彩强度映射风险等级」约定） */
export type RiskLevel = "normal" | "warn" | "danger";

export const DANGER_PERCENT = 5;
export const WARN_PERCENT = 20;

/** 剩余百分比 → 风险等级（null = 数值行，不参与分级） */
export function riskLevel(remainingPercent: number | null | undefined): RiskLevel {
  if (remainingPercent === null || remainingPercent === undefined) return "normal";
  if (remainingPercent < DANGER_PERCENT) return "danger";
  if (remainingPercent < WARN_PERCENT) return "warn";
  return "normal";
}

/** 已知窗口键 → i18n 叶子名；未知键返回 null（前端原样显示 key，不隐藏数据） */
export function windowLabelKey(key: string): string | null {
  const known: Record<string, string> = {
    rolling: "rolling",
    five_hour: "rolling",
    weekly: "weekly",
    week: "weekly",
    monthly: "monthly",
    mcp: "mcp",
    usage: "usage",
  };
  if (known[key]) return known[key];
  if (key.startsWith("balance_")) return "balance";
  return null;
}

/** 窗口显示标签：厂商自带 label 优先，其次 i18n 映射，最后原样显示 key */
export function entryLabel(
  entry: QuotaEntry,
  translate: (key: string) => string,
): string {
  if (entry.label) return entry.label;
  const leaf = windowLabelKey(entry.key);
  return leaf ? translate(leaf) : entry.key;
}

/** 摘要行取值：百分比行取「剩余最小」的一个；纯数值行取第一条 */
export function summaryEntry(entries: QuotaEntry[]): QuotaEntry | null {
  const percents = entries.filter(
    (e) => e.remaining_percent !== null && e.remaining_percent !== undefined,
  );
  if (percents.length > 0) {
    return percents.reduce((worst, current) =>
      (current.remaining_percent ?? 101) < (worst.remaining_percent ?? 101) ? current : worst,
    );
  }
  return entries[0] ?? null;
}

/** 摘要行的剩余百分比（纯数值行 → null） */
export function summaryRemaining(entries: QuotaEntry[]): number | null {
  return summaryEntry(entries)?.remaining_percent ?? null;
}

/** 重置倒计时文案（i18n key + 参数）；无重置时间或已过期返回 null */
export function countdown(
  resetsAt: string | null | undefined,
  now: number,
): { key: string; params: Record<string, number> } | null {
  if (!resetsAt) return null;
  const target = Date.parse(resetsAt);
  if (Number.isNaN(target)) return null;
  const seconds = Math.floor((target - now) / 1000);
  if (seconds <= 0) return null;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 1) return { key: "rightbar.quotaResetSoon", params: {} };
  const hours = Math.floor(minutes / 60);
  if (hours < 1) return { key: "rightbar.quotaResetMinutes", params: { m: minutes } };
  const days = Math.floor(hours / 24);
  if (days < 1) {
    return { key: "rightbar.quotaResetHours", params: { h: hours, m: minutes % 60 } };
  }
  return { key: "rightbar.quotaResetDays", params: { d: days, h: hours % 24 } };
}

/** 「X 分钟前更新」：取所有快照里最新的 fetched_at；无数据返回 null */
export function relativeUpdatedAt(
  snapshots: QuotaSnapshot[],
  now: number,
): { key: string; params: Record<string, number> } | null {
  const latest = snapshots
    .map((s) => Date.parse(s.fetched_at))
    .filter((t) => !Number.isNaN(t))
    .reduce<number | null>((max, t) => (max === null || t > max ? t : max), null);
  if (latest === null) return null;
  const minutes = Math.floor(Math.max(0, now - latest) / 60000);
  if (minutes < 1) return { key: "rightbar.quotaUpdatedJustNow", params: {} };
  return { key: "rightbar.quotaUpdatedMinutes", params: { m: minutes } };
}

/** 百分比展示：整数 + %（0.5 步进以下不进位，避免 99.9 显示成 100） */
export function formatPercent(value: number | null | undefined): string {
  if (value === null || value === undefined) return "—";
  return `${Math.floor(value)}%`;
}
