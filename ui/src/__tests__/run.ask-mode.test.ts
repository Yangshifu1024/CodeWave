// 批准门选档（[docs/mode-gate-and-subagent-sync]）store 层：ask:opened 载荷的选项 `mode` 透传到 AskState，
// AskPanel 据此判定「批准类选项」（直提 + 按实际档位同步胶囊）。
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

function seedTab(session: string) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [], running: false, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
  useSessions.setState({ activeKey: session });
}

describe("ask:opened 批准门选档", () => {
  it("选项的 mode 原样落到 AskState.questions；plan_file / allow_always 一并映射", () => {
    const session = "s-mode";
    seedTab(session);
    const h = useRun.getState().bindGlobalHandlers();
    h["ask:opened"]({
      session, ask_id: "a1", kind: "ask", approval: true,
      plan_file: "/ws/.codewave/tasks/plan-x.md", allow_always: true,
      questions: [{
        id: "q1", question: "【方案】第一步",
        options: [
          { id: "mode_auto", label: "以自动编辑档执行", mode: "auto_edit", recommended: true },
          { id: "mode_full", label: "以完全访问档执行", mode: "full_access" },
          { id: "revise", label: "补充意见" },
        ],
      }],
    });
    const ask = useRun.getState().tabs[session]!.ask!;
    expect(ask.planFile).toBe("/ws/.codewave/tasks/plan-x.md");
    expect(ask.allowAlways).toBe(true);
    expect(ask.questions![0].options!.map((o) => o.mode)).toEqual(["auto_edit", "full_access", undefined]);
  });

  it("旧载荷（选项无 mode）不因类型收敛而丢字段：id / label / recommended 照常", () => {
    const session = "s-legacy";
    seedTab(session);
    const h = useRun.getState().bindGlobalHandlers();
    h["ask:opened"]({
      session, ask_id: "a2", kind: "ask",
      questions: [{ id: "q1", question: "选一个", options: [{ id: "approve", label: "执行方案", recommended: true }] }],
    });
    const ask = useRun.getState().tabs[session]!.ask!;
    expect(ask.questions![0].options![0]).toEqual({ id: "approve", label: "执行方案", recommended: true });
    expect(ask.questions![0].options![0].mode).toBeUndefined();
  });
});
