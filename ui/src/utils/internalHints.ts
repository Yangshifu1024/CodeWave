// Synthetic user-role history entries are model instructions, not Composer input.
// Keep the match narrow: complete reserved tags or exact drive.rs templates.
const INTERNAL_HINT_TAGS = [
  "<continue-notice>",
  "<tool-args-rejected>",
  "<text-turn-limit>",
  "<budget-notice>",
  "<final-report>",
  "<supervision-notice>",
  "<supervision-escalated>",
] as const;

// core/agent/drive.rs: approval_mode_label, plan approval and subagent mode sync.
const MODE_LABELS = ["逐项确认模式", "自动编辑模式", "计划模式", "完全访问模式"] as const;
const INTERNAL_SYSTEM_MESSAGES = new Set(MODE_LABELS.flatMap(mode => [
  `[system] 方案已获批准，会话已切换到${mode}。请立即按方案开始执行，无需再次询问。`,
  `[system] 权限模式已变更为 ${mode}`,
]));

/** Display/recall only; never change persisted history, wire or approval state. */
export function isInternalHint(text: string, hasImages = false): boolean {
  // The runtime emits text-only hints. Attached images identify actual user input.
  if (hasImages) return false;
  const trimmed = text.trim();
  return INTERNAL_SYSTEM_MESSAGES.has(trimmed) || INTERNAL_HINT_TAGS.some(
    tag => trimmed.startsWith(tag) && trimmed.endsWith(`</${tag.slice(1, -1)}>`),
  );
}
