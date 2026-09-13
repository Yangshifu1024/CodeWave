// UI 偏好：语言、主题、面板开关、通知堆栈（强调色锁定中性墨色，无自定义强调色）
import { create } from "zustand";

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
  tasksOpen: boolean;
  statsOpen: boolean;
  /** 关于弹框（[docs/oss-prep-batch](../../../docs/oss-prep-batch.md) 批次）：从左下角状态区打开 */
  aboutOpen: boolean;
  /** 右栏开合持久态（[docs/sidebar-toggle-buttons](../../../docs/sidebar-toggle-buttons.md)）：localStorage 记忆，重启保留 */
  rightBarOpen: boolean;
  setRightBarOpen(open: boolean): void;
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
  tasksOpen: false,
  statsOpen: false,
  aboutOpen: false,
  rightBarOpen: localStorage.getItem("ws_right_bar_open") !== "0",
  setRightBarOpen(open) {
    localStorage.setItem("ws_right_bar_open", open ? "1" : "0");
    set({ rightBarOpen: open });
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
