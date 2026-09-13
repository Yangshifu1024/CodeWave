// Session auto-naming ([docs/session-auto-title](../../../docs/session-auto-title.md)): session:title event → applyTitle syncs the session list meta and open Tab titles
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import type { SessionMeta } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

const prefs = { approval_mode: "auto_edit" as const, model_id: null, reasoning_effort: null };

function meta(id: string, title: string): SessionMeta {
  return {
    id,
    title,
    workspace: "/tmp/ws",
    model_id: null,
    created_at: "2026-09-01T00:00:00Z",
    updated_at: "2026-09-01T00:00:00Z",
    message_count: 0,
    project_id: null,
    roots: ["/tmp/ws"],
  };
}

function tab(id: string, title: string) {
  return {
    key: id,
    sessionId: id,
    workspace: "/tmp/ws",
    title,
    projectId: null,
    createdAt: "2026-09-01T00:00:00Z",
    prefs,
  };
}

beforeEach(() => {
  useSessions.setState({ tabs: [], activeKey: null, sessions: [], projects: [] });
  useRun.setState((s) => {
    s.tabs = {}; s.drafts = {};
  });
});

describe("session:title 自动命名落位", () => {
  it("事件同步更新会话列表 meta 与已打开 Tab 的标题", () => {
    useSessions.setState({
      sessions: [meta("s1", "帮我修一下登…")],
      tabs: [tab("s1", "帮我修一下登…")],
    });
    useRun.getState().bindGlobalHandlers()["session:title"]({ session: "s1", title: "修复登录超时" });
    const s = useSessions.getState();
    expect(s.sessions.find((m) => m.id === "s1")?.title).toBe("修复登录超时");
    expect(s.tabs.find((t) => t.sessionId === "s1")?.title).toBe("修复登录超时");
  });

  it("空标题被忽略，不改写现有标题", () => {
    useSessions.setState({
      sessions: [meta("s1", "原标题")],
      tabs: [tab("s1", "原标题")],
    });
    useRun.getState().bindGlobalHandlers()["session:title"]({ session: "s1", title: "" });
    const s = useSessions.getState();
    expect(s.sessions[0].title).toBe("原标题");
    expect(s.tabs[0].title).toBe("原标题");
  });

  it("applyTitle 直接调用：会话未开 Tab 时只更新列表，不误建 Tab", () => {
    useSessions.setState({ sessions: [meta("s9", "旧标题")] });
    useSessions.getState().applyTitle("s9", "新标题");
    const s = useSessions.getState();
    expect(s.sessions[0].title).toBe("新标题");
    expect(s.tabs).toHaveLength(0);
  });
});
