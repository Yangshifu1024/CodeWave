// ask 卡片问答呈现（用户要求）：回答完**不展开**就能看到「题干一行 / 答案一行」，不省略；
// 展开后只看原始数据。本文件同时钉住纯函数 askAnswerRows 的各类边界。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { i18n } from "../i18n";
import ToolCallCard from "../features/tools/ToolCallCard";
import { askAnswerRows } from "../features/tools/askAnswerRows";
import type { ToolView } from "../stores/run";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const ARGS = JSON.stringify({
  questions: [
    {
      id: "approve_plan",
      question: "按上面的方案开工吗？分支 fix/image-vision-context。",
      options: [
        { id: "approve", label: "批准开发" },
        { id: "revise", label: "补充意见" },
      ],
    },
  ],
});

const NOT_ANSWERED = i18n.t("tools.notAnswered");

function askCard(partial: Partial<ToolView>): ToolView {
  return {
    callKey: "b1:0",
    tool: "ask",
    status: "ok",
    progressTail: "",
    argsPreview: ARGS,
    ...partial,
  };
}

function body(): string {
  return document.body.textContent ?? "";
}

afterEach(() => cleanup());

describe("askAnswerRows（纯函数）", () => {
  it("单选 + 备注：答案用中文标签，备注原样返回", () => {
    const rows = askAnswerRows(
      ARGS,
      { raw: { answers: { approve_plan: { selections: ["revise"], note: "先把文档补上" } } } },
      { notAnswered: NOT_ANSWERED },
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]!.question).toContain("按上面的方案开工吗");
    expect(rows[0]!.answer).toBe("补充意见");
    expect(rows[0]!.note).toBe("先把文档补上");
  });

  it("多选以「、」连接；未答显示「未回答」", () => {
    const rows = askAnswerRows(
      JSON.stringify({
        questions: [
          { id: "q1", question: "选哪些？", options: [{ id: "a", label: "甲" }, { id: "b", label: "乙" }] },
          { id: "q2", question: "还有吗？", options: [{ id: "c", label: "丙" }] },
        ],
      }),
      { raw: { answers: { q1: { selections: ["a", "b"], note: "" }, q2: { selections: [], note: "" } } } },
      { notAnswered: NOT_ANSWERED },
    );
    expect(rows[0]!.answer).toBe("甲、乙");
    expect(rows[1]!.answer).toBe(NOT_ANSWERED);
  });

  it("只有备注（自由输入作答）：答案就是备注，不另起一行", () => {
    const rows = askAnswerRows(
      JSON.stringify({ questions: [{ id: "q1", question: "怎么说？", options: [{ id: "a", label: "甲" }] }] }),
      { raw: { answers: { q1: { selections: [], note: "用 B 方案" } } } },
      { notAnswered: NOT_ANSWERED },
    );
    expect(rows[0]!.answer).toBe("用 B 方案");
    expect(rows[0]!.note).toBe("");
  });

  it("找不到对应标签的 id：原样显示，不猜", () => {
    const rows = askAnswerRows(
      JSON.stringify({ questions: [{ id: "q1", question: "选？", options: [{ id: "a", label: "甲" }] }] }),
      { raw: { answers: { q1: { selections: ["zzz"], note: "" } } } },
      { notAnswered: NOT_ANSWERED },
    );
    expect(rows[0]!.answer).toBe("zzz");
  });

  it("入参截断 / 无出参 / 无 questions：返回空数组（回退原始数据视图）", () => {
    const texts = { notAnswered: NOT_ANSWERED };
    expect(askAnswerRows(undefined, { raw: { answers: {} } }, texts)).toEqual([]);
    expect(askAnswerRows(ARGS, null, texts)).toEqual([]);
    expect(askAnswerRows('{"_args_truncated":true}', { raw: { answers: {} } }, texts)).toEqual([]);
    expect(askAnswerRows('{"questions":[]}', { raw: { answers: {} } }, texts)).toEqual([]);
  });
});

describe("ask 卡片渲染", () => {
  it("回答完不展开就能看到题干与答案（含备注行）", () => {
    render(
      <ToolCallCard
        tool={askCard({
          outcome: {
            ok: true,
            data: { raw: { answers: { approve_plan: { selections: ["approve"], note: "开工" } } } },
          },
        })}
      />,
    );
    // 未点击 .tool-head → 收起态
    expect(body()).toContain("按上面的方案开工吗");
    expect(body()).toContain("批准开发");
    expect(body()).toContain(i18n.t("tools.notePrefix"));
    expect(body()).toContain("开工");
    // 收起态不渲染原始 JSON
    expect(body()).not.toContain("approve_plan");
  });

  it("未回答的题显示「未回答」", () => {
    render(
      <ToolCallCard
        tool={askCard({
          outcome: { ok: true, data: { raw: { answers: { approve_plan: { selections: [], note: "" } } } } },
        })}
      />,
    );
    expect(body()).toContain(NOT_ANSWERED);
  });

  it("展开后展示原始数据（问答行仍在）", () => {
    render(
      <ToolCallCard
        tool={askCard({
          outcome: {
            ok: true,
            data: { raw: { answers: { approve_plan: { selections: ["approve"], note: "" } } } },
          },
        })}
      />,
    );
    fireEvent.click(document.querySelector(".tool-head")!);
    expect(body()).toContain(i18n.t("tools.rawData"));
    expect(body()).toContain("approve_plan");
    expect(body()).toContain("批准开发");
  });

  it("被忽略（E_ASK_NOT_ANSWERED）：不渲染问答行，头部保持中性文案", () => {
    render(
      <ToolCallCard
        tool={askCard({
          status: "error",
          outcome: { ok: false, error: { code: "E_ASK_NOT_ANSWERED", message: "" }, data: null },
        })}
      />,
    );
    expect(body()).toContain(i18n.t("tools.notAnswered"));
    expect(document.querySelector(".tool-card .ask-rows")).toBeNull();
  });
});
