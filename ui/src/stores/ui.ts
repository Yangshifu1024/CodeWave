// UI 偏好：语言、主题、面板开关、通知堆栈（强调色锁定中性墨色，无自定义强调色）
import { create } from "zustand";
import { clampNavWidth, clampRightBarWidth, NAV_W_DEFAULT, RB_W_DEFAULT } from "../utils/layout";
// 树展开/折叠态的落盘链路（uiState → ipc.setUiState）。本文件与 utils/uiState 互为引用（那边要读 useUi），
// 但双方都只在函数体内取用对方的绑定，模块顶层互不触达，ESM 活绑定足以支撑这个环。
import {
  setTreeCollapsed as persistTreeCollapsed,
  setTreeExpanded as persistTreeExpanded,
} from "../utils/uiState";

/** 界面语言 */
type Lang = "zh-CN" | "en-US";

/** 主题偏好档位：system = 跟随系统（默认，现状行为）；light/dark = 用户手动固定 */
export type ThemePref = "system" | "light" | "dark";

const THEME_STORAGE_KEY = "ws_theme";

/** 从 localStorage 读主题偏好；值缺失或非法（手改/旧数据）一律回退 "system" */
export function readStoredTheme(): ThemePref {
  if (typeof localStorage === "undefined") return "system";
  const raw = localStorage.getItem(THEME_STORAGE_KEY);
  return raw === "light" || raw === "dark" ? raw : "system";
}

/** UI 域 store：面板开关、通知、MCP 状态等界面态 */
interface UiState {
  language: Lang;
  /** 主题偏好档位（设置 → 外观选择；localStorage 持久化，即时生效） */
  theme: ThemePref;
  settingsOpen: boolean;
  /** 设置弹窗当前页签（通用/外观/供应商/安全/mcp/技能）：提升进 store 以便外部调用方（认证错误引导等）指定页签打开（[docs/auth-error-guidance](../../../docs/auth-error-guidance.md)） */
  settingsTab: string;
  /** 退出拦截请求（后端 app:exit_requested 下发运行中会话列表）：AppShell 消费后弹三选项或直接放行；null = 无待处理退出请求。
   *  放在 ui store 是因为事件 handler 与弹窗分处两层（runHandlers 注册、AppShell 渲染），store 是二者唯一交点 */
  exitRequest: { running: string[] } | null;
  /** 关 Tab 请求（有草稿/未发队列时由 sessions.requestCloseTab 置位）：待确认的 Tab key，null = 无。
   *  同样放 store：Cmd+W / 顶栏 / 左栏等关闭入口分散在多处，弹窗只在 AppShell 渲染一处 */
  closeTabRequest: string | null;
  /** 设置/清除关 Tab 请求（null = 关闭弹窗） */
  setCloseTabRequest(key: string | null): void;
  tasksOpen: boolean;
  statsOpen: boolean;
  /** 左栏会话树「显示更多」的展开集合（key = 分组 key，如 `proj:<项目 id>`）。
   *  放 store 而不是组件 useState / 模块内存：hydrate（异步读盘）晚于 ProjectNav 首渲染，
   *  组件挂载时读到的永远是空值；store 的 setState 能把后到货的快照推给已挂载的订阅者
   *  （会话保存与恢复优化 · 批1） */
  treeExpand: Record<string, boolean>;
  /** 左栏「项目」区是否折叠（同样是随快照恢复的左栏态） */
  treeCollapsed: boolean;
  /** 展开/折叠某项目分组的「显示更多」（next 省略 = 按当前值取反） */
  setTreeGroupExpanded(key: string, next?: boolean): void;
  /** 折叠/展开左栏「项目」区 */
  setTreeCollapsed(collapsed: boolean): void;
  /** 关于弹框（[docs/oss-prep-batch](../../../docs/oss-prep-batch.md) 批次）：从左下角状态区打开 */
  aboutOpen: boolean;
  /** 右栏开合持久态（[docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md)）：localStorage 记忆，重启保留 */
  rightBarOpen: boolean;
  setRightBarOpen(open: boolean): void;
  /** 左栏宽度（可拖拽，localStorage 全局记忆；显示时按窗口宽度夹取，见 utils/layout） */
  navWidth: number;
  setNavWidth(width: number): void;
  /** 右栏宽度（同上） */
  rightBarWidth: number;
  setRightBarWidth(width: number): void;
  /** 右栏当前页签（变更/信息/日志/文件）：提升进 store 以便外部调用方（Composer 命令等）切换 */
  rbTab: string;
  setRbTab(tab: string): void;
  /** 打开「变更」：确保右栏展开并落在变更页签 */
  showChanges(): void;
  /** 打开设置弹窗，可指定落地页签（默认「通用」） */
  showSettings(tab?: string): void;
  /** 空态引导：ProjectNav 监听此标志打开新建项目弹框（用完即复位） */
  createProjectRequested: boolean;
  mcpStatus: { name: string; state: any; tools: number }[];
  /** 应用内通知堆栈；带 sessionId 时点击可回跳对应会话（通知点击回跳批次） */
  notifications: { id: number; title: string; body: string; sessionId?: string }[];
  setLanguage(lang: Lang): void;
  /** 切换主题偏好档位：即时生效（视觉属性），localStorage 持久化（与字体/右栏开合同惯例） */
  setTheme(theme: ThemePref): void;
  notify(title: string, body: string, sessionId?: string): void;
  toast(body: string): void;
  /** 点击/消费后移除单条通知 */
  dismiss(id: number): void;
  /** 系统通知点击回跳后，顺带清除同一会话的应用内通知 */
  dismissBySession(sessionId: string): void;
}

