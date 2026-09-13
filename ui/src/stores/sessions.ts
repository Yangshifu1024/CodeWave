// 会话与多 Tab 状态：会话依附于「项目」（单主目录，docs/12 语义）或作为临时会话免目录存在
import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ProjectEntry, SessionMeta, SessionPrefs } from "../ipc/types";
import { DEFAULT_PREFS } from "../ipc/types";
import { useRun } from "./run";
import { useUi } from "./ui";
import { baseName } from "../utils/path";
import { i18n } from "../i18n";

/** 前端 Tab：一个打开的会话窗口实例 */
export interface Tab {
  key: string; // = sessionId
  sessionId: string;
  workspace: string; // 主目录
  title: string;
  projectId: string | null;
  createdAt: string; // 会话创建时间
  /** 会话级运行参数（审批档/模型/力度，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）；前端为事实源，后端镜像 */
  prefs: SessionPrefs;
}

/** 会话域 store：Tab 列表 + 项目注册表 + 会话索引 + 未读点等；左栏导航与顶栏 Tab 条的数据源。 */
interface SessionsState {
  tabs: Tab[];
  activeKey: string | null;
  sessions: SessionMeta[];
  projects: ProjectEntry[];
  loading: boolean;
  /** [docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)：会话结束未读点（用户离开时 run done/error 置位）。仅内存态，不持久化 */
  unread: Record<string, boolean>;
  /** run.ts 的 handler 在运行结束时调用；幂等；活跃会话由 subscribe 层自动清除 */
  markUnread(sessionId: string): void;
  clearUnread(sessionId: string): void;
  /** 左栏开合状态（[docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md)）：localStorage 记忆，重启保留 */
  explorerOpen: boolean;
  setExplorerOpen(open: boolean): void;
  refresh(): Promise<void>;
  loadProjects(): Promise<void>;
  addProject(entry: ProjectEntry): Promise<void>;
  updateProject(entry: ProjectEntry): Promise<void>;
  deleteProject(id: string): Promise<number>;
  openProjectSession(projectId: string): Promise<void>;
  openFreeSession(workspace?: string): Promise<void>;
  openSession(meta: SessionMeta): Promise<void>;
  closeTab(key: string): void;
  removeSession(meta: SessionMeta): Promise<void>;
  rename(meta: SessionMeta, title: string): Promise<void>;
  /** 后端自动命名落地（session:title 事件）：同步会话列表 meta 与已开 Tab 的标题 */
  applyTitle(sessionId: string, title: string): void;
  /** 通知点击回跳：Tab 已开则直接切 activeKey，否则从会话列表重开；找不到（已删除）返回 false 交调用方处理 */
  revealSession(sessionId: string): boolean;
  cycleTab(dir: 1 | -1): void;
  /** 更新会话运行参数（本地乐观更新 + 后端镜像；失败回滚并 toast） */
  updatePrefs(key: string, patch: Partial<SessionPrefs>): Promise<void>;
}

function dirName(p: string): string {
  return baseName(p);
}

