// 右栏信息面板的本地偏好（localStorage，**全局一份**，跨项目/会话共享）
// （[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 三项偏好都遵循「值非法/缺失 → 回默认」：技能/计划默认展开、额度家默认折叠、编辑器默认取
// 后端候选表第一个检测到的（本模块只记用户手动改选过的那一个）。

/** 折叠的段落 id（值为「已折叠」集合；缺省 = 全展开） */
export type CollapsibleSection = "skills" | "plan" | "goal";

export const COLLAPSED_SECTIONS_KEY = "ws_rb_sections_collapsed";
export const EXPANDED_QUOTA_KEY = "ws_rb_quota_expanded";
export const PREFERRED_EDITOR_KEY = "ws_editor";

function readIdSet(key: string): Set<string> {
  if (typeof localStorage === "undefined") return new Set();
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((v): v is string => typeof v === "string"));
  } catch {
    return new Set();
  }
}

function writeIdSet(key: string, ids: Set<string>): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(key, JSON.stringify([...ids]));
}

/** 已折叠的段落集合（缺省空集 = 全部展开）。 */
export function readCollapsedSections(): Set<CollapsibleSection> {
  const all = readIdSet(COLLAPSED_SECTIONS_KEY);
  const out = new Set<CollapsibleSection>();
  for (const id of ["skills", "plan", "goal"] as const) {
    if (all.has(id)) out.add(id);
  }
  return out;
}

/** 写回折叠集合。 */
export function writeCollapsedSections(sections: Set<CollapsibleSection>): void {
  writeIdSet(COLLAPSED_SECTIONS_KEY, sections);
}

/**
 * 已展开的额度提供商集合（缺省空集 = 全部收起为一行摘要）。
 * `validIds` = 当前快照里的供应商 id 集合：旧版本写入的 kind 串（如 `opencode-go`）在这层被过滤掉，
 * **不回写** localStorage（只有用户操作行时才写），避免把用户偏好换成脏值。
 */
export function readExpandedQuotaProviders(validIds?: Set<string>): Set<string> {
  const all = readIdSet(EXPANDED_QUOTA_KEY);
  if (!validIds) return all;
  const out = new Set<string>();
  for (const id of all) if (validIds.has(id)) out.add(id);
  return out;
}

/** 写回额度提供商的展开集合。 */
export function writeExpandedQuotaProviders(providers: Set<string>): void {
  writeIdSet(EXPANDED_QUOTA_KEY, providers);
}

/** 用户上次手动选定的编辑器 id（未选过返回 null = 用检测到的第一个）。 */
export function readPreferredEditor(): string | null {
  if (typeof localStorage === "undefined") return null;
  const raw = localStorage.getItem(PREFERRED_EDITOR_KEY);
  return raw && raw.trim() ? raw : null;
}

/** 记住用户手动选定的编辑器。 */
export function writePreferredEditor(id: string): void {
  if (typeof localStorage === "undefined") return;
  localStorage.setItem(PREFERRED_EDITOR_KEY, id);
}
