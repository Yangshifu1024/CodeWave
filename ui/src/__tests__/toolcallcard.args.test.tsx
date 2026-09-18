// 工具卡失败态可读性（fix）：失败调用的出参恒为 Null → 展开体只剩 `{}`，无从判断失败原因。
// 本文件锁定两处补齐：
//   1) 出参为空且有入参时，兜底 JSON 分支回退渲染「入参」（附 dim「入参」标签）；
//   2) 摘要白名单补 `todos` 一条（plan 的入参键），使失败卡收起态也能看出 todo 规模与进行中数量。
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, fireEvent } from "@testing-library/react";
import { i18n } from "../i18n"; // i18n init（直接渲染组件不会触发初始化，t() 否则返回 key 本身）
import ToolCallCard from "../features/tools/ToolCallCard";
import type { ToolView } from "../stores/run";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

function toolView(partial: Partial<ToolView>): ToolView {
  return {
    callKey: "b1:0",
    tool: "read",
    status: "ok",
    progressTail: "",
    ...partial,
  };
}

/** 12 项 todo，其中 2 项 in_progress —— 与 plan 报「同一时刻最多一个 in_progress」的真实场景同构 */
function planTodos() {
  return [
    { content: "梳理工具卡失败态", status: "in_progress", priority: "high" },
    { content: "补齐入参回退渲染", status: "in_progress", priority: "high" },
    { content: "补测试", status: "pending", priority: "medium" },
    { content: "跑 cargo test", status: "pending", priority: "medium" },
    { content: "跑前端测试", status: "pending", priority: "medium" },
    { content: "跑构建", status: "pending", priority: "medium" },
    { content: "更新文档", status: "pending", priority: "low" },
    { content: "登记 README", status: "pending", priority: "low" },
    { content: "自查范围外文件", status: "pending", priority: "low" },
    { content: "对齐接口", status: "pending", priority: "low" },
    { content: "清理临时文件", status: "pending", priority: "low" },
    { content: "汇报", status: "pending", priority: "low" },
  ];
}

const planFailOutcome = (message = "同一时刻最多一个 in_progress 任务") => ({
  ok: false,
  data: null,
  error: { code: "E_PLAN_INVALID", message },
});

function headText(): string {
  const el = document.querySelector<HTMLElement>(".tool-card .tool-head");
  if (!el) throw new Error("tool-head not found");
  return el.textContent ?? "";
}

function summaryText(): string {
  const el = document.querySelector<HTMLElement>(".tool-card .summary");
  if (!el) throw new Error("summary span not found");
  return el.textContent ?? "";
}

function bodyText(): string {
  const el = document.querySelector<HTMLElement>(".tool-card .tool-body");
  if (!el) throw new Error("tool-body not found（展开未生效？）");
  return el.textContent ?? "";
}

/** 展开：点击卡片头部（与既有 toolcallcard.test.tsx 的 DOM 约定一致） */
function expand() {
  const head = document.querySelector<HTMLElement>(".tool-card .tool-head");
  if (!head) throw new Error("tool-head not found");
  fireEvent.click(head);
}

afterEach(() => {
  cleanup();
});

