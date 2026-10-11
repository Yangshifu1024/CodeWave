import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ActivitySnapshot } from "../ipc/types";

interface ActivityState {
  snapshot: ActivitySnapshot | null;
  refreshing: boolean;
  error: string | null;
  lastSyncedAt: number | null;
  active: boolean;
  generation: number;
  expanded: string[];
  recentOpen: boolean;
  setActive(active: boolean): void;
  refresh(): Promise<void>;
  setExpanded(keys: string[]): void;
  setRecentOpen(open: boolean): void;
}

// 单飞覆盖隐藏/重开边界；旧请求完成后才发新代际请求。
let inFlight: Promise<void> | null = null;
let queued = false;

export const useActivity = create<ActivityState>((set, get) => ({
  snapshot: null, refreshing: false, error: null, lastSyncedAt: null,
  active: false, generation: 0, expanded: [], recentOpen: false,
  setExpanded: (expanded) => set({ expanded }),
  setRecentOpen: (recentOpen) => set({ recentOpen }),
  setActive(active) {
    if (active === get().active) return;
    set({ active, generation: get().generation + 1, refreshing: false });
    if (active) void get().refresh();
  },
  refresh() {
    if (!get().active) return Promise.resolve();
    if (inFlight) {
      queued = true;
      return inFlight;
    }
    const generation = get().generation;
    set({ refreshing: true });
    const request = (async () => {
      try {
        const snapshot = await ipc.getActivitySnapshot();
        if (!get().active || get().generation !== generation) return;
        if (get().snapshot && snapshot.revision < get().snapshot!.revision) return;
        set({ snapshot, error: null, lastSyncedAt: snapshot.sampled_at_ms });
      } catch (e) {
        if (get().active && get().generation === generation) set({ error: String(e) });
      } finally {
        if (get().generation === generation) set({ refreshing: false });
      }
    })();
    inFlight = request;
    void request.finally(() => {
      if (inFlight === request) inFlight = null;
      if (queued) {
        queued = false;
        if (get().active) void get().refresh();
      }
    });
    return request;
  },
}));

/** 页面可见时刷新；卸载/隐藏不轮询，恢复焦点立即对账。 */
export function watchActivity(): () => void {
  let timer: ReturnType<typeof setInterval> | null = null;
  const sync = () => {
    const visible = document.visibilityState !== "hidden";
    useActivity.getState().setActive(visible);
    if (visible && timer === null) timer = setInterval(() => void useActivity.getState().refresh(), 2000);
    if (!visible && timer !== null) { clearInterval(timer); timer = null; }
  };
  const focus = () => { sync(); if (document.visibilityState !== "hidden") void useActivity.getState().refresh(); };
  document.addEventListener("visibilitychange", sync);
  window.addEventListener("focus", focus);
  sync();
  return () => {
    if (timer !== null) clearInterval(timer);
    document.removeEventListener("visibilitychange", sync);
    window.removeEventListener("focus", focus);
    useActivity.getState().setActive(false);
  };
}
