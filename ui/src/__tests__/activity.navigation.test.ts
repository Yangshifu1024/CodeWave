import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import "../i18n";
import { ipc } from "../ipc/client";
import { useRun } from "../stores/run";
import { useSessions } from "../stores/sessions";
import { useUi } from "../stores/ui";
import { openActivityWaiting } from "../features/dashboard/navigation";
import { DEFAULT_PREFS, type ActivitySession, type AskOpenedEvent, type SessionMeta } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => null), Channel: class {} }));
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
const session: ActivitySession = { session_id: "s1", run_id: "r1", title: "one", project_id: null, started_at_ms: 1, activity: "waiting", waiting: [{ ask_id: "a1", kind: "approval", title: "approve", detail: "detail" }], subs: [] };
const payload: AskOpenedEvent = { session: "s1", ask_id: "a1", kind: "approval", title: "Original", plan_file: "/plan.md", plan_body: "full body", allow_always: true, approval_kind: "scope", approval: true, questions: [{ id: "q", question: "Continue?", options: [{ id: "yes", label: "Yes", mode: "full_access" }] }] };
const meta: SessionMeta = { id: "s1", title: "one", workspace: "/w", project_id: null, created_at: "now", updated_at: "now", model_id: null, message_count: 0, roots: ["/w"], running: true, interrupted: null };

beforeEach(() => {
  useSessions.setState({ tabs: [], activeKey: null, sessions: [meta], loading: false });
  useRun.setState({ tabs: {}, drafts: {} });
  useUi.setState({ dashboardOpen: true, notifications: [] });
  vi.spyOn(ipc, "loadSession").mockResolvedValue({ messages: [], paging: { format: "legacy", loaded_from_seq: 0, segment_count: 1, total_messages: 0, bytes: 0, has_more: false } });
  vi.spyOn(useRun.getState(), "refreshGit").mockResolvedValue();
  vi.spyOn(useRun.getState(), "reconcileRun").mockImplementation(async (id) => {
    useRun.setState((s) => { s.tabs[id].running = true; s.tabs[id].runId = "r1"; s.tabs[id].runEpoch = 10; });
  });
});
afterEach(() => { vi.restoreAllMocks(); useUi.setState({ dashboardOpen: false }); });