export const useSessions = create<SessionsState>((set, get) => ({
  tabs: [],
  activeKey: null,
  sessions: [],
  projects: [],
  loading: false,
  explorerOpen: localStorage.getItem("ws_explorer_open") !== "0",
  unread: {},

  setExplorerOpen(open) {
    localStorage.setItem("ws_explorer_open", open ? "1" : "0");
    set({ explorerOpen: open });
  },

  markUnread(sessionId) {
    if (!sessionId) return;
    set((s) => (s.unread[sessionId] ? s : { unread: { ...s.unread, [sessionId]: true } }));
  },

  clearUnread(sessionId) {
    if (!sessionId) return;
    set((s) => (s.unread[sessionId] ? { unread: { ...s.unread, [sessionId]: false } } : s));
  },

  async refresh() {
    set({ sessions: await ipc.listSessions().catch(() => []) });
  },

  async loadProjects() {
    set({ projects: await ipc.listProjects().catch(() => []) });
  },

  async addProject(entry) {
    await ipc.saveProject(entry);
    set((s) => ({ projects: [...s.projects, entry] }));
  },

  async updateProject(entry) {
    await ipc.saveProject(entry);
    set((s) => ({ projects: s.projects.map((p) => (p.id === entry.id ? entry : p)) }));
  },

  async deleteProject(id) {
    const res = await ipc.deleteProject(id);
    // 级联：按 projectId 关闭该项目已打开的 Tab（review H-3：此前按 workspace 目录匹配，
    // 会误关恰好选了项目目录的临时会话，也漏掉目录已变更的项目会话）
    const closed = get().tabs.filter((t) => t.projectId === id).map((t) => t.key);
    for (const key of closed) useRun.getState().dispose(key);
    set((s) => {
      const tabs = s.tabs.filter((t) => t.projectId !== id);
      const activeKey =
        s.activeKey && closed.includes(s.activeKey) ? (tabs[0]?.key ?? null) : s.activeKey;
      return { tabs, activeKey, projects: s.projects.filter((p) => p.id !== id) };
    });
    await get().refresh();
    return res.deleted_sessions;
  },

  async openProjectSession(projectId) {
    if (get().loading) return; // M-6：去抖
    set({ loading: true });
    try {
      const project = get().projects.find((p) => p.id === projectId);
      const created = await ipc.createSession(projectId, null);
      const tab: Tab = {
        key: created.session_id,
        sessionId: created.session_id,
        workspace: created.workspace,
        title: created.project_name ?? project?.name ?? dirName(created.workspace),
        projectId: created.project_id,
        createdAt: new Date().toISOString(),
        prefs: { ...DEFAULT_PREFS },
      };
      set((s) => ({ tabs: [...s.tabs, tab], activeKey: tab.key }));
      useRun.getState().initTab(tab.sessionId);
      void ipc.connectMcp(tab.sessionId).catch(() => {});
      await get().refresh();
    } catch (e) {
      useUi.getState().toast(String(e)); // M-6：失败不再静默
    } finally {
      set({ loading: false });
    }
  },

  async openFreeSession(workspace?) {
    if (get().loading) return;
    set({ loading: true });
    try {
      // 临时会话免目录（需求 1.2）：不选目录直接开聊；workspace 传空由后端回退全局数据目录
      const created = await ipc.createSession(null, (workspace ?? null) as any);
      const tab: Tab = {
        key: created.session_id,
        sessionId: created.session_id,
        workspace: created.workspace,
        title: i18n.t("titlebar.tempSession"),
        projectId: null,
        createdAt: new Date().toISOString(),
        prefs: { ...DEFAULT_PREFS },
      };
      set((s) => ({ tabs: [...s.tabs, tab], activeKey: tab.key }));
      useRun.getState().initTab(tab.sessionId);
      void ipc.connectMcp(tab.sessionId).catch(() => {});
      await get().refresh();
    } catch (e) {
      useUi.getState().toast(String(e));
    } finally {
      set({ loading: false });
    }
  },

  async openSession(meta) {
    // 已打开则聚焦
    const existing = get().tabs.find((t) => t.sessionId === meta.id);
    if (existing) {
      set({ activeKey: existing.key });
      return;
    }
    if (get().loading) return;
    set({ loading: true });
    try {
      const msgs = await ipc.loadSession(meta.id, meta.workspace);
      const tab: Tab = {
        key: meta.id,
        sessionId: meta.id,
        workspace: meta.workspace,
        title: meta.title || (meta.workspace.split("/").pop() || i18n.t("sessions.untitled")),
        projectId: meta.project_id,
        createdAt: meta.created_at,
        prefs: { ...DEFAULT_PREFS },
      };
      set((s) => ({ tabs: [...s.tabs, tab], activeKey: tab.key }));
      // 回读会话级运行参数：同进程内关 Tab 再重开后与后端运行时对齐（漂移防护，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）
      void ipc
        .getSessionPrefs(tab.sessionId)
        .then((p) =>
          useSessions.setState((s) => ({
            tabs: s.tabs.map((t) => (t.sessionId === tab.sessionId ? { ...t, prefs: p } : t)),
          })),
        )
        .catch(() => {});
      const run = useRun.getState();
      run.initTab(tab.sessionId);
      run.restoreFromMessages(tab.sessionId, msgs);
      // M4：同进程关 Tab 不打断后端运行——重开时若仍在运行则恢复运行态
      void ipc
        .sessionRunning(tab.sessionId)
        .then((running) => {
          if (running) useRun.getState().markRunning(tab.sessionId);
        })
        .catch(() => {});
      await run.refreshGit(tab.sessionId);
      void ipc
        .getTokenBreakdown(tab.sessionId)
        .then((b) => useRun.getState().setBreakdown(tab.sessionId, b))
        .catch(() => {});
      void ipc.connectMcp(tab.sessionId).catch(() => {});
    } catch (e) {
      // H-4（迁移中遗失项）：loadSession 失败必须浮出，绝不静默成未处理的 rejection
      useUi.getState().toast(String(e));
    } finally {
      set({ loading: false });
    }
  },

  closeTab(key) {
    const idx = get().tabs.findIndex((t) => t.key === key);
    if (idx < 0) return;
    set((s) => {
      const tabs = s.tabs.filter((t) => t.key !== key);
      const activeKey =
        s.activeKey === key ? (tabs[Math.max(0, idx - 1)]?.key ?? null) : s.activeKey;
      return { tabs, activeKey };
    });
    // 同步清理运行态桶
    useRun.getState().dispose(key);
  },

  async removeSession(meta) {
    await ipc.deleteSession(meta.id);
    get().closeTab(meta.id);
    await get().refresh();
  },

  async rename(meta, title) {
    await ipc.renameSession(meta.id, title);
    set((s) => ({
      tabs: s.tabs.map((t) => (t.sessionId === meta.id ? { ...t, title } : t)),
    }));
    await get().refresh();
  },

  applyTitle(sessionId, title) {
    if (!sessionId || !title) return; // 空标题非法，忽略
    set((s) => ({
      sessions: s.sessions.map((m) => (m.id === sessionId ? { ...m, title } : m)),
      tabs: s.tabs.map((t) => (t.sessionId === sessionId ? { ...t, title } : t)),
    }));
  },

  revealSession(sessionId) {
    if (get().tabs.some((t) => t.sessionId === sessionId)) {
      set({ activeKey: sessionId });
      return true;
    }
    const meta = get().sessions.find((m) => m.id === sessionId);
    if (!meta) return false;
    void get().openSession(meta); // 内部自带去重（已打开则聚焦）并恢复运行态
    return true;
  },

  cycleTab(dir) {
    const { tabs, activeKey } = get();
    if (!tabs.length) return;
    const idx = tabs.findIndex((t) => t.key === activeKey);
    const next = (idx + dir + tabs.length) % tabs.length;
    set({ activeKey: tabs[next].key });
  },

  async updatePrefs(key, patch) {
    const prev = get().tabs.find((t) => t.key === key);
    if (!prev) return;
    const next: SessionPrefs = { ...prev.prefs, ...patch };
    // 乐观更新（Tab 切换天然隔离）；后端镜像失败则回滚并 toast
    set((s) => ({ tabs: s.tabs.map((t) => (t.key === key ? { ...t, prefs: next } : t)) }));
    try {
      await ipc.setSessionPrefs(key, next);
    } catch (e) {
      set((s) => ({ tabs: s.tabs.map((t) => (t.key === key ? { ...t, prefs: prev.prefs } : t)) }));
      useUi.getState().toast(String(e));
    }
  },
}));

// ---------- 未读点清除（[docs/ask-ink-accent-and-composer-cover](../../../docs/ask-ink-accent-and-composer-cover.md)） ----------
// 不变式：活跃会话永远无未读。activeKey 一变即清——
// openSession/revealSession/cycleTab/closeTab 邻位切换全部收口于这一处，新增入口自动覆盖。
useSessions.subscribe((state, prev) => {
  if (!state.activeKey || state.activeKey === prev.activeKey) return;
  if (state.unread[state.activeKey]) useSessions.getState().clearUnread(state.activeKey);
});

// ---------- 组件订阅 helper ----------
export const useActiveTab = (): Tab | null =>
  useSessions((s) => s.tabs.find((t) => t.key === s.activeKey) ?? null);
// key === sessionId，两者等值；保留语义名
export const useActiveId = (): string | null => useSessions((s) => s.activeKey);
export const useActiveWorkspace = (): string | null =>
  useSessions((s) => s.tabs.find((t) => t.key === s.activeKey)?.workspace ?? null);

/** 按会话 id 取标题，兜底应用名。 */
export function titleOf(state: SessionsState, sessionId: string): string {
  return state.tabs.find((t) => t.sessionId === sessionId)?.title ?? "CodeWave";
}
