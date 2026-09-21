// Composer 文件引用的「文本 ↔ chip」双向工具（[docs/composer-file-ref-chips](../../../../docs/composer-file-ref-chips.md)）。
//
// 为什么需要它：引用曾经就是**正文文本**（选完附件往输入框追加一行 `@路径`），
// 于是项目外的长绝对路径把正文挤走、Windows 上还会带 canonicalize 的 `\\?\` 前缀。
// 现在引用住在草稿的 `refs[]` 里、以 chip 展示，**发送前一刻**才合成回 `@路径` 追加到正文末尾——
// 后端 startChat 契约与模型所见语义完全不变，改动只在展示层。
//
// 三条不变式：
//   1. mergeRefs(splitRefs(t)) 对「引用在末尾、单空格分隔」（即 app 自己产出的形态）逐字节等于原文；
//      引用在句子中间时位置会被规范化到末尾——所以**回填路径一律走 recoverRefs**，用这条往返式做门禁，
//      宁可把原文原样留在输入框里也不搬动/截断用户写的内容；
//   2. 判定是**保守启发式**：`@types/node`、`@用户名` 一律不算引用（留在正文）；
//   3. 判定错了也不会有功能风险：合成永远带回完整 token，回填又有门禁，误判最多影响观感。

/** 引用 token 的保守判定：`@` 打头、且形态像路径（末段带扩展名，或绝对路径形态）。
 *  宁可漏判（留在正文里）也不要误判（把正文词抠成 chip）。 */
export function isRefToken(tok: string): boolean {
  if (!tok.startsWith("@") || tok.length < 2) return false;
  const body = tok.slice(1);
  if (body.includes("@")) return false; // `@a@b` 不是引用
  const lastSeg = body.split(/[\\/]/).pop() ?? "";
  const hasExt = /\.[A-Za-z0-9]{1,10}$/.test(lastSeg);
  if (!/[\\/]/.test(body)) return hasExt; // report.xlsx ✓ / 用户名 ✗
  // 有分隔符：末段带扩展名（src/a.ts）或整体是绝对形态（C:\x、\\srv\share、/tmp/x、~/x）
  return hasExt || /^(?:[A-Za-z]:[\\/]|\\\\|\/|~)/.test(body);
}

/** 把正文里形如引用的 token 抽成 refs，其余文本原样保留（只 trim 两端，内部分隔空白不动）；
 *  移除 token 时吃掉它左边那段空白（换行不算空白块，行结构因此保住），免得原地留下双空格。 */
export function splitRefs(text: string): { text: string; refs: string[] } {
  const parts = text.split(/(\s+)/); // 保留分隔符：重组时中间空白逐字节还原
  const refs: string[] = [];
  const kept: string[] = [];
  for (const p of parts) {
    if (isRefToken(p)) {
      const ref = p.slice(1);
      if (!refs.includes(ref)) refs.push(ref);
      if (kept.length && /^[ \t]+$/.test(kept[kept.length - 1])) kept.pop();
      continue;
    }
    kept.push(p);
  }
  return { text: kept.join("").trim(), refs };
}

/** 合成发送正文：正文在前、引用追加末尾（与历史上 appendRefs 的落点一致）；
 *  正文里已经写着同一个引用时不重复追加（手打路径 + chip 并存的情形）。 */
export function mergeRefs(text: string, refs: string[]): string {
  const body = text.trim();
  const seen = new Set<string>();
  const extra = refs.filter((r) => {
    if (!r || seen.has(r)) return false;
    seen.add(r);
    return !body.includes(`@${r}`);
  });
  return [body, ...extra.map((r) => `@${r}`)].filter(Boolean).join(" ");
}

/** 回填路径专用的安全解析（历史召回 / 队列「编辑」/ 用户消息「修改」回填）：
 *  只有解析结果能**逐字节**还原原文时才采用——即只认 app 自己产出的「正文 + 末尾 `@引用`」形态。
 *  手打形态一律不解析、正文原样留在输入框里，典型有三类：
 *   - 引用在句子中间（解析会把 token 搬到末尾，改动用户的行文）；
 *   - 路径含空格（`@C:\\My Documents\\a.xlsx` 会被 token「遇空格即断」截成半个路径）；
 *   - 引用独占一行（移除后会多出一个空行）。
 *  宁可不好看，也不要把用户写的内容搬走、截断或凭空多出空行。 */
export function recoverRefs(text: string): { text: string; refs: string[] } {
  const parsed = splitRefs(text);
  if (parsed.refs.length === 0) return parsed;
  return mergeRefs(parsed.text, parsed.refs) === text ? parsed : { text, refs: [] };
}

/** 追加引用到草稿（去重、保首次出现顺序）；空串与重复项静默丢弃。 */
export function addRefs(current: string[], incoming: string[]): string[] {
  const out = [...current];
  for (const r of incoming) if (r && !out.includes(r)) out.push(r);
  return out;
}