describe("dashboard request navigation", () => {
  it("waits for history and runtime reconciliation before fetching and hydrating the original ask", async () => {
    const history = deferred<Awaited<ReturnType<typeof ipc.loadSession>>>();
    const reconcile = deferred<void>();
    vi.mocked(ipc.loadSession).mockReturnValue(history.promise);
    vi.mocked(useRun.getState().reconcileRun).mockImplementation(async (id) => {
      await reconcile.promise;
      useRun.setState((s) => { s.tabs[id].running = true; s.tabs[id].runId = "r1"; s.tabs[id].runEpoch = 10; });
    });
    const getAsk = vi.spyOn(ipc, "getActivityAsk").mockResolvedValue(payload);
    const answer = vi.spyOn(ipc, "resolveAsk");
    const open = openActivityWaiting(session, session.waiting[0]);
    expect(getAsk).not.toHaveBeenCalled();
    history.resolve({ messages: [], paging: { format: "legacy", loaded_from_seq: 0, segment_count: 1, total_messages: 0, bytes: 0, has_more: false } });
    await vi.waitFor(() => expect(useRun.getState().reconcileRun).toHaveBeenCalled());
    expect(getAsk).not.toHaveBeenCalled();
    reconcile.resolve();
    expect(await open).toBe(true);
    expect(getAsk).toHaveBeenCalledWith("s1", "r1", "a1");
    expect(useSessions.getState().activeKey).toBe("s1");
    expect(useRun.getState().tabs.s1.ask).toMatchObject({ askId: "a1", planBody: "full body", planFile: "/plan.md", allowAlways: true, approvalKind: "scope", questions: payload.questions });
    expect(useSessions.getState().tabs[0].prefs.approval_mode).toBe(DEFAULT_PREFS.approval_mode);
    expect(answer).not.toHaveBeenCalled();
  });

  it("rejects expired requests without answering or clearing another current ask", async () => {
    await useSessions.getState().openSessionReady("s1");
    useRun.getState().restoreAsk({ ...payload, ask_id: "a2" });
    vi.spyOn(ipc, "getActivityAsk").mockResolvedValue(null);
    const answer = vi.spyOn(ipc, "resolveAsk");
    expect(await openActivityWaiting(session, session.waiting[0])).toBe(false);
    expect(useRun.getState().tabs.s1.ask?.askId).toBe("a2");
    expect(answer).not.toHaveBeenCalled();
  });

  it.each(["closed", "new-run"])("rejects in-flight ask after %s", async (cause) => {
    await useSessions.getState().openSessionReady("s1");
    const result = deferred<AskOpenedEvent | null>();
    vi.spyOn(ipc, "getActivityAsk").mockReturnValue(result.promise);
    const open = openActivityWaiting(session, session.waiting[0]);
    await vi.waitFor(() => expect(ipc.getActivityAsk).toHaveBeenCalled());
    if (cause === "closed") useRun.getState().bindGlobalHandlers()["ask:closed"]({ session: "s1", ask_id: "a1" });
    else useRun.setState((s) => { s.tabs.s1.runId = "r2"; s.tabs.s1.runEpoch = 11; });
    result.resolve(payload);
    expect(await open).toBe(false);
    expect(useRun.getState().tabs.s1.ask).toBeNull();
  });

  it("closing a different concurrent ask preserves the displayed ask", () => {
    useRun.getState().initTab("s1");
    useRun.getState().restoreAsk({ ...payload, ask_id: "a2" });
    useRun.getState().bindGlobalHandlers()["ask:closed"]({ session: "s1", ask_id: "a1" });
    expect(useRun.getState().tabs.s1.ask?.askId).toBe("a2");
  });

  it("closing another ask while restoring does not invalidate the target", async () => {
    await useSessions.getState().openSessionReady("s1");
    const result = deferred<AskOpenedEvent | null>();
    vi.spyOn(ipc, "getActivityAsk").mockReturnValue(result.promise);
    const open = openActivityWaiting(session, session.waiting[0]);
    await vi.waitFor(() => expect(ipc.getActivityAsk).toHaveBeenCalled());
    useRun.getState().bindGlobalHandlers()["ask:closed"]({ session: "s1", ask_id: "another" });
    result.resolve(payload);
    expect(await open).toBe(true);
    expect(useRun.getState().tabs.s1.ask?.askId).toBe("a1");
  });

  it("old load cannot restore into a closed and reopened Tab with the same id", async () => {
    const old = deferred<Awaited<ReturnType<typeof ipc.loadSession>>>();
    vi.mocked(ipc.loadSession).mockReturnValueOnce(old.promise);
    const initial = useSessions.getState().openSessionReady("s1");
    useSessions.getState().closeTab("s1", { force: true });
    await useSessions.getState().openSessionReady("s1");
    old.resolve({ messages: [{ role: "user", content: [{ type: "text", text: "OLD HISTORY" }] }], paging: { format: "legacy", loaded_from_seq: 0, segment_count: 1, total_messages: 1, bytes: 0, has_more: false } });
    await expect(initial).rejects.toThrow();
    expect(useRun.getState().tabs.s1.items).toEqual([]);
    expect(ipc.loadSession).toHaveBeenCalledTimes(2);
  });

  it("Git refresh finishing after close cannot recreate the disposed bucket", async () => {
    vi.mocked(useRun.getState().refreshGit).mockRestore();
    const git = deferred<Awaited<ReturnType<typeof ipc.gitStatus>>>();
    vi.spyOn(ipc, "gitStatus").mockReturnValue(git.promise);
    const open = useSessions.getState().openSessionReady("s1");
    await vi.waitFor(() => expect(ipc.gitStatus).toHaveBeenCalled());
    useSessions.getState().closeTab("s1", { force: true });
    git.resolve({ repo: false, entries: [] });
    await expect(open).rejects.toThrow();
    expect(useRun.getState().tabs.s1).toBeUndefined();
  });

  it.each(["project", "temporary", "history"])("%s navigation leaves overview", async (kind) => {
    vi.spyOn(ipc, "listSessions").mockResolvedValue([meta]);
    vi.spyOn(ipc, "createSession").mockResolvedValue({ session_id: "s1", workspace: "/w", project_id: null, roots: ["/w"] });
    if (kind === "project") await useSessions.getState().openProjectSession("p1");
    else if (kind === "temporary") await useSessions.getState().openFreeSession();
    else await useSessions.getState().openSession(meta);
    expect(useUi.getState().dashboardOpen).toBe(false);
    expect(useSessions.getState().activeKey).toBe("s1");
  });

  it("reports a busy concurrent restore failure and allows retry", async () => {
    useSessions.setState({ loading: true });
    vi.mocked(ipc.loadSession).mockRejectedValueOnce(new Error("load failed"));
    await expect(useSessions.getState().openSessionReady("s1")).rejects.toThrow("load failed");
    expect(useSessions.getState().tabs[0].loaded).toBe(false);
    await useSessions.getState().openSessionReady("s1");
    expect(useSessions.getState().tabs[0].loaded).toBe(true);
  });

  it("does not recreate a closed Tab after legacy preferences resolve", async () => {
    const legacy = deferred<void>();
    vi.spyOn(ipc, "restoreLegacyModelPrefs").mockReturnValue(legacy.promise);
    const open = useSessions.getState().openSessionReady("s1");
    await vi.waitFor(() => expect(ipc.restoreLegacyModelPrefs).toHaveBeenCalled());
    useSessions.getState().closeTab("s1", { force: true });
    legacy.resolve();
    await expect(open).rejects.toThrow();
    expect(useRun.getState().tabs.s1).toBeUndefined();
  });
});
