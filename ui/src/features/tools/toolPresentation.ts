import type { ToolView } from "../../stores/run.types";

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
