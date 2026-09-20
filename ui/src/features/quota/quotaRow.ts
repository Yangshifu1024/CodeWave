// 额度行集合的纯逻辑（无 React、无 i18n：**文案映射一律留在 QuotaSection 里**，
// 这样本文件不进 features 的「文件 → i18n 段」白名单，见 ui/src/__tests__/settings.registry.test.ts）。
//
// 行集合 = CodeWave 供应商配置（[docs/quota-from-provider-config](../../../docs/quota-from-provider-config.md)）：
// 后端按「可查询类在前 + 配置顺序、unsupported 殿后」排好序，前端不再重排序，只做展示层加工
// （标题消歧 / unsupported 折叠判定 / 窗口状态归一 / 相对时间分级）。
import type { QuotaSnapshot } from "../../ipc/types";

/** unsupported 行**多于此值**时才折叠成一行汇总（3 家以内直接逐条列） */
export const UNSUPPORTED_COLLAPSE_MIN = 3;

/** 把快照切成「可查询类（含 no_key / rejected 等独立行）」与「unsupported 段」，两侧都保持后端顺序 */
export function splitBySupport(snapshots: QuotaSnapshot[]): {
  main: QuotaSnapshot[];
  unsupported: QuotaSnapshot[];
} {
  const main: QuotaSnapshot[] = [];
  const unsupported: QuotaSnapshot[] = [];
  for (const s of snapshots) {
    if (s.status === "unsupported") unsupported.push(s);
    else main.push(s);
  }
  return { main, unsupported };
}

/** unsupported 段是否默认折叠为一行汇总 */
export function shouldCollapseUnsupported(count: number): boolean {
  return count > UNSUPPORTED_COLLAPSE_MIN;
}

/** unsupported 的原因（未知值一律按「域名不在支持范围」处理） */
export type UnsupportedReason = "empty_base_url" | "no_adapter";

export function unsupportedReason(snapshot: Pick<QuotaSnapshot, "reason">): UnsupportedReason {
  return snapshot.reason === "empty_base_url" ? "empty_base_url" : "no_adapter";
}

/** `base_url` → 主机名（容忍缺 scheme 的写法，如 `api.example.com/v1`）；拿不到返回 null */
export function hostOf(baseUrl: string | null | undefined): string | null {
  const raw = (baseUrl ?? "").trim();
  if (!raw) return null;
  for (const candidate of [raw, `https://${raw}`]) {
    try {
      const host = new URL(candidate).host;
      if (host) return host;
    } catch {
      // 换一种写法再试；两种都不行就返回 null（调用方跳过后缀）
    }
  }
  return null;
}

/**
 * 标题消歧（后端顺序为唯一权威）：
 * ① 按 `display_name` 分组，名字唯一 → 原样；
 * ② 名字重复 → 该组每条追加 `base_url` 主机名后缀（拿不到主机名就跳过后缀）；
 * ③ 名字与主机名都相同（同家两账号）→ 组内按后端顺序再追加序号，第 1 条不加、第 2 条起 `(2)`、`(3)`…
 */
export function disambiguateTitles(
  snapshots: QuotaSnapshot[],
  hostFor: (providerId: string) => string | null,
): Map<string, string> {
  const hostById = new Map<string, string | null>();
  const baseName = new Map<string, string>();
  for (const s of snapshots) {
    const host = hostFor(s.provider_id);
    hostById.set(s.provider_id, host);
    const name = (s.display_name ?? "").trim();
    baseName.set(s.provider_id, name || host || s.provider_id);
  }

  const byName = new Map<string, QuotaSnapshot[]>();
  for (const s of snapshots) {
    const key = baseName.get(s.provider_id)!;
    const list = byName.get(key);
    if (list) list.push(s);
    else byName.set(key, [s]);
  }

  const titles = new Map<string, string>();
  for (const [name, group] of byName) {
    if (group.length === 1) {
      titles.set(group[0].provider_id, name);
      continue;
    }
    const byHost = new Map<string, QuotaSnapshot[]>();
    for (const s of group) {
      const key = hostById.get(s.provider_id) ?? "\u0000";
      const list = byHost.get(key);
      if (list) list.push(s);
      else byHost.set(key, [s]);
    }
    for (const sameHost of byHost.values()) {
      sameHost.forEach((s, index) => {
        const host = hostById.get(s.provider_id);
        const title = host ? `${name} · ${host}` : name;
        titles.set(s.provider_id, index === 0 ? title : `${title} (${index + 1})`);
      });
    }
  }
  return titles;
}

/** 窗口状态归一：受限类着色，其它未知值原样灰显（forward-compatible） */
export type EntryStatusKind = "none" | "limited" | "other";

/** 受限类状态（大小写与 `-` / `_` / 空格归一后比较） */
const LIMITED_STATUSES = new Set(["ratelimited", "exceeded", "blocked", "limited"]);

export function entryStatusKind(status: string | null | undefined): EntryStatusKind {
  const raw = (status ?? "").trim();
  if (!raw || raw.toLowerCase() === "ok") return "none";
  const normalized = raw.toLowerCase().replace(/[-_\s]+/g, "");
  return LIMITED_STATUSES.has(normalized) ? "limited" : "other";
}

/** 相对时间分级（`error` / `invalid` / `rejected` 行的「时间行」用；文案由组件按 kind 映射） */
export type AgoKind =
  | { kind: "just_now" }
  | { kind: "minutes"; value: number }
  | { kind: "hours"; value: number }
  | { kind: "days"; value: number };

/**
 * 相对时间分级；返回 `null` = 没有可用时刻（`last_ok_at` 为 null，或值不可解析）。
 * 数据源是后端持久化的 `last_ok_at`（`~/.codewave/quota.json`），「从未成功过」已由后端的 null 表达，
 * 前端不再自己记忆会话内缺省，故没有 never 分支：null 时 `rejected` 行由组件写「从未成功查询」，
 * 其余行（`error` / `invalid`）干脆不显示这一行。
 */
export function agoKind(iso: string | null | undefined, now: number): AgoKind | null {
  if (!iso) return null;
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return null;
  const minutes = Math.floor(Math.max(0, now - ts) / 60000);
  if (minutes < 1) return { kind: "just_now" };
  if (minutes < 60) return { kind: "minutes", value: minutes };
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return { kind: "hours", value: hours };
  return { kind: "days", value: Math.floor(hours / 24) };
}
