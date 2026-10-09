// 内部提示在恢复历史时的可见性（[docs/main-run-finish-with-pending-todos](../../../docs/main-run-finish-with-pending-todos.md) §8.4）：
//
//   后端 drive 把 `<continue-notice>` / `<tool-args-rejected>` / `<text-turn-limit>` /
//   `<budget-notice>` / `<final-report>` / `<supervision-notice>` / `<supervision-escalated>`
//   以 **user 消息**注入 `rt.history`——必须进（要出网给模型看，否则就是「提示注入却永不被
//   模型看到」的静默成功缺陷）。但它们是给模型看的内部指令，重开历史时以 user 气泡原样
//   展示 XML 标签既难看，又让人误以为是自己发的。
//
//   口径：**只改展示**。落盘数据 / 恢复数据 / wire 三者保持一致，模型仍能在历史里看到它们。
//   判别力核心：整条消息恰好是某标签（成对包裹）→ notice；用户自己在正文里**提到**标签
//   （前后有别的文字）→ 仍是 user 气泡，绝不误伤。
//
// 惯例：唯一 invoke 入口 ui/src/ipc/client 必须 mock；zustand store 是模块级单例，
// 逐用例重建态桶（AGENTS.md 踩坑清单）。
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const ipcMock = vi.hoisted(() => ({
  loadSession: vi.fn(async (): Promise<unknown> => ({ messages: [] })),
  listSessions: vi.fn(async (): Promise<unknown[]> => []),
  listProjects: vi.fn(async (): Promise<unknown[]> => []),
  getSessionPrefs: vi.fn(async () => ({ approval_mode: "auto_edit", model_id: null, reasoning_effort: null })),
  restoreLegacyModelPrefs: vi.fn(async () => undefined),
  sessionRunning: vi.fn(async () => false),
  gitStatus: vi.fn(async () => ({ repo: false, entries: [] })),
  getTokenBreakdown: vi.fn(async () => null),
  mcpConnect: vi.fn(async () => undefined),
  getUiState: vi.fn(async () => null),
  setUiState: vi.fn(async () => undefined),
}));

vi.mock("../ipc/client", () => ({ ipc: ipcMock }));

import { i18n } from "../i18n";
import type { Message } from "../ipc/types";
import { useRun } from "../stores/run";

/** 以 user 角色构造一条纯文本历史消息 */
const userMsg = (text: string): Message =>
  ({ role: "user", content: [{ type: "text", text }] }) as unknown as Message;

const assistantMsg = (text: string): Message =>
  ({ role: "assistant", content: [{ type: "text", text }] }) as unknown as Message;

/** 后端 drive.rs 注入的全部内部提示标签（与 INTERNAL_HINT_TAGS 同源，测试侧独立列一遍钉住契约） */
/** 后端 drive.rs 注入的全部内部提示标签（与 INTERNAL_HINT_TAGS 同源，测试侧独立列一遍钉住契约）。
 *  **正文逐字取自生产代码**，不得自行编造形态——否则可能在验证一个不存在的形态
 *  （code-review 2026-10-10 就是这么抓到 `<final-report>` 缺闭合标签的漏判）。 */
const HINT_BODIES = [
  // drive.rs continue_notice()
  "<continue-notice>你在上一回合只输出了文字、没有发起工具调用。若任务尚未完成，立即继续调用工具推进；全部完成时以 <report>…</report> 包裹输出最终汇报。</continue-notice>",
  // drive.rs continue_notice_pending_todos()
  "<continue-notice>你在上一回合只输出了文字、没有发起工具调用，而当前计划仍有未完成项：补单测；验证\n请立即调用工具推进；若这些待办其实已完成或不再需要，先用 plan 更新计划（把已完成项标为 completed、删除无关项）。再次只输出文字将被视为收尾。</continue-notice>",
  // drive.rs tool_args_rejected_notice()
  "<tool-args-rejected>你上一回合有 1 个工具调用因参数 JSON 无法解析而被拒绝、未执行。\n请修正参数后重新发起该调用；不要就此结束任务。</tool-args-rejected>",
  // drive.rs StopWithLimit（纯文本连转超限分支）
  "<text-turn-limit>连续多步未能发起有效工具调用（只输出文字，或调用参数反复不可解析），本 run 已终止。若用户重新发起运行，先用 ask 工具确认：继续（说明已准备的新推进方式）或就此收尾。</text-turn-limit>",
  // drive.rs budget_notice
  "<budget-notice>步数预算即将耗尽，请尽快收敛并输出汇报。</budget-notice>",
  // drive.rs force_report 末步（成对标签）
  "<final-report>已到达步数上限。停止调用工具，立即输出最终汇报：已完成、未完成、结论。</final-report>",
  // core/agent/supervise.rs 纠偏
  "<supervision-notice>你在重复同一个失败调用，请换一种推进方式。</supervision-notice>",
  // drive.rs 监督终止
  "<supervision-escalated>上一次 run 因连续多步无实质进展而被监督终止。若用户重新发起运行，先用 ask 工具向用户确认：继续（换一种推进方式，如直接执行/提问/换文件）或就此收尾；未经用户选择，不要继续重复读取。</supervision-escalated>",
];