describe("ToolCallCard 失败态回退渲染入参", () => {
  it("① plan 失败（data=null）：展开体渲染 todos 入参 JSON，不再是 `{}`", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "plan",
          status: "error",
          argsPreview: JSON.stringify({ todos: planTodos() }),
          outcome: planFailOutcome(),
        })}
      />,
    );
    expand();
    const text = bodyText();
    // 关键证据：能读到 todos 的 JSON（状态值与标题），而失败出参为空 —— 修复前这里是 `{}`
    expect(text).toContain("in_progress");
    expect(text).toContain("梳理工具卡失败态");
    expect(text).toContain("补齐入参回退渲染");
    expect(text).toContain("入参"); // dim 标签，区分入参 / 出参
    expect(text).not.toContain("{}");
    // 结构未变：错误行仍在入参 JSON 之后
    expect(text).toContain("E_PLAN_INVALID");
    expect(text).toContain("同一时刻最多一个 in_progress 任务");
    expect(document.querySelector(".tool-card.st-error")).not.toBeNull();
  });

  it("①b 出参为 `{}`（非 null，如 ask 未作答类）同样回退渲染入参", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "ask",
          status: "error",
          argsPreview: JSON.stringify({ question: "要不要继续？" }),
          outcome: { ok: false, data: {}, error: { code: "E_ASK_CANCELLED", message: "用户取消或未回答" } },
        })}
      />,
    );
    expand();
    expect(bodyText()).toContain("要不要继续？");
  });

  it("①c 无 argsPreview：保持既有行为（渲染空出参），不抛异常", () => {
    render(<ToolCallCard tool={toolView({ tool: "plan", status: "error", outcome: planFailOutcome() })} />);
    expect(() => expand()).not.toThrow();
    const text = bodyText();
    expect(text).toContain("{}");
    expect(text).not.toContain("入参");
  });

  it("③ 成功且 data 非空：兜底分支仍渲染出参，不渲染入参", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "calculate",
          status: "ok",
          argsPreview: JSON.stringify({ expression: "6*7" }),
          outcome: { ok: true, data: { result: 42 } },
        })}
      />,
    );
    expand();
    const text = bodyText();
    expect(text).toContain("result");
    expect(text).toContain("42");
    expect(text).not.toContain("expression");
    expect(text).not.toContain("入参");
  });

  it("④a 入参过大（edit）：既有「参数过大」文案不变", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "edit",
          status: "ok",
          argsPreview: JSON.stringify({ _args_truncated: true, hint: "参数过大，前端不展示 diff" }),
          outcome: { ok: true, data: { edited: ["a.md"], count: 1 } },
        })}
      />,
    );
    expand();
    expect(bodyText()).toContain(i18n.t("tools.argsTooLarge"));
  });

  it("④b 入参过大 + 失败出参（plan）：不解析截断标记，不抛异常", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "plan",
          status: "error",
          argsPreview: JSON.stringify({ _args_truncated: true, hint: "参数过大，前端不展示 diff" }),
          outcome: planFailOutcome(),
        })}
      />,
    );
    expect(() => expand()).not.toThrow();
    const text = bodyText();
    // 截断标记不是真实入参 JSON：既不渲染 hint（未解析）也不渲染「入参」标签，回落到空出参
    expect(text).not.toContain("参数过大，前端不展示 diff");
    expect(text).not.toContain("入参");
    expect(text).toContain("{}");
  });
});

describe("ToolCallCard 头部摘要支持 plan 的 todos", () => {
  it("② plan 有进行中项：摘要 `12 项 · 2 进行中`", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "plan",
          status: "error",
          argsPreview: JSON.stringify({ todos: planTodos() }),
          outcome: planFailOutcome(),
        })}
      />,
    );
    expect(summaryText()).toBe("12 项 · 2 进行中");
    expect(headText()).toContain("12 项 · 2 进行中");
  });

  it("⑤ plan 无进行中项：摘要只显示 `12 项`", () => {
    const todos = planTodos().map((t) => ({ ...t, status: "pending" }));
    render(<ToolCallCard tool={toolView({ tool: "plan", argsPreview: JSON.stringify({ todos }) })} />);
    expect(summaryText()).toBe("12 项");
  });

  it("⑤b plan 全完成：同样只显示 `12 项`", () => {
    const todos = planTodos().map((t) => ({ ...t, status: "completed" }));
    render(<ToolCallCard tool={toolView({ tool: "plan", argsPreview: JSON.stringify({ todos }) })} />);
    expect(summaryText()).toBe("12 项");
  });

  it("⑥ 非 plan 工具（grep 含 pattern）仍优先走 pattern，不被 todos 分支影响", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "grep",
          argsPreview: JSON.stringify({ pattern: "in_progress", todos: [] }),
          outcome: { ok: true, data: { matches: [], total: 0 } },
        })}
      />,
    );
    expect(summaryText()).toBe("/in_progress/");
  });

  it("⑥b url/path/command 优先级不变（带 todos 的入参也不抢摘要）", () => {
    render(
      <ToolCallCard
        tool={toolView({
          tool: "web_fetch",
          argsPreview: JSON.stringify({ url: "https://example.com/doc", todos: planTodos() }),
          outcome: { ok: true, data: { status: 200, content: "x" } },
        })}
      />,
    );
    expect(summaryText()).toBe("https://example.com/doc");
  });
});
