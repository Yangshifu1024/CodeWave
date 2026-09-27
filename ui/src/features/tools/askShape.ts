// 选项互斥语义启发式推断（[docs/ask-plan-card-and-option-shape](../../../docs/ask-plan-card-and-option-shape.md)）。
// 形态优先级（AskPanel 调用方口径）：批准 > 显式 single=true > 显式 single=false > 启发式 > 多选兑底。
// 模型未声明 single 时，按本函数的判定结果决定 radio / checkbox 渲染；显式 single=false 永远多选。
import type { AskOptionPayload, AskQuestionPayload } from "../../ipc/types";

/** 「只能」「单选」类强约束（一句话里两个标记同时出现也算）。 */
const FIRST_WORD = /(只能|单选)/;
/** 「要么…要么…」二者择一。问句里必须成对出现，否则不立。 */
const EITHER_PAIR = /要么[\s\S]*?要么/;
/** 题干以「选一个」/「哪一种」/…收尾（尾随任意字符也算）。 */
const QUESTION_TAIL = /(选一个|哪一种|走哪种|选中|是否按[\s\S]*?执行|选哪个)\s*$/;
/** 多选关键词：命中任一即默认多选。 */
const MULTI_KEY = /(同时|可多|多选|哪些)/;
/** 题干多选语境：题面里出现这类词 → 强默认多选（不靠选项 label 兑底）。 */
const MULTI_QUESTION = /(勾选所有|请勾选|哪些项|多项|多选|请选择所有|同时适用|全选)/;
/** 选项 id 上的强约束（大小写不敏感）。 */
const SINGLE_ID = /(mutually_exclusive|single_choice)/;

/**
 * 判定一道非批准的题应渲染为 radio（互斥）还是 checkbox（多选）。
 * - `q.single === true` 永远 radio；`q.single === false` 永远多选。
 * - `q` 为 undefined 时安全返回 false（多选兑底）。
 * - 选项 < 1 时返回 false（题目为空，按多选兑底）。
 */
export function inferSingle(q: AskQuestionPayload | undefined): boolean {
  if (!q) return false;
  // 显式互斥 / 多选优先于启发式
  if (q.single === true) return true;
  if (q.single === false) return false;
  const opts = q.options ?? [];
  if (opts.length < 1) return false;
  const labels = opts.map((o) => String((o as AskOptionPayload)?.label ?? "").toLowerCase());
  const ids = opts.map((o) => String((o as AskOptionPayload)?.id ?? "").toLowerCase());
  // ① 选项 id 含「mutually_exclusive」或「single_choice」
  if (ids.some((s) => SINGLE_ID.test(s))) return true;
  // ② 选项 label 首词命中「只能」/「单选」/「要么…要么…」(后者需成对出现)
  if (labels.some((l) => FIRST_WORD.test(l))) return true;
  if (labels.some((l) => EITHER_PAIR.test(l))) return true;
  // ③ 题干以「选一个」/「哪一种」/…收尾
  const tail = String(q.question ?? "");
  if (QUESTION_TAIL.test(tail)) return true;
  // ③ 题干含多选语境（勾选所有 / 请勾选 / 哪些项 / 多项…）→ 多选优先于 ④ 的 radio 判底
  if (MULTI_QUESTION.test(tail)) return false;
  // ④ 选项数 ≤ 4 且任一选项 label 含「同时」/「可多」/「多选」/「哪些」 → 反转：多选
  if (opts.length <= 4 && labels.some((l) => MULTI_KEY.test(l))) return false;
  // ④ 选项数 ≤ 4 且无多选关键词 → radio
  if (opts.length <= 4) return true;
  // 选项数 > 4 → 多选兑底
  return false;
}
