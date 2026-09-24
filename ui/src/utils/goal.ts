// 目标模式（`ApprovalMode::Goal`）的展示映射。
// Composer 提示条与右栏「目标」段共用同一张表，避免两处各写一份状态标签而漂移。
// 注意：这里的值是 i18n **键名**（不是文案），调用点形如 `t(GOAL_STATUS_KEYS[status])`——
// 纯动态键调用点必须在 `settings.registry.test.ts` 的 DYNAMIC_KEY_CALLS 里登记（闭包正则看不见）。
import type { GoalStatus } from "../ipc/types";

/** 状态 → i18n 键（键落在 `composer.*` 段：两处展示的都是同一套档位语汇） */
export const GOAL_STATUS_KEYS: Record<GoalStatus, string> = {
  clarify: "composer.goalStatusClarify",
  executing: "composer.goalStatusExecuting",
  paused: "composer.goalStatusPaused",
  done: "composer.goalStatusDone",
  aborted: "composer.goalStatusAborted",
};

/** 目标状态缺省值：无目标 = 待澄清（与后端「切档后下一条消息即目标」的口径一致） */
export const GOAL_STATUS_DEFAULT: GoalStatus = "clarify";
