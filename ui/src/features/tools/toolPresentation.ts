import type { TimelineSeg, ToolView } from "../../stores/run.types";

export interface ToolOrigin { key: string; sig: string; createdAt?: string }

export function toolTime(iso: string | undefined): string {
  if (!iso) return "";
  const value = Date.parse(iso);
  if (!Number.isFinite(value)) return "";
  const date = new Date(value);
  return [date.getHours(), date.getMinutes(), date.getSeconds()].map(part => String(part).padStart(2, "0")).join(":");
}

export function toolArgs(tool: ToolView): Record<string, unknown> {
  try {
    const args: unknown = JSON.parse(tool.argsPreview ?? "{}");
    return args && typeof args === "object" && !Array.isArray(args) ? args as Record<string, unknown> : {};
  } catch { return {}; }
}

export function toolFilePaths(tool: ToolView): string[] {
  const data = tool.outcome?.data;
  const args = toolArgs(tool);
  const strings = (value: unknown): string[] => Array.isArray(value)
    ? value.filter((p): p is string => typeof p === "string" && p !== "") : [];
  const objects = (value: unknown): string[] => strings(Array.isArray(value)
    ? value.map((f: unknown) => f && typeof f === "object" && "path" in f ? f.path : null) : []);
  if (tool.tool === "create") {
    const path = data?.path ?? args.path;
    return typeof path === "string" && path ? [path] : [];
  }
  const fromOutcome = tool.tool === "edit" ? strings(data?.edited) : objects(data?.files);
  return fromOutcome.length ? fromOutcome : objects(args.files);
}

/** 旧历史没有本次写入统计时不从入参猜测，也不借用 Git 工作区统计。 */
export function toolLineChanges(tool: ToolView, path: string): { added: number; removed: number } | null {
  const changes: unknown = tool.outcome?.data?.line_changes;
  if (tool.status !== "ok" || !tool.outcome?.ok || !Array.isArray(changes)) return null;
  const row = changes.find((c) => c && typeof c === "object" && c.path === path);
  return row && Number.isSafeInteger(row.added) && row.added >= 0 && Number.isSafeInteger(row.removed) && row.removed >= 0
    ? { added: row.added, removed: row.removed } : null;
}

/** 聚合仅限已成功完成、无警告且没有独立交互入口的读取/搜索/命令。 */
export function canGroupTool(tool: ToolView | undefined): tool is ToolView {
  if (!tool || tool.status !== "ok" || !tool.outcome?.ok || tool.outcome.warnings?.length) return false;
  if (tool.tool === "command") return tool.outcome.data?.exit_code === 0;
  if (!["read", "batch_read", "grep", "list_files"].includes(tool.tool)) return false;
  const files: unknown = tool.outcome.data?.files;
  return !Array.isArray(files) || files.every((file) => !file?.error && file?.ok !== false);
}

/** 简洁模式「过程组」的一个成员：思考段或工具段（工具的出处由 toolsMap 提供）。
 *  组内成员按原 timeline 顺序渲染，展开后与未折叠时逐段一致。 */
export type ProcessMember =
  | { kind: "thinking"; index: number; seg: Extract<TimelineSeg, { kind: "thinking" }> }
  | { kind: "tool"; index: number; callKey: string; tool: ToolView };

/** 正在计时中的思考段（有 startedAt、尚未定格 durationMs）：摘要要显示实时秒数。
 *  恢复的历史段与被中断的段都落在 else 分支 —— 它们没有真实耗时，绝不补 0s。 */
export function isThinkingActive(seg: Extract<TimelineSeg, { kind: "thinking" }>, streamingTail: boolean): boolean {
  return seg.durationMs === undefined && seg.startedAt !== undefined && streamingTail;
}

/** 段能否并入过程组：思考恒可；工具看 canGroupTool。子代理 / 正文 / 未知段一律不可（分隔）。 */
export function canJoinProcessGroup(
  seg: TimelineSeg,
  toolsMap: Record<string, ToolView>,
  options?: { includeRunning?: boolean },
): boolean {
  if (seg.kind === "thinking") return true;
  if (seg.kind !== "tool") return false;
  const tool: ToolView | undefined = toolsMap[seg.callKey];
  // Subagents have their own timeline cards. Grouping must not bypass that deduplication.
  if (tool?.tool === "subagent") return false;
  if (canGroupTool(tool)) return true;
  // 运行中的工具同样留在组内：跑批期间也保持一行，摘要带加载指示。
  // 只放 running（正在等审批的 waiting 与失败的 error 必须独立成行，不能藏进摘要）。
  // 不靠 canGroupTool 的收窄读 status（它会把失败分支收成 never），用局部变量重新读。
  const running = toolsMap[seg.callKey];
  return !!options?.includeRunning && running !== undefined && running.status === "running";
}

/** 过程组的稳定锚点根键：优先取组内首个工具的 callKey（provider 侧 tool_use.id，跨消息摊平后仍稳定），
 *  纯思考组才退回 `thinking:<段下标>`（这类组必在同一消息内，下标稳定）。
 *  绝不伪造 callKey —— 它是 outputAnchor 与重复 id 守卫的唯一依据（[docs/chat-and-interface](../../../../docs/chat-and-interface.md) 5.2.x）。 */
export function processGroupKey(members: readonly ProcessMember[]): string | null {
  for (const member of members) {
    if (member.kind === "tool") return member.callKey;
  }
  const first = members[0];
  return first && first.kind === "thinking" ? `thinking:${first.index}` : null;
}

/** 组的输出锚点键（与 DOM 上的 data-output-key 同构）：工具沿用 `tool:` 前缀，思考段用 `thinking:`。 */
export function processMemberOutputKey(member: ProcessMember): string {
  return member.kind === "tool" ? `tool:${member.callKey}` : `thinking:${member.index}`;
}

/** 组摘要行自身的 data-output-key：取根键对应的那个成员（与 processGroupKey 同一成员，
 *  否则「根键 → 锚点节点」会指向另一个成员，outputAnchor 的同类型匹配会错落）。
 *  单源实现：根键本身已编码了成员身份（tool:<callKey> 或 thinking:<下标>），直接补前缀。 */
export function processGroupOutputKey(members: readonly ProcessMember[]): string | null {
  const key = processGroupKey(members);
  if (key === null) return null;
  return key.startsWith("thinking:") ? key : `tool:${key}`;
}
