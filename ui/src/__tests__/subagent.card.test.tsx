// SubagentItemCard 收尾原因徽标（defect: 子代理提前退出却显示绿色成功态）：
// ended = "no_report" / "budget" 视为疑似提前结束（橙色警示），ended 缺省 / "report" 保持绿勾（旧数据向后兼容）。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import "../i18n"; // directly mounted components must explicitly init i18next (no global entry outside App.tsx)

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: any = null;
  },
}));

import SubagentItemCard from "../features/subagent/SubagentItemCard";
import { useRun, type SubView } from "../stores/run";
import { useSessions } from "../stores/sessions";

function subView(over: Partial<SubView> = {}): SubView {
  return {
    subId: "sub_1", role: "frontend-dev", name: "frontend-dev", description: "实现卡片",
    task: "实现子代理卡收尾徽标", step: 22, maxSteps: 80, tokens: 1200, lastTools: [],
    status: "done", ...over,
  };
}

function seedTab(session: string, sub: SubView) {
  useRun.setState((s) => {
    s.tabs[session] = {
      items: [{ kind: "assistant", timeline: [{ kind: "sub", subId: sub.subId }], toolsMap: {}, streaming: false }] as any,
      running: false,
      streamGen: 0,
      ask: null,
      breakdown: null,
      todos: [],
      suggestions: [],
      subs: [sub],
      subStreams: { [sub.subId]: { timeline: [], toolsMap: {}, status: sub.status, gen: 0, loaded: true } },
      subDrawer: { open: false, subId: null },
      gitEntries: null, writeTick: 0, queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
  // 卡片经 useActiveRun() 取数 → 必需同时设 sessions store 的 activeKey（否则取不到当前 Tab，卡片渲染为 null）
  useSessions.setState({
    tabs: [{
      key: session, sessionId: session, workspace: "/tmp/ws", title: "t",
      projectId: null, createdAt: "2026-09-01T00:00:00Z",
      prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null },
    }],
    activeKey: session,
  });
}

function card(over: Partial<SubView> = {}): HTMLElement {
  seedTab("s1", subView(over));
  render(<SubagentItemCard subId="sub_1" />);
  return document.querySelector(".sub-card") as HTMLElement;
}

describe("SubagentItemCard 收尾原因徽标", () => {
  beforeEach(() => {
    useRun.setState({ tabs: {}, drafts: {} });
  });

  afterEach(() => {
    cleanup();
    useRun.setState({ tabs: {}, drafts: {} });
  });

  it("ended: no_report → 橙色警示图标 + 「提前结束」文案（不再显示绿勾）", () => {
    const c = card({ ended: "no_report" });
    expect(c.textContent).toContain("提前结束");
    expect(c.querySelector(".anticon-check")).toBeNull();
    const warn = c.querySelector(".anticon-exclamation-circle") as HTMLElement;
    expect(warn).not.toBeNull();
    // 警示色走主题桥的 antd colorWarning，不硬编码色值
    expect(warn.style.color).toBe("var(--ws-warn)");
    // 收尾原因文本与警示图标同档强度（此前与普通 meta 同色，夹在步数/token 里读不出来）
    expect(c.querySelector(".sub-card-warn")?.textContent).toContain("提前结束");
    // 步数 / token 展示保持不变（收尾理由追加在 meta 末尾）
    expect(c.textContent).toContain("22/80");
    expect(c.textContent).toContain("1.2k tok");
  });

  it("ended: budget → 橙色警示图标 + 「预算耗尽」文案", () => {
    const c = card({ ended: "budget" });
    expect(c.textContent).toContain("预算耗尽");
    expect(c.textContent).not.toContain("提前结束");
    expect(c.querySelector(".anticon-check")).toBeNull();
    expect(c.querySelector(".anticon-exclamation-circle")).not.toBeNull();
  });

  it("ended: report → 绿勾、无警示文案", () => {
    const c = card({ ended: "report" });
    expect(c.querySelector(".anticon-check")).not.toBeNull();
    expect(c.querySelector(".anticon-exclamation-circle")).toBeNull();
    expect(c.textContent).not.toContain("提前结束");
    expect(c.textContent).not.toContain("预算耗尽");
  });

  it("ended 缺省（旧会话）→ 绿勾，向后兼容", () => {
    const c = card();
    expect(c.querySelector(".anticon-check")).not.toBeNull();
    expect(c.querySelector(".anticon-exclamation-circle")).toBeNull();
    expect(c.textContent).not.toContain("提前结束");
  });
});
