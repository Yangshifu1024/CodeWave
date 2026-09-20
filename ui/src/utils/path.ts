/** 受管数据目录名（与后端 core/config.rs 的 MANAGED_DIR_NAME 同源；改动需两端同步） */
export const MANAGED_DIR_NAME = ".codewave";

/** 取路径末端的目录/文件名：Windows 反斜杠与 POSIX 分隔符统一归一处理 */
export function baseName(p: string): string {
  return p.replace(/\\/g, "/").split("/").filter(Boolean).pop() || p;
}

/** 末段标签（等价工具卡旧实现 String(p ?? "").split(/[\\/]/).pop() ?? ""：尾随分隔符得空串）。 */
function lastSeg(p: unknown): string {
  return String(p ?? "").split(/[\\/]/).pop() ?? "";
}

/** 最短可区分标签：默认取末段（与旧 basename 输出逐字节一致）；仅对**同标签冲突组**逐步向前扩段
 *  （a/ui.ts / b/ui.ts）直到组内两两不同或段用尽；段用尽仍相同则保持原标签，空串保持空串。
 *  结果与入参按下标一一对应。 */
export function shortestUniqueLabels(paths: string[]): string[] {
  const segs = paths.map((p) => String(p ?? "").split(/[\\/]/));
  const labels = paths.map((p) => lastSeg(p));
  const groups = new Map<string, number[]>();
  labels.forEach((label, i) => {
    const g = groups.get(label);
    if (g) g.push(i);
    else groups.set(label, [i]);
  });
  for (const idxs of groups.values()) {
    if (idxs.length < 2) continue; // 无冲突组一律保持末段标签
    // 空串冲突组（尾随分隔符路径，如 "a/" / "b/"）：不向前扩段——那会把空标签写成非空，
    // 与「空串保持空串」的约定相违；这类条目的过滤由调用方（摘要的 filter(Boolean)）负责
    if (idxs.every((i) => labels[i] === "")) continue;
    const maxDepth = Math.max(...idxs.map((i) => segs[i].length));
    for (let d = 2; d <= maxDepth; d++) {
      const cand = idxs.map((i) => segs[i].slice(-d).join("/"));
      if (new Set(cand).size === idxs.length) {
        idxs.forEach((i, k) => (labels[i] = cand[k]));
        break;
      }
    }
  }
  return labels;
}
