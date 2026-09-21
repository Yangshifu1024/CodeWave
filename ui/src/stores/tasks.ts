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
  /** 加载/操作失败的原因（null = 无错误）；界面据此显示错误行 */
  error: string | null;
  /** 正在运行的任务 id（由 scheduled:fired / scheduled:done 驱动；不持久化） */
  runningIds: string[];
  load(): Promise<void>;
  applyFired(p: { id?: string; name?: string }): void;
  applyDone(p: { id?: string; name?: string; status?: string; summary?: string }): void;
  upsertLocal(task: ScheduledTask): void;
  removeLocal(id: string): void;
}

export const useTasks = create<TasksState>((set, get) => ({
  items: [],
  loading: false,
  error: null,
  runningIds: [],

  async load() {
    set({ loading: true });
    try {
      // 该命令是全局列表（后端忽略 sessionId；既有调用方都传当前会话 id）——这里无会话上下文，传空串
      const list = await ipc.listScheduledTasks("");
      // 防御非数组载荷（未 mock 的命令会返回 null）：坏数据不该把左栏与任务页一起炸掉，也不该抹掉旧列表
      set({ items: Array.isArray(list) ? list : [], error: null, loading: false });
    } catch (e) {
      // 只写 error，**不动 items**：读盘失败必须与「确实没有任务」区分开，
      // 否则一次瞬时失败会把列表擦成空态，用户会以为任务被删了
      set({ error: String(e), loading: false });
    }
  },

  applyFired(p) {
    const id = p?.id;
    if (!id || get().runningIds.includes(id)) return;
    // 只置运行标记：提示已由用户拍板静默（[docs/tasks-module-polish]）
    set((s) => ({ runningIds: [...s.runningIds, id] }));
  },

  applyDone(p) {
    const id = p?.id;
    const name = p?.name;
    // 载荷缺字段时保留旧值，避免把已有状态抹成 null
    const status = typeof p?.status === "string" ? p.status : null;
    const summary = typeof p?.summary === "string" ? p.summary : null;
    set((s) => {
      // 先认 id；旧/异常载荷没带 id 时退回按 name 匹配（名字非空才有意义）
      let hitId = id ?? "";
      const items = s.items.map((t) => {
        const hit = id ? t.id === id : !!name && t.name === name;
        if (!hit) return t;
        hitId = t.id;
        return { ...t, last_status: status ?? t.last_status, last_summary: summary ?? t.last_summary };
      });
      // 命中项可能不在列表里（还没加载完 / 已被删）：仍按 id 清运行标记
      return { items, runningIds: hitId ? s.runningIds.filter((x) => x !== hitId) : s.runningIds };
    });
    // 事件只带收尾摘要，runs/next_run 是后端落盘后才有的：重新拉一次列表补齐。
    // fire-and-forget——失败由 load 自己兜进 error，这里不再重复处理
    void get().load();
  },

  upsertLocal(task) {
    // 本地就地合并（创建/编辑成功后的乐观落地）：免一次整表刷新带来的闪烁与滚动跳位
    set((s) => {
      const i = s.items.findIndex((t) => t.id === task.id);
      if (i < 0) return { items: [...s.items, task] };
      const items = s.items.slice();
      items[i] = task;
      return { items };
    });
  },

  removeLocal(id) {
    // 删除成功后本地摘除（同上：不整表刷新）
    set((s) => ({ items: s.items.filter((t) => t.id !== id) }));
  },
}));
