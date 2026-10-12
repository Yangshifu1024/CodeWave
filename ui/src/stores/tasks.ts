// 计划任务 store：任务列表的**单一数据源**。
// 面板与左栏都从这里取数，不再各自 useState + 各自 invoke——否则「立即运行 / 开关 / 编辑」改到的那份
// 与另一处显示的那份会分叉（同一任务两处状态不一致）。列表刷新只有 load 一条路径。
import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ScheduledTask } from "../ipc/types";

/** 状态 → 视觉档位：none = 从未跑过（不显示状态），neutral = 正常收尾，error = 失败 */
export type TaskStatusKind = "none" | "neutral" | "error";

/** 状态 → 视觉档位的**唯一**映射：ok/skipped 中性、其余非空为 error，面板与左栏共用（禁止各自再写一份） */
export function statusKind(status: string | null | undefined): TaskStatusKind {
  if (!status) return "none";
  return status === "ok" || status === "skipped" ? "neutral" : "error";
}

/**
 * 状态 → 用户可读文案的**唯一**映射（任务页标签、历史行、左栏 tooltip 共用）。
 * 已知状态走 i18n；未知状态**原样回显**（不臆造翻译，免得把后端的枚举值说成别的东西）。
 * `t` 由调用方注入：本文件不 import i18n，保持纯数据模块可被任意上下文复用。
 */
export function statusLabel(status: string | null | undefined, t: (key: string) => string): string {
  if (!status) return "";
  if (status === "ok") return t("tasks.statusOk");
  if (status === "error") return t("tasks.statusError");
  if (status === "skipped") return t("tasks.skipped");
  return status;
}

interface TasksState {
  items: ScheduledTask[];
  loading: boolean;
  error: string | null;
  diagnostics: import("../ipc/types").TaskDiagnostics | null;
  runningIds: string[];
  runningRuns: Record<string, string>;
  terminalRuns: string[];
  deletedIds: string[];
  load(): Promise<void>;
  loadDiagnostics(): Promise<void>;
  applyFired(p: { id?: string; name?: string; run_id?: string }): void;
  applyDone(p: { id?: string; name?: string; run_id?: string; status?: string; summary?: string; persistence_error?: string | null }): void;
  captureRevision(): number;
  upsertLocal(task: ScheduledTask, requestRevision?: number): void;
  removeLocal(id: string): void;
}

// Any event/edit/delete invalidates a list request started before it. A newer request wins.
let requestSerial = 0;
let revision = 0;
let diagnosticsSerial = 0;

function runningProjection(items: ScheduledTask[]) {
  const runningRuns: Record<string, string> = {};
  for (const task of items) if (task.active_run) runningRuns[task.id] = task.active_run.run_id;
  return { runningRuns, runningIds: Object.keys(runningRuns) };
}

