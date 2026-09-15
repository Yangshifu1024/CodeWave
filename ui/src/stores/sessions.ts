// 会话与多 Tab 状态：会话依附于「项目」（单主目录，docs/12 语义）或作为临时会话免目录存在
import { create } from "zustand";
import { ipc } from "../ipc/client";
import type { ProjectEntry, SessionMeta, SessionPrefs } from "../ipc/types";
import { DEFAULT_PREFS } from "../ipc/types";
import { useRun } from "./run";
import { useUi } from "./ui";
import { applyRetainedContent, dropTabContent, retainTabContent, tabHasPendingContent } from "../utils/uiState";
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
  /** 骨架标记（会话保存与恢复优化 · 批1）：false = 仅骨架（重启恢复的非活跃 Tab，首次激活才拉消息）；
   *  缺省/true = 已加载（新建会话与从导航打开）——大量既有构造点无需改动，只有恢复路径显式写 false */
  loaded?: boolean;
}

/** 重启恢复的输入（由 utils/uiState 的 ui-state 快照转出）：只含骨架元数据，不含消息 */
export interface RestoredTab {
  key: string;
  workspace: string;
  title: string;
  projectId: string | null;
  createdAt: string;
  prefs: SessionPrefs;
}

/** 相邻回落：在候选集里取离 sourceIdx 最近的 Tab（同项目优先由调用方经 filter 控制） */
function nearestTab(tabs: Tab[], sourceIdx: number, filter?: (t: Tab) => boolean): Tab | null {
  const cands = filter ? tabs.filter(filter) : tabs;
  if (cands.length === 0) return null;
  let best = cands[0];
  let bestDist = Infinity;
  for (const t of cands) {
    const dist = Math.abs(tabs.indexOf(t) - sourceIdx);
    if (dist < bestDist) {
      best = t;
      bestDist = dist;
    }
  }
  return best;
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
  /** 重启恢复（会话保存与恢复优化 · 批1）：按 ui-state 重建 Tab 骨架；会话已删/项目已删的引用静默剔除。
   *  返回保留下来的 sessionId（顺序同 Tab 条），活跃 Tab 失效时回落邻位（同项目优先） */
  restoreTabs(input: {
    tabs: RestoredTab[];
    activeKey: string | null;
    activeProject: string | null;
    unread: Record<string, boolean>;
  }): string[];
  /** 激活 Tab（切 Tab/通知回跳统一入口）：骨架 Tab 首次激活时才拉消息（惰性加载），已加载过的不重复拉 */
  activate(key: string): void;
  addProject(entry: ProjectEntry): Promise<void>;
  updateProject(entry: ProjectEntry): Promise<void>;
  deleteProject(id: string): Promise<number>;
  openProjectSession(projectId: string): Promise<void>;
  openFreeSession(workspace?: string): Promise<void>;
  openSession(meta: SessionMeta): Promise<void>;
  /** 关闭 Tab（会话保存与恢复优化 · 批1）：有未发送内容（草稿/前端队列）时不直接关，
   *  改为置 `ui.closeTabRequest` 交 AppShell 的确认弹窗（Cmd+W / 顶栏 / 左栏三个入口共用一处 UI）。
   *  force = 跳过确认（删除会话等用户已明确丢弃内容的路径） */
  closeTab(key: string, opts?: { force?: boolean }): void;
  /** 落实关 Tab 确认结果：discard = 丢弃未发送内容；keep = 搬进驻留表（重开该会话时回填） */
  resolveCloseTab(key: string, choice: "discard" | "keep"): void;
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

/** 拉取会话内容并回填运行态（从导航打开与惰性激活共用的唯一加载路径）。
 *  抛出交给调用方浮出（H-4：loadSession 失败不得静默） */
async function loadTabContent(tab: Tab): Promise<void> {
  const msgs = await ipc.loadSession(tab.sessionId, tab.workspace);
  const run = useRun.getState();
  run.initTab(tab.sessionId);
  run.restoreFromMessages(tab.sessionId, msgs);
  // 回读会话级运行参数：同进程内关 Tab 再重开后与后端运行时对齐（漂移防护，[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)）
  void ipc
    .getSessionPrefs(tab.sessionId)
    .then((p) =>
      useSessions.setState((s) => ({
        tabs: s.tabs.map((t) => (t.sessionId === tab.sessionId ? { ...t, prefs: p } : t)),
      })),
    )
    .catch(() => {});
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

  restoreTabs(input) {
    const known = new Set(get().sessions.map((s) => s.id));
    const projIds = new Set(get().projects.map((p) => p.id));
    const kept: Tab[] = [];
    for (const raw of input.tabs) {
      // 失效引用静默剔除：会话不在 listSessions 结果里（已删）或所属项目已删 ⇒ 丢弃该项，不报错
      if (!raw?.key || !known.has(raw.key)) continue;
      if (raw.projectId && !projIds.has(raw.projectId)) continue;
      kept.push({
        key: raw.key,
        sessionId: raw.key,
        workspace: raw.workspace,
        title: raw.title || raw.workspace.split("/").filter(Boolean).pop() || i18n.t("sessions.untitled"),
        projectId: raw.projectId,
        createdAt: raw.createdAt || new Date().toISOString(),
        prefs: raw.prefs ?? { ...DEFAULT_PREFS },
        loaded: false, // 骨架：只有活跃 Tab 立即拉消息，其余首次激活才拉
      });
    }
    // 活跃 Tab 失效 ⇒ 回落相邻 Tab（同项目优先，其次全局邻位，都无效则空态）
    let activeKey =
      input.activeKey && kept.some((t) => t.key === input.activeKey) ? input.activeKey : null;
    if (!activeKey && kept.length) {
      const srcIdx = input.tabs.findIndex((t) => t.key === input.activeKey);
      const sameProject = input.activeProject
        ? nearestTab(kept, srcIdx >= 0 ? srcIdx : 0, (t) => t.projectId === input.activeProject)
        : null;
      const near = srcIdx >= 0 ? nearestTab(kept, srcIdx) : null;
      activeKey = (sameProject ?? near ?? kept[0]).key;
    }
    // 未读集合恢复上次快照（不是启动全标未读）；只保留仍存在的会话
    const unread: Record<string, boolean> = {};
    for (const [id, flag] of Object.entries(input.unread ?? {})) {
      if (flag && known.has(id)) unread[id] = true;
    }
    set({ tabs: kept, activeKey, unread });
    return kept.map((t) => t.key);
  },

  activate(key) {
    const tab = get().tabs.find((t) => t.key === key);
    if (!tab) return;
    set({ activeKey: key });
    if (tab.loaded === false) {
      // 先置已加载再拉取：连点行/快捷键切 Tab 不会并发重复拉；失败保持已加载标记，
      // 避免每次激活都重试同一失败（重试入口：重新从导航打开该会话）
      set((s) => ({ tabs: s.tabs.map((t) => (t.key === key ? { ...t, loaded: true } : t)) }));
      void loadTabContent(tab).catch((e) => useUi.getState().toast(String(e)));
    }
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
    // 级联删除：项目下所有会话随磁盘一起没了，逐个清掉驻留内容（关 Tab 时选择「保留」的草稿/队列/面板）。
    // 不清则快照仍会把它写盘、下次启动 loadUiState 又搬回驻留表 → 孤儿草稿永久复现（审查 E14）；
    // 必须在 refresh() 之前枚举：刷新后这些会话已不在列表里，无从知道该清谁
    for (const m of get().sessions.filter((s) => s.project_id === id)) dropTabContent(m.id);
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
        loaded: true,
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
        loaded: true,
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
    // 已打开则聚焦；骨架 Tab 走 activate 触发首次拉取（惰性加载）
    const existing = get().tabs.find((t) => t.sessionId === meta.id);
    if (existing) {
      get().activate(existing.key);
      return;
    }
    if (get().loading) return;
    set({ loading: true });
    try {
      const tab: Tab = {
        key: meta.id,
        sessionId: meta.id,
        workspace: meta.workspace,
        title: meta.title || (meta.workspace.split("/").pop() || i18n.t("sessions.untitled")),
        projectId: meta.project_id,
        createdAt: meta.created_at,
        prefs: { ...DEFAULT_PREFS },
        loaded: true,
      };
      set((s) => ({ tabs: [...s.tabs, tab], activeKey: tab.key }));
      // 关 Tab 时选择「保留草稿」的内容在此回填（重开后草稿/队列原样回来）
      applyRetainedContent(tab.sessionId);
      await loadTabContent(tab);
    } catch (e) {
      // H-4（迁移中遗失项）：loadSession 失败必须浮出，绝不静默成未处理的 rejection；
      // Tab 保留（骨架仍在），用户可重新从导航打开重试
      useUi.getState().toast(String(e));
    } finally {
      set({ loading: false });
    }
  },

  closeTab(key, opts) {
    const idx = get().tabs.findIndex((t) => t.key === key);
    if (idx < 0) return;
    if (!opts?.force && tabHasPendingContent(key)) {
      useUi.getState().setCloseTabRequest(key);
      return;
    }
    set((s) => {
      const tabs = s.tabs.filter((t) => t.key !== key);
      const activeKey =
        s.activeKey === key ? (tabs[Math.max(0, idx - 1)]?.key ?? null) : s.activeKey;
      return { tabs, activeKey };
    });
    // 同步清理运行态桶
    useRun.getState().dispose(key);
  },

  resolveCloseTab(key, choice) {
    useUi.getState().setCloseTabRequest(null);
    if (choice === "keep") retainTabContent(key);
    else dropTabContent(key);
    get().closeTab(key, { force: true });
  },

  async removeSession(meta) {
    await ipc.deleteSession(meta.id);
    // 删除会话是明确丢弃：内容随磁盘历史一起没了，不再弹「保留草稿」确认。
    // 此前关 Tab 选择「保留」而寄存在驻留表的内容同样必须清掉——否则快照照旧写盘，
    // 下次启动又搬回驻留表，用户会看到已删会话的草稿（审查 E14）
    dropTabContent(meta.id);
    get().closeTab(meta.id, { force: true });
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
      get().activate(sessionId);
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
    get().activate(tabs[next].key); // 走 activate：骨架 Tab 切过去时才拉消息
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
