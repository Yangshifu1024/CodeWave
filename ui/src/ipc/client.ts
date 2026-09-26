/**
 * invoke 封装：前端访问后端的唯一入口。
 * 契约：方法名与后端 IPC 命令一一对应（命令名 snake_case，一个字符不可改）；
 * 入参对象键名即 Tauri invoke 参数名；返回类型与 ipc/types.ts 的 serde 结构一一对应。
 * 组件不得直接 import @tauri-apps/api，新增命令一律收敛在此处。
 */
import { invoke } from "@tauri-apps/api/core";
import type { Channel } from "@tauri-apps/api/core";
import type { AgentMeta, CleanupOutcome, CleanupPreview, CleanupStatus, ConfigState, DailyStats, DocumentBackupEntry, EarlierPage, EditorInfo, GitDiffFile, GitLogEntry, GoalState, LegacyCleanupOutcome, LegacyCleanupPreview, LoadSessionPayload, LogFileContent, LogFileEntry, McpConfigDoc, McpSaveResult, McpScope, McpSnapshot, McpTestResult, Message, ProjectEntry, QuotaSnapshot, ScheduledTask, SessionFileEntry, SessionMeta, SessionPrefs, ShellInfo, SkillFull, SkillMeta } from "./types";

export const ipc = {
  ping: () => invoke<string>("ping"),

  listProjects: () => invoke<ProjectEntry[]>("list_projects"),
  saveProject: (entry: ProjectEntry) => invoke<void>("save_project", { entry }),
  deleteProject: (projectId: string) =>
    invoke<{ deleted_sessions: number }>("delete_project", { projectId }),

  getConfig: () => invoke<ConfigState>("get_config"),
  /** 保存整份配置。opts.skipCleanup = 本次跳过「按新保留期清理」这一步（用户在确认框里选了「暂不清理」）；
   *  返回本次清理结果（被删会话 id 列表 + 成败条数），保留期未变或本次跳过时为 null —— 调用方必须拿到返回值。 */
  saveConfig: (config: ConfigState, opts?: { skipCleanup?: boolean }) =>
    invoke<CleanupOutcome | null>("save_config", { config, skipCleanup: opts?.skipCleanup ?? false }),
  // 当前配置解析出的代理 URL（null = 直连）：检查更新传参与设置页系统代理回显共用
  resolveProxy: () => invoke<string | null>("resolve_proxy"),

  // ---------- 会话保留期与清理（[docs/session-cleanup](../../../docs/session-cleanup.md)）----------
  // 三个命令都是会话数据的批量删除路径，调用方必须显式面对返回值（删了几条）
  /** 预览：按给定保留期返回将删的会话条数与前几条标题（确认框文案数据源；days = null 时不会有候选） */
  previewSessionCleanup: (days: number | null) => invoke<CleanupPreview>("preview_session_cleanup", { days }),
  /** 执行清理（用**已保存**的保留期，不是未保存的草稿值）：返回本次被删会话 id 列表与成败条数 */
  runSessionCleanup: () => invoke<CleanupOutcome>("run_session_cleanup"),
  /** 上次清理记录（设置页只读行数据源；存后端，重启后仍在） */
  getCleanupStatus: () => invoke<CleanupStatus>("get_cleanup_status"),

  // ---------- 「清理旧格式历史」（分段 JSONL 落地后的显式入口）----------
  /** 预览可回收的旧格式历史：可回收会话数 / 字节数 + **必须保留**的会话数（无新格式数据 = 唯一副本） */
  previewLegacyHistoryCleanup: () => invoke<LegacyCleanupPreview>("preview_legacy_history_cleanup"),
  /** 执行清理（只删已有新格式数据的旧文件）：返回回收统计与被保留（跳过）的条数 */
  runLegacyHistoryCleanup: () => invoke<LegacyCleanupOutcome>("run_legacy_history_cleanup"),

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
  /** 恢复会话首屏（批2 P2 起返回 `{ messages, paging }`，不再是裸 `Message[]`）：`messages` = **最近一段**的 display
   *  口径转录（完整、不 trim）；更早内容由 `loadSessionEarlier` 按段前翻。命令名 `load_session` 不变。 */
  loadSession: (sessionId: string, workspace: string) =>
    invoke<LoadSessionPayload>("load_session", { sessionId, workspace }),
  /** 加载更早**一段**历史（分页前翻）：`beforeSeq` = 当前已加载到的最早段序号（首屏的 `paging.loaded_from_seq`）。
   *  只读一个段文件，翻页代价与已加载内容量无关；越界 / 已到最早 / 会话不存在返回空数组 + `from_seq: 0`，
   *  **不报错**（界面只需知道「没有更早内容了」，不当失败处理）。 */
  loadSessionEarlier: (sessionId: string, beforeSeq: number) =>
    invoke<EarlierPage>("load_session_earlier", { sessionId, beforeSeq }),
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
  restoreLegacyModelPrefs: (sessionId: string, modelId: string | null, reasoningEffort: SessionPrefs["reasoning_effort"]) =>
    invoke<void>("restore_legacy_model_prefs", { sessionId, modelId, reasoningEffort }),
  injectRunMessage: (sessionId: string, text: string) =>
    invoke<void>("inject_run_message", { sessionId, text }),
  resolveAsk: (sessionId: string, askId: string, value: any) =>
    invoke<void>("resolve_ask", { sessionId, askId, value }),
  compactSession: (sessionId: string) => invoke<void>("compact_session", { sessionId }),

  // ---------- 目标模式（`ApprovalMode::Goal`）----------
  /** 暂停后继续推进（与 `start_chat` 同构：注册事件 channel 并返回本轮 run_id；
   *  目标状态本身走 `goal:update` 事件——含起跑失败时的「执行中 → 已暂停」回滚） */
  resumeGoal: (sessionId: string, onEvent: Channel) =>
    invoke<string>("resume_goal", { sessionId, onEvent }),
  reopenGoal: (sessionId: string) => invoke<void>("reopen_goal", { sessionId }),
  setGoalBudget: (sessionId: string, budget: import("./types").GoalBudget) =>
    invoke<GoalState>("set_goal_budget", { sessionId, budget }),
  acceptGoal: (sessionId: string, accepted: boolean, feedback?: string) =>
    invoke<GoalState>("accept_goal", { sessionId, accepted, feedback }),
  /** 目标状态快照（会话恢复 / 右栏重挂载时拉取；null = 该会话当前无目标） */
  getSessionGoal: (sessionId: string) => invoke<GoalState | null>("get_session_goal", { sessionId }),

  searchWorkspacePaths: (sessionId: string, query: string, limit?: number) =>
    invoke<string[]>("search_workspace_paths", { sessionId, query, limit }),
  readWorkspaceFile: (sessionId: string, path: string) =>
    invoke<{ path: string; size: number; encoding: string; content: string }>("read_workspace_file", { sessionId, path }),

  // [docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)：四个文件入口
  /** 原生文件选择框（任意文件）；返回绝对路径列表 */
  selectDocumentFiles: () => invoke<string[]>("select_document_files"),
  /** 判定路径是否在会话可访问范围内（在外时给出所在目录与拿给模型的引用写法） */
  checkExternalPath: (sessionId: string, path: string) =>
    invoke<{ inside: boolean; dir: string; ref: string }>("check_external_path", { sessionId, path }),
  /** 放行一个目录：persist=true 写进项目设置（以后不再询问）；返回放行后的全部根 */
  allowExternalDir: (sessionId: string, dir: string, persist: boolean) =>
    invoke<string[]>("allow_external_dir", { sessionId, dir, persist }),

  // [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：会话产物
  listSessionFiles: (sessionId: string) =>
    invoke<SessionFileEntry[]>("list_session_files", { sessionId }),
  readWorkspaceFileBase64: (sessionId: string, path: string) =>
    invoke<{ path: string; size: number; content: string }>("read_workspace_file_base64", { sessionId, path }),

  // [docs/session-restore-fidelity](../../../docs/session-restore-fidelity.md)：工具结果原样 sidecar 批量回读。
  // 只应针对「历史里那份模型侧文本解析失败」的调用发起（那些才可能被瘦身/截断）；
  // 缺失 / 非法键 / 超限的条目不会出现在返回里，故前端无需区分「无备份」与「读失败」。
  loadToolOutcomes: (sessionId: string, callIds: string[]) =>
    invoke<{ call_id: string; outcome: any; duration_ms?: number | null }[]>("load_tool_outcomes", {
      sessionId,
      callIds,
    }),

  // [docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)：文档预览取数
  /** 表格 / 文档 / PDF 的结构化预览数据（与 read_document 工具同一条解析路径；失败时 reject 的错误文本形如 "E_XXX: 说明"） */
  previewDocument: (
    sessionId: string,
    path: string,
    opts?: { sheet?: string; range?: string; pages?: string },
  ) =>
    invoke<any>("preview_document", {
      sessionId,
      path,
      sheet: opts?.sheet ?? null,
      range: opts?.range ?? null,
      pages: opts?.pages ?? null,
    }),
  /** 分片读取原始字节（base64）：大文件不进一次性 base64 通道，避免单条 IPC 消息过大 */
  readFileChunk: (sessionId: string, path: string, offset: number, length?: number) =>
    invoke<{ offset: number; length: number; total: number; eof: boolean; content: string }>(
      "read_file_chunk",
      { sessionId, path, offset, length: length ?? null },
    ),
  /** 某个文档可回退的备份（新的在前）；没有备份返回空数组而不是报错 */
  listDocumentBackups: (sessionId: string, path: string) =>
    invoke<DocumentBackupEntry[]>("list_document_backups", { sessionId, path }),
  /** 把某一份备份还原回原文件位置；回退前会把当前内容也备份一份（回退本身能再撒回） */
  restoreDocumentBackup: (sessionId: string, path: string, backupPath: string) =>
    invoke<{ path: string; restoredFrom: string; currentBackup: string | null; size: number }>(
      "restore_document_backup",
      { sessionId, path, backupPath },
    ),

  gitStatus: (sessionId: string) =>
    invoke<{ repo: boolean; entries: any[]; branch?: string | null }>("git_status", { sessionId }),
  gitDiff: (sessionId: string, path?: string) =>
    invoke<{ files: GitDiffFile[]; truncated: boolean }>("git_diff", { sessionId, path }),
  gitRecentLog: (sessionId: string, n?: number) =>
    invoke<GitLogEntry[]>("git_recent_log", { sessionId, n }),
  gitUserInfo: (sessionId: string | null) =>
    invoke<{ name: string | null; email: string | null }>("git_user_info", { sessionId }),
  getTokenBreakdown: (sessionId: string) => invoke<any>("get_token_breakdown", { sessionId }),

  // ---------- MCP（[docs/mcp-module-rebuild](../../../docs/mcp-module-rebuild.md)）----------
  // 作用域化命令：scope 为 "global" | "project"；项目级需要 sessionId（用于定位项目目录）
  mcpListConfig: (scope: McpScope, sessionId?: string) =>
    invoke<McpConfigDoc>("mcp_list_config", { scope, sessionId: sessionId ?? null }),
  /** 保存某作用域配置。有 error 级 issue 时**不落盘**，返回值 `saved: false` + issues */
  mcpSaveConfig: (scope: McpScope, json: string, sessionId?: string) =>
    invoke<McpSaveResult>("mcp_save_config", { scope, json, sessionId: sessionId ?? null }),
  /** 建连（并行、立即返回；状态走 mcp:status 事件）。names 省略 = 整个可见集 */
  mcpConnect: (sessionId: string, names?: string[]) =>
    invoke<void>("mcp_connect", { sessionId, names: names ?? null }),
  /** 断开（names 省略 = 释放整个可见集） */
  mcpDisconnect: (sessionId: string, names?: string[]) =>
    invoke<void>("mcp_disconnect", { sessionId, names: names ?? null }),
  /** 重连单个 server（含被淘汰条目，绕过防抖立即重拉） */
  mcpReconnect: (sessionId: string, name: string) =>
    invoke<void>("mcp_reconnect", { sessionId, name }),
  /** 临时测试连接：起 → tools/list → 立即回收，**不并入连接池、不改正式状态** */
  mcpTest: (scope: McpScope, name: string, sessionId?: string) =>
    invoke<McpTestResult>("mcp_test", { scope, name, sessionId: sessionId ?? null }),
  /** 某会话的状态快照（断开 / 淘汰 / 批量停止后拉全量） */
  mcpSnapshot: (sessionId: string) => invoke<McpSnapshot>("mcp_snapshot", { sessionId }),

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
  /** 改任务正文（改完 next_run 会按调度器重算） */
  updateScheduledTask: (id: string, name: string, instruction: string, schedule: string) =>
    invoke<ScheduledTask>("update_scheduled_task", { id, name, instruction, schedule }),
  setScheduledTaskEnabled: (id: string, enabled: boolean) =>
    invoke<ScheduledTask>("set_scheduled_task_enabled", { id, enabled }),
  /** 手工触发一次（后端异步跑，立即返回）；已有任务在跑时后端 reject */
  runScheduledTaskNow: (id: string) => invoke<void>("run_scheduled_task_now", { id }),
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

  // 额度快照：行集合 = CodeWave 供应商配置（后端按「可查询类在前 + 配置顺序、unsupported 殿后」排好序）；
  // activeProviderId = 当前会话生效模型所属供应商 uuid，仅用于后端标注「当前」
  quotaSnapshots: (activeProviderId?: string | null) =>
    invoke<QuotaSnapshot[]>("quota_snapshots", { activeProviderId: activeProviderId ?? null }),

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

  // ---------- 界面字体（[docs/custom-font-and-titlebar]）----------
  /** 字体偏好落盘（真源在配置文件的 ui.font_sans / ui.font_mono；只写这两个字段） */
  setFontPrefs: (sans: string, mono: string) => invoke<void>("set_font_prefs", { sans, mono }),
};
