// Left nav session row states batch ([docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)):
// 1) While awaiting confirmation (askPending), inline rename/delete buttons do not render (same slot as the "awaiting confirmation" badge, they would overlap)
// 2) Unread dot on session end — run done/error marks the session when the user is not viewing it (incl. late events after the tab closed); entering the session clears it
// 3) ask:opened sends a system notification on window blur (notify_system, click-to-reveal via [docs/notification-click-reveal](../../../docs/notification-click-reveal.md))
// vitest loads no CSS: render the component directly + seed data via useSessions/useRun setState; events injected by calling bindGlobalHandlers() handlers directly
import { describe, it, expect, vi, afterEach } from "vitest";
import { render, cleanup, act, waitFor } from "@testing-library/react";
import { App } from "antd";
import ProjectNav from "../features/shell/ProjectNav";
import { useSessions, type Tab } from "../stores/sessions";
import { useRun } from "../stores/run";
import { invoke } from "@tauri-apps/api/core";
import { DEFAULT_PREFS, type SessionMeta } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

function meta(partial: Partial<SessionMeta> & { id: string; title: string }): SessionMeta {
  return {
    workspace: "/tmp/ws",
    model_id: null,
    created_at: "",
    updated_at: "",
    message_count: 0,
    project_id: null,
    roots: ["/tmp/ws"],
    ...partial,
  };
}

function tab(partial: Partial<Tab> & { sessionId: string; title: string }): Tab {
  return {
    key: partial.sessionId,
    workspace: "/tmp/ws",
    projectId: null,
    createdAt: "2026-09-03T09:00:00Z",
    prefs: { ...DEFAULT_PREFS },
    ...partial,
  };
}

function renderNav(state: {
  tabs?: Tab[];
  sessions?: SessionMeta[];
  activeKey?: string | null;
  unread?: Record<string, boolean>;
}) {
  useSessions.setState({
    tabs: state.tabs ?? [],
    sessions: state.sessions ?? [],
    activeKey: state.activeKey ?? null,
    unread: state.unread ?? {},
  });
  return render(
    <App>
      <ProjectNav />
    </App>,
  );
}

function firstRow(): HTMLElement {
  const row = document.querySelector(".session-nav-row");
  expect(row).toBeTruthy();
  return row as HTMLElement;
}

afterEach(() => {
  cleanup();
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [], unread: {} });
  useRun.setState((s) => {
    s.tabs = {};
  });
  vi.mocked(invoke).mockClear();
});

describe("等待确认行：重命名/删除按钮与「等待确认」徽标互斥（docs/ask-ink-accent-and-composer-cover）", () => {
  it("ask 存在 → 无 .row-actions、有「等待确认」徽标；ask 关闭 → 按钮恢复", () => {
    renderNav({ tabs: [tab({ sessionId: "s1", title: "提问中会话" })], activeKey: "s1" });
    useRun.getState().initTab("s1");

    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"].ask = { askId: "a1", kind: "ask" } as any;
      });
    });
    let row = firstRow();
    expect(row.querySelector(".row-actions")).toBeNull();
    expect(row.textContent).toContain("等待确认");

    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"].ask = null;
      });
    });
    row = firstRow();
    expect(row.querySelectorAll(".row-action")).toHaveLength(2);
  });
});

describe("会话结束未读点（docs/ask-ink-accent-and-composer-cover）", () => {
  it("run:done 时用户不在该会话 → 标记未读；活跃会话 → 不标", () => {
    renderNav({
      tabs: [tab({ sessionId: "s1", title: "甲" }), tab({ sessionId: "s2", title: "乙" })],
      activeKey: "s2",
    });
    useRun.getState().initTab("s1");
    useRun.setState((s) => {
      s.tabs["s1"].running = true;
    });
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => {
      handlers["run:done"]({ session: "s1", run_id: "r1" });
    });
    expect(useSessions.getState().unread["s1"]).toBe(true);

    act(() => {
      handlers["run:done"]({ session: "s2", run_id: "r2" });
    });
    expect(useSessions.getState().unread["s2"]).toBeFalsy();
  });

  it("迟到 done（Tab 已关、状态桶已删）仍标记；run:error 同样标记", () => {
    renderNav({ tabs: [tab({ sessionId: "s1", title: "甲" })], activeKey: "s1" });
    const handlers = useRun.getState().bindGlobalHandlers();
    // s2 has no state bucket (tab closed): marked before the guard, so the row still gets the "new results" signal
    act(() => {
      handlers["run:done"]({ session: "s2", run_id: "r1" });
    });
    expect(useSessions.getState().unread["s2"]).toBe(true);

    act(() => {
      handlers["run:error"]({ session: "s3", error: "boom" });
    });
    expect(useSessions.getState().unread["s3"]).toBe(true);
  });

  it("组件：seed 未读 → 运行槽位出现 .session-unread-dot；进入会话（revealSession）→ 清除", () => {
    renderNav({
      tabs: [tab({ sessionId: "s1", title: "甲" })],
      sessions: [meta({ id: "s1", title: "甲" })],
      unread: { s1: true },
      activeKey: null,
    });
    expect(document.querySelector(".session-run-slot .session-unread-dot")).toBeTruthy();

    act(() => {
      useSessions.getState().revealSession("s1");
    });
    expect(useSessions.getState().activeKey).toBe("s1");
    expect(useSessions.getState().unread["s1"]).toBeFalsy();
    expect(document.querySelector(".session-unread-dot")).toBeNull();
  });
});

describe("ask:opened 失焦系统通知（docs/ask-ink-accent-and-composer-cover）", () => {
  it("窗口失焦时发出 notify_system（含 sessionId），聚焦时不发", async () => {
    const hasFocusSpy = vi.spyOn(document, "hasFocus").mockReturnValue(false);
    renderNav({ tabs: [tab({ sessionId: "s1", title: "确认会话" })], activeKey: "s2" });
    useRun.getState().initTab("s1");
    const handlers = useRun.getState().bindGlobalHandlers();
    act(() => {
      handlers["ask:opened"]({
        session: "s1", ask_id: "a1", kind: "approval", title: "高危命令确认", detail: "x", allow_always: true,
      });
    });
    await waitFor(() => {
      const calls = vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === "notify_system");
      expect(calls).toHaveLength(1);
      expect((calls[0][1] as any).sessionId).toBe("s1");
    });

    // Focused: the in-app ask window already exists, so no system notification is sent
    vi.mocked(invoke).mockClear();
    hasFocusSpy.mockReturnValue(true);
    act(() => {
      useRun.setState((s) => {
        s.tabs["s1"].ask = null;
      });
      handlers["ask:opened"]({
        session: "s1", ask_id: "a2", kind: "approval", title: "再次确认", detail: "x", allow_always: false,
      });
    });
    expect(vi.mocked(invoke).mock.calls.some(([cmd]) => cmd === "notify_system")).toBe(false);
    hasFocusSpy.mockRestore();
  });
});