export const useUi = create<UiState>((set, get) => ({
  language: (localStorage.getItem("ws_lang") as Lang) || "zh-CN",
  theme: readStoredTheme(),
  settingsOpen: false,
  settingsTab: "general",
  exitRequest: null,
  closeTabRequest: null,
  setCloseTabRequest(key) {
    set({ closeTabRequest: key });
  },
  tasksOpen: false,
  statsOpen: false,
  treeExpand: {},
  treeCollapsed: false,
  // 单一写入口：状态写进 store + 触发防抖落盘都在 uiState 的 setTreeExpanded/setTreeCollapsed 里（这里不再自己 set，
  // 免得多一次 setState 通知与重渲染）
  setTreeGroupExpanded(key, next) {
    const cur = get().treeExpand;
    persistTreeExpanded({ ...cur, [key]: next ?? !cur[key] });
  },
  setTreeCollapsed(collapsed) {
    persistTreeCollapsed(collapsed);
  },
  aboutOpen: false,
  rightBarOpen: localStorage.getItem("ws_right_bar_open") !== "0",
  setRightBarOpen(open) {
    localStorage.setItem("ws_right_bar_open", open ? "1" : "0");
    set({ rightBarOpen: open });
  },
  // 栏宽：读盘即收敛到合法区间（手改/旧数据/NaN 一律回默认），拖动只走 setNavWidth/RightBarWidth
  navWidth: localStorage.getItem("ws_nav_width") === null
    ? NAV_W_DEFAULT
    : clampNavWidth(Number(localStorage.getItem("ws_nav_width"))),
  setNavWidth(width) {
    const normalized = clampNavWidth(width);
    localStorage.setItem("ws_nav_width", String(normalized));
    set({ navWidth: normalized });
  },
  rightBarWidth: localStorage.getItem("ws_rb_width") === null
    ? RB_W_DEFAULT
    : clampRightBarWidth(Number(localStorage.getItem("ws_rb_width"))),
  setRightBarWidth(width) {
    const normalized = clampRightBarWidth(width);
    localStorage.setItem("ws_rb_width", String(normalized));
    set({ rightBarWidth: normalized });
  },
  rbTab: "info",
  setRbTab(tab) {
    set({ rbTab: tab });
  },
  showChanges() {
    get().setRightBarOpen(true);
    set({ rbTab: "changes" });
  },
  showSettings(tab?: string) {
    set({ settingsOpen: true, settingsTab: tab ?? "general" });
  },
  createProjectRequested: false,
  mcpStatus: [],
  notifications: [],
  setLanguage(lang: Lang) {
    localStorage.setItem("ws_lang", lang);
    set({ language: lang });
  },
  setTheme(theme: ThemePref) {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
    set({ theme });
  },
  notify(title: string, body: string, sessionId?: string) {
    const id = Date.now() + Math.random();
    set((s) => ({ notifications: [...s.notifications, { id, title, body, sessionId }] }));
    setTimeout(() => {
      set((s) => ({ notifications: s.notifications.filter((n) => n.id !== id) }));
    }, 6000);
  },
  dismiss(id) {
    set((s) => ({ notifications: s.notifications.filter((n) => n.id !== id) }));
  },
  dismissBySession(sessionId) {
    set((s) => ({ notifications: s.notifications.filter((n) => n.sessionId !== sessionId) }));
  },
  toast(body: string) {
    get().notify("CodeWave", body);
  },
}));
