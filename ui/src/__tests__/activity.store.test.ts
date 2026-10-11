import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ipc } from "../ipc/client";
import { useActivity, watchActivity } from "../stores/activity";
import type { ActivitySnapshot } from "../ipc/types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const snapshot = (revision: number): ActivitySnapshot => ({ revision, sampled_at_ms: revision * 1000, sessions: [], recent: [] });
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>((r) => { resolve = r; }); return { promise, resolve }; }
const flush = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };

beforeEach(() => {
  useActivity.getState().setActive(false);
  useActivity.setState({ snapshot: null, error: null, lastSyncedAt: null, expanded: [], recentOpen: false });
});
afterEach(() => { useActivity.getState().setActive(false); vi.restoreAllMocks(); vi.useRealTimers(); });

describe("activity snapshot lifecycle", () => {
  it("retains previous snapshot and reading state on failure; ignores older revisions", async () => {
    const get = vi.spyOn(ipc, "getActivitySnapshot").mockResolvedValue(snapshot(4));
    useActivity.getState().setActive(true);
    await flush();
    useActivity.getState().setExpanded(["session:run"]);
    useActivity.getState().setRecentOpen(true);
    get.mockRejectedValueOnce(new Error("offline"));
    await useActivity.getState().refresh();
    expect(useActivity.getState().snapshot?.revision).toBe(4);
    expect(useActivity.getState().error).toContain("offline");
    expect(useActivity.getState().lastSyncedAt).toBe(4000);
    get.mockResolvedValueOnce(snapshot(3));
    await useActivity.getState().refresh();
    expect(useActivity.getState().snapshot?.revision).toBe(4);
    expect(useActivity.getState().expanded).toEqual(["session:run"]);
    expect(useActivity.getState().recentOpen).toBe(true);
  });

  it("uses a single request and rejects responses from a hidden generation", async () => {
    const first = deferred<ActivitySnapshot>();
    const second = deferred<ActivitySnapshot>();
    const get = vi.spyOn(ipc, "getActivitySnapshot").mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    useActivity.getState().setActive(true);
    void useActivity.getState().refresh();
    expect(get).toHaveBeenCalledTimes(1);
    useActivity.getState().setActive(false);
    useActivity.getState().setActive(true);
    expect(get).toHaveBeenCalledTimes(1);
    first.resolve(snapshot(10));
    await flush();
    expect(useActivity.getState().snapshot).toBeNull();
    expect(get).toHaveBeenCalledTimes(2);
    second.resolve(snapshot(11));
    await flush();
    expect(useActivity.getState().snapshot?.revision).toBe(11);
  });

  it("polls while visible, pauses on hide/unmount, and refreshes on focus", async () => {
    vi.useFakeTimers();
    const get = vi.spyOn(ipc, "getActivitySnapshot").mockResolvedValue(snapshot(1));
    const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    const stop = watchActivity();
    await flush();
    expect(get).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(2000);
    expect(get).toHaveBeenCalledTimes(2);
    visibility.mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    await vi.advanceTimersByTimeAsync(4000);
    expect(get).toHaveBeenCalledTimes(2);
    visibility.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    await flush();
    window.dispatchEvent(new Event("focus"));
    await flush();
    expect(get).toHaveBeenCalledTimes(4);
    stop();
    await vi.advanceTimersByTimeAsync(4000);
    expect(get).toHaveBeenCalledTimes(4);
    expect(useActivity.getState().active).toBe(false);
  });
});
