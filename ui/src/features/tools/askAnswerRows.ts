// ask 卡片的问答行转换（纯函数，便于单测）：入参 `questions`（题干 + 选项 id→中文标签）
// + 出参 `raw.answers`（每题 selections + note）→「题干一行 / 答案一行」的行数据。
//
// 规则（用户要求）：题干与答案**永远完整显示**（不省略、不依赖展开）；答案按中文标签展示、
// 多选以「、」连接；备注单独一行；只有备注没有选项时答案就是备注；都没答显示「未回答」。
// 找不到对应标签的 id 原样显示（不猜）。

export interface AskAnswerRow {
  /** 题干（完整，不截断；长文本由样式换行） */
  question: string;
  /** 答案行 */
  answer: string;
  /** 备注原文（空串 = 没有；已并入答案时同样为空） */
  note: string;
}

interface AskOption {
  id?: unknown;
  label?: unknown;
}
interface AskQuestion {
  id?: unknown;
  question?: unknown;
  options?: unknown;
}

/**
 * 入参 + 出参 → 问答行；任一前提缺失（入参截断、无出参、无 questions）时返回空数组，
 * 调用方回退到原始数据视图。
 */
export function askAnswerRows(
  argsPreview: string | undefined,
  data: unknown,
  texts: { notAnswered: string },
): AskAnswerRow[] {
  const questions = parseQuestions(argsPreview);
  if (questions.length === 0) return [];
  const answers = (data as { raw?: { answers?: unknown } } | null | undefined)?.raw?.answers;
  if (!answers || typeof answers !== "object") return [];
  const table = answers as Record<string, { selections?: unknown; note?: unknown }>;

  return questions.map((q) => {
    const qid = typeof q.id === "string" ? q.id : "";
    const ans = table[qid] ?? {};
    const options = Array.isArray(q.options) ? (q.options as AskOption[]) : [];
    const selected = Array.isArray(ans.selections)
      ? ans.selections.filter((s): s is string => typeof s === "string")
      : [];
    const labels = selected.map((id) => {
      const hit = options.find((o) => o?.id === id);
      return typeof hit?.label === "string" && hit.label !== "" ? hit.label : id;
    });
    const note = typeof ans.note === "string" ? ans.note.trim() : "";
    const question = typeof q.question === "string" ? q.question : qid;

    // 只有备注（自由输入作答）：答案就是备注本身，不再另起一行
    if (labels.length === 0) {
      return { question, answer: note !== "" ? note : texts.notAnswered, note: "" };
    }
    return { question, answer: labels.join("、"), note };
  });
}

/** 入参 JSON → questions；解析不出来（被截断 / 非 JSON）返回空数组 */
function parseQuestions(argsPreview: string | undefined): AskQuestion[] {
  if (typeof argsPreview !== "string") return [];
  const s = argsPreview.trim();
  if (!s.startsWith("{")) return [];
  try {
    const v = JSON.parse(s);
    return Array.isArray(v?.questions) ? (v.questions as AskQuestion[]) : [];
  } catch {
    return [];
  }
}
