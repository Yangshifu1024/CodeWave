// 清理提示的去重记录（[docs/session-cleanup](../../docs/session-cleanup.md) §3 第 14 条）。
//
// 为什么单独一个工具：后端「上次清理记录」只有一份，而前端有两个地方会把它展示给用户——
//   ① 启动轻提示（features/shell/AppShell.tsx）：启动时后端已按保留期清理过一次，确有删除就提醒一句；
//   ② 设置页「上次清理」只读行（features/panels/SettingsPage.tsx）：进设置页时拉同一份记录。
// 去重口径只有一份（本文件）：只要用户在任意一处看过这一次清理，下次启动都不该为同一件事再提醒一遍。
// 若两处各写一套判定（此前就是 AppShell 里内联的 localStorage 比较），用户在设置页看过结果后，
// 下次启动仍会被提醒——这正是本次返工要修的问题。
//
// 记录内容是「已经提示过的那一次清理的时间」，键名风格与 ws_theme / ws_auto_update /
// ws_settings_show_advanced 一致（ws_ 前缀 = 前端本地偏好）。

/** 去重记录键：存「已经提示过的那一次清理的时间」（= 后端记录里的 `last_run_at`，RFC3339 串） */
export const CLEANUP_NOTICE_SEEN_KEY = "ws_cleanup_notice_seen_run";

/**
 * 取「还没提示过的那次清理的时间」：调用方拿到非空值就该提示（并随后调用 markCleanupNoticeSeen）。
 * 返回 null 的两种情形都是「不用打扰用户」：还没清理过（没有记录）／这一次已经提示过。
 */
export function pendingCleanupNotice(lastRunAt: string | null | undefined): string | null {
  const at = lastRunAt ?? null;
  if (!at) return null;
  try {
    return localStorage.getItem(CLEANUP_NOTICE_SEEN_KEY) === at ? null : at;
  } catch {
    // 读不到记录（隐私模式 / 配额被拒）：按「还没提示过」处理——宁可多说一次，也不吞掉清理结果
    return at;
  }
}

/**
 * 标记「这一次清理已经提示过」：调用方在**展示结果之后**调用（写入失败不抛错，最多下次启动重复提示一遍）。
 * 没有记录（`last_run_at` 为空）时不写：没有可提示的内容，也就没有可去重的东西。
 */
export function markCleanupNoticeSeen(lastRunAt: string | null | undefined): void {
  const at = lastRunAt ?? null;
  if (!at) return;
  try {
    localStorage.setItem(CLEANUP_NOTICE_SEEN_KEY, at);
  } catch {
    // 写不进去（隐私模式 / 配额被拒）：本次会话内不重复提示由调用方自行把握，这里只保证不抛错
  }
}
