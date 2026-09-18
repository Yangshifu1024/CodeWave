/**
 * invoke 封装：前端访问后端的唯一入口。
 * 契约：方法名与后端 IPC 命令一一对应（命令名 snake_case，一个字符不可改）；
 * 入参对象键名即 Tauri invoke 参数名；返回类型与 ipc/types.ts 的 serde 结构一一对应。
 * 组件不得直接 import @tauri-apps/api，新增命令一律收敛在此处。
 */
import { invoke } from "@tauri-apps/api/core";
import type { Channel } from "@tauri-apps/api/core";
import type { AgentMeta, ConfigState, DailyStats, EditorInfo, GitDiffFile, GitLogEntry, LogFileContent, LogFileEntry, Message, ProjectEntry, QuotaSnapshot, ScheduledTask, SessionFileEntry, SessionMeta, SessionPrefs, ShellInfo, SkillFull, SkillMeta } from "./types";

export const ipc = {
  ping: () => invoke<string>("ping"),

  listProjects: () => invoke<ProjectEntry[]>("list_projects"),
  saveProject: (entry: ProjectEntry) => invoke<void>("save_project", { entry }),
  deleteProject: (projectId: string) =>
    invoke<{ deleted_sessions: number }>("delete_project", { projectId }),

  getConfig: () => invoke<ConfigState>("get_config"),
  saveConfig: (config: ConfigState) => invoke<void>("save_config", { config }),
  // 当前配置解析出的代理 URL（null = 直连）：检查更新传参与设置页系统代理回显共用
  resolveProxy: () => invoke<string | null>("resolve_proxy"),

  // shell 探测（设置面板 Shell 下拉数据源）
  listAvailableShells: () => invoke<ShellInfo[]>("list_available_shells"),

  createSession: (projectId: string | null, workspace: string | null) =>
    invoke<{
      session_id: string;
      workspace: string;
      project_id: string | null;
      project_name?: string;
      roots: string[];
    }>("create_session", { projectId: projectId ?? null, workspace: workspace ?? null }),
  selectWorkspaceDir: () => invoke<string | null>("select_workspace_dir"),
  listSessions: () => invoke<SessionMeta[]>("list_sessions"),
  loadSession: (sessionId: string, workspace: string) =>
    invoke<Message[]>("load_session", { sessionId, workspace }),
  // [docs/titlebar-logo-toggle](../../../docs/titlebar-logo-toggle.md)：子智能体过程历史（会话恢复后由过程抽屉按需重建消息流；未落盘时返回空）
  loadSubagentHistory: (sessionId: string, subId: string) =>
    invoke<Message[]>("load_subagent_history", { sessionId, subId }),
  deleteSession: (sessionId: string) => invoke<void>("delete_session", { sessionId }),
  renameSession: (sessionId: string, title: string) =>
    invoke<void>("rename_session", { sessionId, title }),

  startChat: (sessionId: string, text: string, images: { mime: string; data: string }[], onEvent: Channel) =>
    invoke<string>("start_chat", { sessionId, text, images, onEvent }),
  cancelRun: (sessionId: string) => invoke<void>("cancel_run", { sessionId }),
  // 子代理卡单独停止（后端 stop_subagent 命令既有；主会话停止的级联取消走 cancel_run）
  stopSubagent: (sessionId: string, subId: string) =>
    invoke<void>("stop_subagent", { sessionId, subId }),
  sessionRunning: (sessionId: string) => invoke<boolean>("session_running", { sessionId }),
  setSessionPrefs: (sessionId: string, prefs: SessionPrefs) =>
    invoke<void>("set_session_prefs", { sessionId, prefs }),
  getSessionPrefs: (sessionId: string) => invoke<SessionPrefs>("get_session_prefs", { sessionId }),
  injectRunMessage: (sessionId: string, text: string) =>
    invoke<void>("inject_run_message", { sessionId, text }),
  resolveAsk: (sessionId: string, askId: string, value: any) =>
    invoke<void>("resolve_ask", { sessionId, askId, value }),
  compactSession: (sessionId: string) => invoke<void>("compact_session", { sessionId }),

  searchWorkspacePaths: (sessionId: string, query: string, limit?: number) =>
    invoke<string[]>("search_workspace_paths", { sessionId, query, limit }),
  readWorkspaceFile: (sessionId: string, path: string) =>
    invoke<{ path: string; size: number; content: string }>("read_workspace_file", { sessionId, path }),

  // [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：会话产物
  listSessionFiles: (sessionId: string) =>
    invoke<SessionFileEntry[]>("list_session_files", { sessionId }),
  readWorkspaceFileBase64: (sessionId: string, path: string) =>
    invoke<{ path: string; size: number; content: string }>("read_workspace_file_base64", { sessionId, path }),

  gitStatus: (sessionId: string) =>
    invoke<{ repo: boolean; entries: any[]; branch?: string | null }>("git_status", { sessionId }),
  gitDiff: (sessionId: string, path?: string) =>
    invoke<{ files: GitDiffFile[]; truncated: boolean }>("git_diff", { sessionId, path }),
  gitRecentLog: (sessionId: string, n?: number) =>
    invoke<GitLogEntry[]>("git_recent_log", { sessionId, n }),
  gitUserInfo: (sessionId: string | null) =>
    invoke<{ name: string | null; email: string | null }>("git_user_info", { sessionId }),
  getTokenBreakdown: (sessionId: string) => invoke<any>("get_token_breakdown", { sessionId }),

  // MCP 服务器配置与连接
  getMcpConfig: () => invoke<string>("get_mcp_config"),
  saveMcpConfig: (json: string) => invoke<void>("save_mcp_config", { json }),
  connectMcp: (sessionId: string) =>
    invoke<{ started: any[]; failed: any[] }>("connect_mcp", { sessionId }),
  mcpStatus: () => invoke<{ name: string; state: any; tools: number }[]>("mcp_status"),

  // Skills 技能扫描与启停
  listSkills: (sessionId: string | null) => invoke<SkillMeta[]>("list_skills", { sessionId }),
  getSkill: (sessionId: string | null, name: string) =>
    invoke<SkillFull | null>("get_skill", { sessionId, name }),
  toggleSkill: (sessionId: string | null, name: string, disabled: boolean) =>
    invoke<void>("toggle_skill", { sessionId, name, disabled }),
  reloadSkills: (sessionId: string | null) => invoke<SkillMeta[]>("reload_skills", { sessionId }),
  deleteSkill: (sessionId: string | null, name: string) =>
    invoke<void>("delete_skill", { sessionId, name }),

  // 内置子代理角色（composer $ 菜单数据源；后端 agents 注册表剔除内部 title 角色）
  listAgents: () => invoke<AgentMeta[]>("list_agents"),

  // P2：计划任务 / 统计
  listScheduledTasks: (sessionId: string) => invoke<ScheduledTask[]>("list_scheduled_tasks", { sessionId }),
  createScheduledTask: (sessionId: string, name: string, instruction: string, schedule: string) =>
    invoke<ScheduledTask>("create_scheduled_task", { sessionId, name, instruction, schedule }),
  deleteScheduledTask: (sessionId: string, id: string) =>
    invoke<void>("delete_scheduled_task", { sessionId, id }),
  getTokenStats: (days: number) => invoke<DailyStats[]>("get_token_stats", { days }),
  stopService: (sessionId: string, serviceId: string) =>
    invoke<void>("stop_service", { sessionId, serviceId }),

  // 运行日志（[docs/session-logging-report](../../../docs/session-logging-report.md)）
  listLogFiles: () => invoke<LogFileEntry[]>("list_log_files"),
  readLogFile: (name: string, tailLines?: number) =>
    invoke<LogFileContent>("read_log_file", { name, tailLines }),
  readSessionLog: (sessionId: string, projectId?: string | null, tailLines?: number) =>
    invoke<LogFileContent>("read_session_log", { sessionId, projectId, tailLines }),
  openLogsDir: () => invoke<void>("open_logs_dir"),

  // 关于弹框：应用版本（首次打开时惰性加载）+ 打开全局数据目录 + 外部仓库链接
  appVersion: () => invoke<string>("app_version"),
  openDataDir: () => invoke<void>("open_data_dir"),
  openUrl: (url: string) => invoke<void>("open_url", { url }),

  // 应用重启（自动更新下载安装完成后调用；updater 替换产物后必须 relaunch 才运行新版本）
  restartApp: () => invoke<void>("restart_app"),

  // 是否运行在 Linux AppImage 中（只有它能自替换二进制；deb/rpm 降级为手动下载）
  isAppimage: () => invoke<boolean>("is_appimage"),

  // 右侧栏信息页签：在系统文件管理器中打开任意目录（项目主目录 / 临时会话工作区）
  openDir: (path: string) => invoke<void>("open_dir", { path }),

  // 编辑器探测与打开（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）
  listEditors: () => invoke<EditorInfo[]>("list_editors"),
  openInEditor: (editorId: string, path: string) => invoke<void>("open_in_editor", { editorId, path }),

  // 订阅额度：只返回检测到凭证的提供商；activeBaseUrl 命中的那家置顶
  quotaSnapshots: (activeBaseUrl?: string | null) =>
    invoke<QuotaSnapshot[]>("quota_snapshots", { activeBaseUrl: activeBaseUrl ?? null }),

  // 系统通知点击回跳（tauri-plugin-notification 桌面端无点击回调，后端按平台原生直驱；失败回退插件路径）
  notifySystem: (sessionId: string, title: string, body: string) =>
    invoke<"native" | "unsupported">("notify_system", { sessionId, title, body }),

  // ---------- 会话保存与恢复（会话保存与恢复优化 · 批1）----------
  // ui-state.json：会话现场态（Tab 集合/顺序/活跃 Tab/滚动锚点/草稿/队列/树展开/未读/面板态/窗口几何）。
  // 结构归前端所有（后端只校验 schema === 1 与体积上限），因此这里用 unknown 透传，类型与校验在 utils/uiState.ts。
  getUiState: () => invoke<unknown | null>("get_ui_state"),
  setUiState: (state: unknown) => invoke<void>("set_ui_state", { state }),
  // 退出拦截：后端询问时下发运行中会话列表（app:exit_requested 事件），前端弹窗后回 resolve_exit_request
  listRunningSessions: () => invoke<string[]>("list_running_sessions"),
  resolveExitRequest: (action: "exit" | "cancel" | "abort" | "wait") =>
    invoke<void>("resolve_exit_request", { action }),
  // 崩溃/退出中断标记清除（左栏会话行的中断徽标）：调用后由调用方刷新会话列表
  clearSessionInterrupt: (sessionId: string) =>
    invoke<void>("clear_session_interrupt", { sessionId }),
};
