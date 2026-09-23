// 布局宽度常量与「只夹显示、保留记忆值」的纯函数
// （[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）。
//
// 约定：左右栏宽度由用户拖拽决定并存 localStorage（全局一份）；窗口变窄放不下时**只夹显示**，
// 不改写记忆值——窗口变宽后回到用户拖出的宽度。所有宽度都是逻辑像素（与 CSS px 同义）。

/** 左栏（项目/会话导航）默认宽（= 原常量 280，保持既有默认视觉） */
export const NAV_W_DEFAULT = 280;
/** 左栏最小宽（再窄会让项目名/操作按钮不可用） */
export const NAV_W_MIN = 180;
/** 左栏最大宽 */
export const NAV_W_MAX = 480;

/** 右栏默认宽（= 原 CSS 328，保持既有默认视觉） */
export const RB_W_DEFAULT = 328;
/** 右栏最小宽：328 = 内容下限 300 + 左右 padding 28（`.rb-tabs{min-width:300px}` 契约不破，不裁切） */
export const RB_W_MIN = 328;
/** 右栏最大宽 */
export const RB_W_MAX = 640;

/** 中栏（对话区）保底宽：与 src-tauri 的窗口最小宽 1024 配套（180 + 480 + 328 = 988 ≤ 1024） */
export const CHAT_W_MIN = 480;

/** 两侧栏合计不得超过窗口宽度的比例（防止把中栏挤没） */
const PAIR_BUDGET_RATIO = 0.7;

/** 把任意数值收敛进 [min, max]；NaN/Infinity 回退 fallback。 */
export function clampWidth(value: unknown, min: number, max: number, fallback: number): number {
  const n = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, Math.round(n)));
}

/** 左栏宽度收敛（拖动与读盘共用）。 */
export function clampNavWidth(value: unknown): number {
  return clampWidth(value, NAV_W_MIN, NAV_W_MAX, NAV_W_DEFAULT);
}

/** 全屏页（设置 / 计划任务）左栏基准宽与窄窗收缩比例：两页共用同一套度量 ——
 *  全屏页左栏恒占「工作区左栏」那一格（同宽、同背景 `--ws-bg-nav`），两页切换时左侧不跳色也不跳宽。 */
export const FULLSCREEN_NAV_W = NAV_W_DEFAULT;
export const FULLSCREEN_NAV_RATIO = 0.32;

/** 全屏页左栏显示宽：基准 280，窄窗按比例收缩，由 clampNavWidth（180..480）夹取。 */
export function fullscreenNavWidth(windowWidth: number): number {
  return clampNavWidth(Math.min(FULLSCREEN_NAV_W, Math.round(windowWidth * FULLSCREEN_NAV_RATIO)));
}

/** 右栏宽度收敛（拖动与读盘共用）。 */
export function clampRightBarWidth(value: unknown): number {
  return clampWidth(value, RB_W_MIN, RB_W_MAX, RB_W_DEFAULT);
}

/**
 * 显示态宽度决议：**纯函数，不改记忆值**。
 * 优先级：① 两侧合计 ≤ 窗口 70%；② 中栏保底 CHAT_W_MIN —— 先压右栏、再压左栏，各自触底为止。
 */
export function resolveDisplayWidths(
  navWidth: number,
  rightBarWidth: number,
  windowWidth: number,
): { nav: number; rightBar: number } {
  let nav = clampNavWidth(navWidth);
  let rightBar = clampRightBarWidth(rightBarWidth);
  const width = Number.isFinite(windowWidth) && windowWidth > 0 ? windowWidth : 0;
  if (width === 0) return { nav, rightBar };

  const pairBudget = Math.max(NAV_W_MIN + RB_W_MIN, Math.floor(width * PAIR_BUDGET_RATIO));
  const chatBudget = Math.max(NAV_W_MIN + RB_W_MIN, width - CHAT_W_MIN);

  const squeeze = (limit: number) => {
    let overflow = nav + rightBar - limit;
    if (overflow <= 0) return;
    const rbRoom = rightBar - RB_W_MIN;
    const takeRb = Math.min(rbRoom, overflow);
    rightBar -= takeRb;
    overflow -= takeRb;
    if (overflow <= 0) return;
    const navRoom = nav - NAV_W_MIN;
    nav -= Math.min(navRoom, overflow);
  };

  squeeze(pairBudget);
  squeeze(chatBudget);
  return { nav, rightBar };
}

/** 拖动某一侧时的上限：不超过「另一侧已占 + 中栏保底 + 70% 预算」允许的空间。 */
export function dragLimit(side: "nav" | "right", other: number, windowWidth: number): number {
  const width = Number.isFinite(windowWidth) && windowWidth > 0 ? windowWidth : 0;
  const pairBudget = width > 0 ? Math.floor(width * PAIR_BUDGET_RATIO) : Number.POSITIVE_INFINITY;
  const chatBudget = width > 0 ? width - CHAT_W_MIN : Number.POSITIVE_INFINITY;
  const allowed = Math.min(pairBudget, chatBudget) - other;
  const floor = side === "nav" ? NAV_W_MIN : RB_W_MIN;
  const ceiling = side === "nav" ? NAV_W_MAX : RB_W_MAX;
  // 窗口极窄时 allowed 可能小于下限：仍以下限为准（显示层再统一夹取）
  return Math.min(ceiling, Math.max(floor, allowed));
}