/** 恢复历史并取出转录项（走真实 restoreFromMessages —— buildTranscript 的唯一入口） */
function restore(messages: Message[]) {
  useRun.getState().restoreFromMessages("s1", messages);
  return useRun.getState().tabs["s1"]!.items;
}

const kinds = (items: { kind: string }[]) => items.map((i) => i.kind);
const userTexts = (items: { kind: string; text?: string }[]) =>
  items.filter((i) => i.kind === "user").map((i) => i.text ?? "");

beforeEach(() => {
  vi.clearAllMocks();
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

afterEach(() => {
  useRun.setState((s) => {
    s.tabs = {};
    s.drafts = {};
  });
});

describe("内部提示恢复：渲染为 notice 而非 user 气泡（§8.4）", () => {
  it("全部七个标签各自命中 → notice，且文案是 i18n 的通用说明（不泄漏 XML 原文）", () => {
    for (const body of HINT_BODIES) {
      const items = restore([userMsg(body)]);
      expect(kinds(items), `未命中 notice：${body}`).toEqual(["notice"]);
      const text = (items[0] as { text: string }).text;
      expect(text).toBe(i18n.t("notice.internalHint"));
      // 绝不把 XML 标签原文透给用户
      expect(text).not.toContain("<continue-notice>");
      expect(text).not.toContain("<tool-args-rejected>");
      expect(text).not.toContain("<supervision-notice>");
      expect(text).not.toContain("<");
    }
  });

  it("判别力：用户自己在正文里提到标签（前后有别的文字）→ 仍是 user 气泡，不被误伤", () => {
    // ① 前面有前缀
    expect(kinds(restore([userMsg("我看到日志里有 <continue-notice> 这个东西")]))).toEqual(["user"]);
    // ② 后面有后缀（标签未闭合到结尾）
    expect(
      kinds(restore([userMsg("<continue-notice>你在上一回合只输出了文字。</continue-notice> 顺便问一下这是什么？")])),
    ).toEqual(["user"]);
    // ③ 标签未闭合
    expect(kinds(restore([userMsg("<continue-notice>你在上一回合只输出了文字。")]))).toEqual(["user"]);
    // ④ 提到多个标签的普通提问
    expect(
      kinds(restore([userMsg("<tool-args-rejected> 和 <text-turn-limit> 有什么区别？")]))
    ).toEqual(["user"]);
  });

  it("首尾空白容错：标签外有换行/缩进仍判定为内部提示", () => {
    const items = restore([userMsg("\n  <continue-notice>正文。</continue-notice>  \n")]);
    expect(kinds(items)).toEqual(["notice"]);
  });

  it("只改展示：提示仍占历史一条（不删除消息、不改变消息顺序）", () => {
    const items = restore([
      userMsg("第一条提问"),
      userMsg(HINT_BODIES[0]),
      assistantMsg("回答"),
      userMsg("第二条提问"),
    ]);
    // 4 条消息 → 4 个转录项（提示没被丢掉，只换了呈现形态）
    expect(items).toHaveLength(4);
    expect(kinds(items)).toEqual(["user", "notice", "assistant", "user"]);
    expect(userTexts(items)).toEqual(["第一条提问", "第二条提问"]);
  });

  it("非标签 user 消息零打扰（含 handoff 压缩摘要与 run-cancelled 各自的既有分支）", () => {
    expect(kinds(restore([userMsg("普通提问")]))).toEqual(["user"]);
    // 压缩摘要沿用既有 includes 分支，不得被本次改动抢走
    const compact = restore([userMsg("[handoff-summary] 前文摘要如下……")]);
    expect(kinds(compact)).toEqual(["notice"]);
    expect((compact[0] as { text: string }).text).toBe(i18n.t("notice.compactSummary"));
    // run-cancelled 独立分支
    const cancelled = restore([userMsg("<run-cancelled/>")]);
    expect(kinds(cancelled)).toEqual(["notice"]);
    expect((cancelled[0] as { text: string }).text).toBe(i18n.t("notice.cancelled"));
  });
});