export const useTasks = create<TasksState>((set, get) => ({
  items: [], loading: false, error: null, diagnostics: null, runningIds: [], runningRuns: {}, terminalRuns: [], deletedIds: [],

  async load() {
    const serial = ++requestSerial;
    const atRevision = revision;
    set({ loading: true });
    try {
      const list = await ipc.listScheduledTasks("");
      if (!Array.isArray(list)) throw new Error("Invalid scheduled task list response");
      if (serial !== requestSerial) return;
      if (atRevision !== revision) { set({ loading: false }); return; }
      revision++;
      set((state) => ({ items: list, ...runningProjection(list), deletedIds: state.deletedIds.filter((id) => !list.some((task) => task.id === id)), error: null, loading: false }));
    } catch (error) {
      if (serial === requestSerial) set({ ...(atRevision === revision ? { error: String(error) } : {}), loading: false });
    }
  },

  async loadDiagnostics() {
    const serial = ++diagnosticsSerial;
    try {
      const diagnostics = await ipc.listScheduledTaskDiagnostics();
      if (!diagnostics || !Array.isArray(diagnostics.issues) || !["loading", "ready", "degraded"].includes(diagnostics.state)) {
        throw new Error("Invalid task diagnostics response");
      }
      if (serial === diagnosticsSerial) set({ diagnostics });
    } catch (error) {
      if (serial === diagnosticsSerial) set({ error: String(error) });
    }
  },

  applyFired(p) {
    const id = p?.id;
    if (!id) return;
    const state = get();
    if (state.deletedIds.includes(id) || (p.run_id && state.terminalRuns.includes(p.run_id))) return;
    // A delayed event for a completed instance must not resurrect it.
    if (p.run_id && state.items.find((t) => t.id === id)?.runs.some((r) => r.run_id === p.run_id)) return;
    const currentRun = state.runningIds.includes(id) ? state.runningRuns[id] : undefined;
    if (currentRun && p.run_id && currentRun !== p.run_id) {
      // Different identities have no ordering information. Read the current backend instance
      // instead of allowing an unknown delayed fired event to replace an established run.
      revision++;
      void get().load();
      return;
    }
    revision++;
    set((s) => ({ runningIds: s.runningIds.includes(id) ? s.runningIds : [...s.runningIds, id],
      runningRuns: { ...s.runningRuns, [id]: p.run_id ?? "" } }));
  },

  applyDone(p) {
    const state = get();
    const hit = p?.id ? state.items.find((t) => t.id === p.id) : state.items.find((t) => !!p?.name && t.name === p.name);
    const id = p?.id ?? hit?.id;
    // Record terminal identity immediately, before requesting the full history snapshot.
    // Otherwise a delayed fired can resurrect the run and invalidate that pending snapshot.
    if (p.run_id && !state.terminalRuns.includes(p.run_id)) {
      set({ terminalRuns: [...state.terminalRuns, p.run_id].slice(-1024) });
    }
    if (id && state.deletedIds.includes(id)) return;
    const activeId = id && state.runningIds.includes(id) ? state.runningRuns[id] || hit?.active_run?.run_id : hit?.active_run?.run_id;
    // Legacy identity-less events cannot settle an identity-aware active run.
    if (activeId && p.run_id !== activeId) return;
    revision++;
    set((s) => {
      const runningRuns = { ...s.runningRuns };
      if (id) delete runningRuns[id];
      return {
        items: s.items.map((task) => task.id !== id ? task : { ...task,
          last_status: typeof p.status === "string" ? p.status : task.last_status,
          last_summary: typeof p.summary === "string" ? p.summary : task.last_summary,
          persistence_error: p.persistence_error ?? task.persistence_error,
          active_run: null,
        }),
        runningIds: s.runningIds.filter((value) => value !== id), runningRuns,
      };
    });
    void get().load();
  },

  captureRevision() { return revision; },

  upsertLocal(task, requestRevision = revision) {
    const staleLifecycle = requestRevision !== revision;
    revision++;
    set((s) => {
      // A deletion completed after the command began; a delayed response cannot resurrect it.
      if (staleLifecycle && s.deletedIds.includes(task.id)) return {};
      const current = s.items.find((item) => item.id === task.id);
      const merged = staleLifecycle && current ? { ...task,
        next_run: current.next_run, active_run: current.active_run, runs: current.runs,
        last_status: current.last_status, last_summary: current.last_summary,
        persistence_error: current.persistence_error,
      } : task;
      const items = current ? s.items.map((item) => item.id === task.id ? merged : item) : [...s.items, merged];
      if (staleLifecycle) return { items };
      // A mutation response is authoritative only for its own task. Other task items may
      // predate fired events whose active identity currently exists only in the projection.
      const runningRuns = { ...s.runningRuns };
      const runningIds = s.runningIds.filter((id) => id !== task.id);
      delete runningRuns[task.id];
      if (task.active_run) {
        runningRuns[task.id] = task.active_run.run_id;
        runningIds.push(task.id);
      }
      return { items, runningRuns, runningIds, deletedIds: s.deletedIds.filter((id) => id !== task.id) };
    });
    // Editable fields are retained, but a lifecycle observed after request start must win.
    // Refresh next_run/history from a new authoritative read rather than guessing from events.
    if (staleLifecycle) void get().load();
  },

  removeLocal(id) {
    revision++;
    set((s) => {
      const runningRuns = { ...s.runningRuns }; delete runningRuns[id];
      return { items: s.items.filter((task) => task.id !== id), runningIds: s.runningIds.filter((value) => value !== id), runningRuns, deletedIds: [...s.deletedIds.filter((value) => value !== id), id].slice(-1024) };
    });
  },
}));
