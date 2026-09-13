/** 技能来源短标签：内置 "<builtin>" 返回空；目录形态（origin 指向 <目录>/SKILL.md）取技能目录名，
 *  单文件形态取文件名（如 xxx.md）。设置页与右栏共享，勿复制分叉。 */
export function originLabel(origin: string): string {
  if (origin === "<builtin>") return "";
  const segs = origin.split(/[\\/]/).filter(Boolean);
  if (segs.length === 0) return origin;
  const last = segs[segs.length - 1];
  if (last === "SKILL.md" && segs.length >= 2) return segs[segs.length - 2];
  return last;
}
