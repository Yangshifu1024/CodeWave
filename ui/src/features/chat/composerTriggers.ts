// Composer 三个触发符（`/` 技能、`$` 子代理、`@` 提及）的**光标感知**判定。
//
// 改造前：判定全部锚定「整段文本的末尾」且完全不读光标——`/` 要求整段就是 `/…`（`^/(\S*)$`，只要正文里
// 还有别的字就永不命中），`@`/`$` 只认「结尾片段」（光标在中间/开头时，片段后面还跟着正文 → 不命中，
// 查询词也被算成「触发符到文末」）。于是「正文里已有内容」时根本拉不起菜单。
//
// 现在：只看**光标之前的文本**，取光标前那段不含空白的片段（片段起点天然就是行首或空白之后），
// 光标之后的正文既不参与判定、也不被回填改动。
//
// 各触发符的门禁（有意不一致，跟随模型侧契约而不是 UI 手感）：
// - `/`、`$`：片段必须从**消息 offset 0** 开始（即消息以它开头）。模型侧规则写死
//   「消息以 /<name> 开头 = 点名该技能」/「消息以 $<role> 开头 = 点名副代理」
//   （`src-tauri/src/skills/mod.rs` 的 prompt_listing 与 `core/prompt.rs` 的 CORE_PROMPT），
//   放宽到句中会让用户以为点名了、其实没点名——所以只修「消息开头打、后面已有正文时打不开」。
// - `@`：片段起点落在行首或空白之后即可（片段本身不含空白）→ 正文任意位置可用；`me@x.com` 不再误弹。
//
// 判定错了也不会有功能风险：回填只替换 [start, caret) 这段片段，光标之后的字一字不动。

export type TriggerKind = "slash" | "dollar" | "at";

/** 命中的触发片段：`start` 是片段起点（含触发符），`end` 是光标位置。 */
export interface TriggerHit {
  kind: TriggerKind;
  /** 触发符之后的查询串（交给 refreshSkills / refreshMention / refreshAgents） */
  query: string;
  /** 回填时替换 `[start, end)` */
  start: number;
  end: number;
}

/** 光标位置校正：越界、非有限数、缺省 → 夹到 `[0, text.length]`（缺省按「文末」理解）。 */
export function clampCaret(text: string, caret: number | null | undefined): number {
  if (caret == null || !Number.isFinite(caret)) return text.length;
  return Math.max(0, Math.min(text.length, Math.trunc(caret)));
}

/** 光标前那段「不含空白的片段」；起点即行首或空白之后（`text` 为片段本身）。 */
export function fragmentBefore(text: string, caret: number): { start: number; text: string } {
  const c = clampCaret(text, caret);
  const frag = text.slice(0, c).match(/[^\s]*$/)?.[0] ?? "";
  return { start: c - frag.length, text: frag };
}

/** 光标处的触发片段（无则 null）。三者在各自门禁下互斥，优先级与改造前一致：`/` > `$` > `@`。 */
export function detectTrigger(text: string, caret: number): TriggerHit | null {
  const c = clampCaret(text, caret);
  const frag = fragmentBefore(text, c);
  const body = frag.text;
  if (!body) return null;
  const head = body[0];
  const tail = body.slice(1);
  // `/`：消息开头（offset 0）且片段内无空白
  if (head === "/" && frag.start === 0) return { kind: "slash", query: tail, start: 0, end: c };
  // `$`：同上；片段内不得再出现 `$`（与原 `[^$\s]*` 口径一致）
  if (head === "$" && frag.start === 0 && !tail.includes("$")) {
    return { kind: "dollar", query: tail, start: 0, end: c };
  }
  // `@`：片段起点即行首/空白之后；片段内不得再出现 `@`（与原 `[^@\s]*` 口径一致）
  if (head === "@" && !tail.includes("@")) {
    return { kind: "at", query: tail, start: frag.start, end: c };
  }
  return null;
}
