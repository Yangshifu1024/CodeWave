// run:error errorKind passthrough ([docs/auth-error-guidance](../../../docs/auth-error-guidance.md)): the backend ProviderError category lands on the error item
// and drives the auth/billing "open model settings" guidance; old payloads without kind stay undefined.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
  Channel: class {
    onmessage: ((f: any) => void) | null = null;
  },
}));

function seed() {
  useSessions.setState({
    tabs: [{ key: "s1", sessionId: "s1", workspace: "/tmp/ws", title: "s1", projectId: null, createdAt: "2026-09-01T00:00:00Z", prefs: { approval_mode: "auto_edit", model_id: null, reasoning_effort: null } }],
    activeKey: "s1",
    projects: [],
  });
  useRun.setState((s) => {
    s.tabs["s1"] = {
      items: [], running: true, streamGen: 0, ask: null, breakdown: null,
      todos: [], suggestions: [], subs: [], subStreams: {}, subDrawer: { open: false, subId: null }, gitEntries: null, writeTick: 0,
      queue: [], pendingItemId: null, draftFromQueue: null, lastDoneRunId: null, compacting: false,
    };
  });
}

function handlers() {
  return useRun.getState().bindGlobalHandlers();
}

describe("run:error errorKind (docs/auth-error-guidance)", () => {
  beforeEach(seed);

  it("carries the payload kind onto the error item", () => {
    handlers()["run:error"]({ session: "s1", error: "认证失败：Header中未收到Authorization参数 (HTTP 401)", kind: "auth" });
    const items = useRun.getState().tabs["s1"]!.items;
    expect(items[items.length - 1]).toMatchObject({ kind: "error", errorKind: "auth" });
    expect(useRun.getState().tabs["s1"]!.running).toBe(false);
  });

  it("keeps errorKind undefined when the payload has no kind (old backend shape)", () => {
    handlers()["run:error"]({ session: "s1", error: "boom" });
    const items = useRun.getState().tabs["s1"]!.items;
    expect(items[items.length - 1]).toMatchObject({ kind: "error", errorKind: undefined });
  });
});
