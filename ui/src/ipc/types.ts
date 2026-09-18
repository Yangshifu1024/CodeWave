// 与 Rust 侧 serde 结构一一对应（[docs/technical-design](../../../docs/technical-design.md) §5 协议；手写绑定 = [docs/technical-design](../../../docs/technical-design.md) 预留的 specta 兜底路径）

/** 会话 id（后端 uuid；前端 Tab 的 key 与之等值） */
export type SessionId = string;

/** 转录消息角色 */
export type Role = "system" | "user" | "assistant" | "tool";

/** 纯文本块 */
export interface ContentText { type: "text"; text: string }
/** 思考块（extended thinking，按生成顺序落盘） */
export interface ContentThinking { type: "thinking"; text: string }
/** 工具调用块：id 与配对 tool_result 的 tool_use_id 对应 */
export interface ContentToolUse { type: "tool_use"; id: string; name: string; args: any }
/** 工具结果块：tool_use_id 回指调用块；is_error 标记失败 */
export interface ContentToolResult { type: "tool_result"; tool_use_id: string; content: string; is_error: boolean }
/** 图片块：base64 编码（data 不带 data: 前缀） */
export interface ContentImage { type: "image"; media_type: string; data: string }
/** 消息内容块联合：一条消息由若干内容块按生成顺序组成 */
export type Content =
  | ContentText | ContentThinking | ContentToolUse | ContentToolResult | ContentImage;

/** 后端转录消息：created_at 仅新会话存在（旧存量为空）；前端无时间留空渲染、绝不伪造「现在」 */
export interface Message { role: Role; content: Content[]; created_at?: string | null }

/** 供应商 API 协议形态（provider 层三协议，协议差异不出该层） */
export type ApiFormat = "openai_chat" | "anthropic_messages" | "openai_responses";

/** provider 下的模型条目（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：wire id 即显示名；上下文窗口/输出/vision 类型一切以用户输入为准 */
export interface ProviderModel {
  id: string;
  /** wire 模型 id（请求体 model 字段） */
  model: string;
  max_tokens: number;
  context_window: number;
  reasoning_effort: string | null;
  /** 输入类型：图片 */
  vision: boolean;
  /** 输入类型：视频（预留标志） */
  video: boolean;
}

/** 模型 provider（[docs/provider-management-refactor](../../../docs/provider-management-refactor.md)）：端点 + 协议 + key 池 + 自带模型列表（无内置目录） */
export interface ProviderConfig {
  id: string;
  name: string;
  api_format: ApiFormat;
  base_url: string;
  keys: string[];
  models: ProviderModel[];
  /** 供应商级自定义请求头（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)）：值支持 ${session_id} 占位符；明文存储 */
  headers: HeaderPair[];
}

/** 自定义请求头条目（[docs/provider-custom-headers](../../../docs/provider-custom-headers.md)） */
export interface HeaderPair {
  name: string;
  value: string;
}

/** 菜单/守卫消费的摊平模型视图（provider + model 摊平，见 utils/models.ts） */
export interface FlatModel {
  id: string;
  providerId: string;
  providerName: string;
  model: string;
  vision: boolean;
  video: boolean;
}

// ---------- 会话级运行参数（[docs/composer-toolbar-batch-report](../../../docs/composer-toolbar-batch-report.md)，与 core/prefs.rs 一一对应） ----------

/** 权限四档：逐条确认 / 自动编辑 / 计划 / 完全访问 */
export type ApprovalMode = "confirm_each" | "auto_edit" | "plan" | "full_access";
/** 推理力度档位 */
export type EffortLevel = "low" | "medium" | "high" | "max";
export interface SessionPrefs {
  approval_mode: ApprovalMode;
  /** null = 跟随全局活跃模型 */
  model_id: string | null;
  /** null = 跟随模型配置的推理力度 */
  reasoning_effort: EffortLevel | null;
}
/** 用户消息图片附件（base64） */
export interface ImageIn { mime: string; data: string }

/** 默认会话参数：plan 档 + 跟随全局模型与模型力度 */
export const DEFAULT_PREFS: SessionPrefs = { approval_mode: "plan", model_id: null, reasoning_effort: null };

/** 审批门设置（fence 判定后的交互策略） */
export interface ApprovalSettings { enabled: boolean; confirm_outside_create: boolean; confirm_git_push: boolean; /** docs/ask-ink-accent-and-composer-cover：无应答 5 分钟后自动确认推荐选项；false = 永不超时（无限等待） */ auto_confirm: boolean; command_allowlist: string[] }
/** 保存后自动验证的语言开关（[docs/lsp-diagnostics]：语义校验按语言分流；json 走内置解析，不属 LSP 六语言） */
export interface ValidationSettings {
  python: boolean;
  rust: boolean;
  typescript: boolean;
  go: boolean;
  json: boolean;
  /** Dart/Flutter 语义校验（可选 = 后端 serde default 向前兼容；缺省按后端默认 true 显示） */
  dart?: boolean;
  /** Java 语义校验（默认关闭：jdtls 需 JDK 21+ 与依赖树索引，首次启用需用户确认） */
  java?: boolean;
  /** LSP 服务配置（命令覆盖 / JDK / 额外 SDK 根 / 预算）；缺省 = DEFAULT_LSP_SETTINGS */
  lsp?: LspSettings;
}
    /** shell 探测结果（list_available_shells 返回；kind 为 posix|powershell|cmd|wsl；auto = 即自动探测默认项） */
    export interface ShellInfo { id: string; name: string; path: string | null; kind: string; limited: boolean; auto: boolean }
/** shell 选择配置（null = 自动：执行时由后端自动探测） */
export interface ShellConfig { selection: string | null }
/** 界面偏好（字号/强调色/界面语言 + AI 回复语言） */
export interface UiPrefs {
  font_size: number;
  accent: string;
  language: string;
  /** AI 回复语言（自由输入，如 "中文"/"English"）；空 = 跟随用户消息语言 */
  ai_language?: string | null;
}
/** [docs/session-logging-report](../../../docs/session-logging-report.md)：全局日志级别（RUST_LOG 优先）+ 会话详尽模式（LLM 请求/响应全文进会话日志） */
export interface LogConfig { level: string; session_verbose: boolean }
/** 全局配置（config schema v2，与 Rust 侧 ConfigState 一一对应；新字段必须 serde default 向前兼容） */
export interface ConfigState {
  /** schema 版本（v2 = providers 嵌套 models，见 [docs/provider-management-refactor](../../../docs/provider-management-refactor.md)） */
  schema_version: number;
  providers: ProviderConfig[];
  /** 活跃模型（指向摊平后的模型条目 id，null = 未配置） */
  active_model_id: string | null;
  proxy: { mode: "none" | "system" | "manual"; url: string } | null;
  network: { allow_private_network: boolean };
  /** 上下文占用超过该比例触发自动压缩 */
  compact_threshold: number;
  /** 自动压缩超时秒数（[docs/tool-optimizations-port](../../../docs/tool-optimizations-port.md) 起可配） */
  compact_timeout_seconds: number;
  approval: ApprovalSettings;
  validation: ValidationSettings;
  ui: UiPrefs;
  /** 用户自定义系统提示词附加段（null = 无） */
  custom_prompt: string | null;
  /** 被禁用的技能名列表 */
  disabled_skills: string[];
  log: LogConfig;
  /** shell 选择（shell-selection-batch：可选 = 后端 serde default 向前兼容；null/缺省 = 自动探测） */
  shell?: ShellConfig;
}

/** [docs/session-logging-report](../../../docs/session-logging-report.md)：运行日志读取契约（全局滚动日志 / 会话日志） */
export interface LogFileEntry { name: string; size: number; modified: string | null }
/** 日志内容：truncated = 尾部截断读取 */
export interface LogFileContent { content: string; truncated: boolean }

/** [docs/session-artifacts-and-files-tab](../../../docs/session-artifacts-and-files-tab.md)：会话产物登记（create/edit 边车 + 统计），first_op/last_op ∈ "create" | "edit" */
export interface SessionFileEntry {
  path: string;
  first_op: "create" | "edit";
  last_op: "create" | "edit";
  first_at: string;
  last_at: string;
  count: number;
  exists: boolean;
  size: number;
}

/** 会话元数据（左栏导航 / 会话列表数据源）。
 *  契约锚点：project_id + roots 是创建时固化的快照，是 @ 提及 / git 聚合的唯一数据源，勿绕过回查项目注册表。 */
export interface SessionMeta {
  id: string;
  title: string;
  /** 主目录（临时会话 = 全局数据目录） */
  workspace: string;
  model_id: string | null;
  created_at: string;
  updated_at: string;
  message_count: number;
  /** 所属项目（null = 临时会话） */
  project_id: string | null;
  /** 创建时固化的全部可读写根目录快照（含主目录） */
  roots: string[];
  /** 后端视角是否有任务正在运行（退出拦截与列表运行 spinner 的真值源） */
  running: boolean;
  /** 上次运行被中断的标记（崩溃 = crash / 正常退出前中止 = quit；null = 无中断）；清除走 clear_session_interrupt */
  interrupted: { kind: "crash" | "quit"; at: string } | null;
}

/** 具名项目：名称 + 项目主目录（单目录语义）；
 *  data_dir = <主目录>/.codewave（为空时按 <主目录>/.codewave 推导，保存时归一化持久化） */
export interface ProjectEntry {
  id: string;
  name: string;
  directory: string;
  data_dir?: string | null;
  created_at: string;
}

// ---------- 高频 Channel 帧 ----------

/** 流式帧契约（start_chat 的 Channel 下发）：gen 为代际阈值（run:retry 后低于该代际的半帧丢弃），
 *  帧到达顺序 = timeline 展示顺序；sub 信封借用父会话通道区分来源（零新增事件键）。 */
export type Frame =
  | { type: "delta_text"; gen: number; text: string }
  | { type: "delta_thinking"; gen: number; text: string }
  | { type: "tool_progress"; batch: string; index: number; chunk: string; name?: string }
  | { type: "usage"; input: number; output: number; cache_read: number; cache_write: number }
  /** 子代理帧信封（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)）：在父会话通道上区分来源，按 sub_id 路由进过程抽屉消息流 */
  | { type: "sub"; sub_id: string; frame: Frame };

// ---------- 事件 payload ----------

/** 上下文 token 分布（tokens:update 事件 payload）：system=系统提示词 / history=历史消息 /
 *  tool_results=工具结果 / tool_schema=工具 schema / total=合计 / context_window=窗口大小 / ratio=占窗口比例（0–1） */
export interface Breakdown {
  system_tokens: number; history_tokens: number; tool_results_tokens: number;
  tool_schema_tokens: number; total_tokens: number; context_window: number; ratio: number;
}

/** 工具错误：code 为机器可读错误码 */
export interface ToolError { code: string; message: string }
/** 工具执行结果：成功带 data，失败带 error，均可携带 warnings */
export interface ToolOutcome { ok: boolean; data: any; error?: ToolError; warnings?: string[] }

/** 工具结果事件（tool:result / tool:error payload） */
export interface ToolResultEvent {
  session: string; run_id: string; batch_id: string; call_index: number;
  /** 工具卡锚点键（= batch:index，与 tool_progress 帧一致） */
  call_key: string; tool: string; args_preview: string;
  outcome: ToolOutcome; duration_ms: number;
}

/** ask:opened payload：询问/审批卡数据 */
export interface AskOpenedEvent {
  session: string; ask_id: string;
  kind: "ask" | "approval";
  /** 问题列表（single = 单选题；options 带 recommended 推荐标记） */
  questions?: { id: string; question: string; single?: boolean; options?: { id: string; label: string; description?: string; recommended?: boolean }[] }[];
  title?: string; detail?: string;
  /** arch 审批门（[docs/arch-orchestrator](../../../docs/arch-orchestrator.md)）：批准后把权限胶囊同步为自动编辑档（多余字段，零新增事件键） */
  switch_to_auto_edit?: boolean;
  /** [docs/ask-approval-shape-note-nav](../../../docs/ask-approval-shape-note-nav.md)：批准形形状标记（后端宽松识别 = 单一事实源：单题 + id/label 命中批准协议） */
  approval?: boolean;
  /** [docs/ask-approval-shape-note-nav](../../../docs/ask-approval-shape-note-nav.md)：批准项 id（前端单选互斥/直提判定用；null = 未识别，前端回退严格判定） */
  approve_id?: string | null;
  /** 子代理审批（2026-09-11 tester 卡死修复）：事件挂主会话下发后，来源子代理 id；null = 主会话审批 */
  sub_id?: string | null;
}

/** git status 行条目：index/worktree 两区 × new/modified/deleted/renamed 状态位 */
export interface GitStatusEntry {
  path: string; index_new: boolean; index_modified: boolean; index_deleted: boolean;
  worktree_new: boolean; worktree_modified: boolean; worktree_deleted: boolean; renamed: boolean;
}

/** 单文件 diff（git_diff 返回，供审批/变更页签渲染） */
export interface GitDiffFile {
  path: string; status: string; additions: number; deletions: number; patch: string;
  /** 多根项目：所属根目录 */
  root?: string | null;
}

/** 最近提交日志条目（git_recent_log 返回；git 集成只读） */
export interface GitLogEntry { short_id: string; summary: string; author: string; time: number }

/** 技能元数据（多目录扫描；origin 标记来源，deletable = 托管 .codewave/skills 来源可在设置页删除） */
export interface SkillMeta { name: string; description: string; whenToUse: string; origin: string; deletable: boolean }

/** 技能完整内容（get_skill 返回；body = SKILL.md 正文） */
export interface SkillFull { meta: SkillMeta; body: string }

/** 内置子代理角色摘要（list_agents 返回；后端注册表剔除内部 title 角色，不含 body 正文） */
export interface AgentMeta { name: string; description: string }

/** 计划 todo 条目（plan:update 事件下发） */
export interface Todo { title: string; status: "pending" | "in_progress" | "completed" }

// ---------- P2：计划任务 / 统计 ----------

/** 计划任务登记与最近一次执行状态 */
export interface ScheduledTask {
  id: string; name: string; instruction: string; schedule: string;
  next_run: string | null; last_status: string | null; last_summary: string | null;
}

/** 按维度聚合的 token 用量 */
export interface ModelAgg { input: number; output: number; cache_read: number; cache_write: number; runs: number }

/** 单日 token 统计（统计页数据源） */
export interface DailyStats {
  date: string;
  by_model: Record<string, ModelAgg>;
  by_workspace: Record<string, ModelAgg>;
  /** 按来源聚合：main（主会话）/ sub（子代理）/ task（计划任务） */
  by_kind?: Record<string, ModelAgg>;
  total: ModelAgg;
}

/** 子代理事件族 payload（sub:spawn/step/report/usage/done/error 共用） */
export interface SubagentEvent {
  session: string; sub_id: string; role?: string; description?: string;
  max_steps?: number; step?: number; tool?: string; usage?: any; error?: string;
  /** 运行摘录（sub:step 采样，最近一段 text/thinking 的尾部） */
  detail?: string;
  /** 最终报告（sub:report） */
  report?: string;
  /** 注册表规范角色名（[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)，展示优先于原始 role 字符串） */
  name?: string | null;
  /** 完整任务文本（截断至 2000 字符，过程抽屉首块展示，[docs/subagent-interaction-drawer](../../../docs/subagent-interaction-drawer.md)） */
  task?: string;
  /** 收尾时的真实已启动步数（sub:done；轮询采样可能滞后，以它刷新最终展示） */
  steps_used?: number;
  /** 收尾原因（sub:done）：report = 按约定带 <report> 标记正常汇报；budget = 步数预算耗尽；no_report = 未按约定汇报即结束（疑似提前退出） */
  ended?: "report" | "budget" | "no_report";
}

// ---------- 订阅额度 / 打开器（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）----------

/** 已检测到的编辑器（后端候选表顺序：VS Code → Cursor → Windsurf → Zed → Sublime Text → Notepad++ → JetBrains 组） */
export interface EditorInfo {
  id: string;
  name: string;
  /** 可执行文件路径（仅展示/排障用，前端不拼命令） */
  path: string;
}

/**
 * 一行额度事实：百分比行（`remaining_percent`）或数值行（`value_text`）二居其一。
 * `label` 是厂商自带标签（Kimi/Zhipu 的 limits）；无 label 时前端按 `key` 做 i18n 映射。
 */
export interface QuotaEntry {
  key: string;
  label: string | null;
  used_percent: number | null;
  remaining_percent: number | null;
  value_text: string | null;
  /** 重置时间（RFC3339） */
  resets_at: string | null;
}

/** 提供商快照状态：invalid = 凭证形态不可用（重试无用）；未配置凭证的提供商不会出现在列表里 */
export type QuotaStatus = "ok" | "invalid" | "error";

/** 一家提供商的额度快照 */
export interface QuotaSnapshot {
  provider_id: string;
  display_name: string;
  status: QuotaStatus;
  entries: QuotaEntry[];
  error: string | null;
  /** 凭证来源（env:NAME / opencode.jsonc / opencode.json / auth.json）——悬浮排障用 */
  credential_source: string | null;
  fetched_at: string;
}

// ---------- LSP 语义校验（六语言 server 探测 / 安装引导 / 状态回显） ----------
// 与后端 src-tauri/src/lsp/mod.rs 的 Lang / ServerStatus / InstallHint 一一对应。

/** 受支持语言（JSON 走内置解析不在此列）；稳定 id 与后端 `Lang::id()`、配置字段名、事件 payload 同源 */
export type LspLanguage = "typescript" | "rust" | "python" | "go" | "java" | "dart";

/** 设置页行序（后端 `Lang::all()` 同序） */
export const LSP_LANGUAGES: LspLanguage[] = ["typescript", "rust", "python", "go", "java", "dart"];

/** 安装引导形态三景：installable = 可一键安装（TS/JS、Python 走 npx；Go 走 go install）；
 *  manual = 需手动安装（Java：jdtls 无跨平台官方装法）；confirm_enable = 默认关闭需先确认（Java）。 */
export type LspInstallKind = "installable" | "manual" | "confirm_enable";

/** 安装引导信息（command 仅 installable 有值；docs_url 在 manual 必填） */
export interface LspInstallHint {
  kind: LspInstallKind;
  command: string | null;
  docs_url: string | null;
  /** 前置条件（如「需 JDK 21+」） */
  prerequisite: string | null;
}

/** 单语言 server 状态（`lsp_status` / `lsp_redetect` 返回；设置页状态徽标数据源） */
export interface LspServerStatus {
  language: LspLanguage;
  /** 该语言开关（config.validation.<language>）是否打开 */
  enabled: boolean;
  /** 是否找到可用 server */
  found: boolean;
  /** 来源：config（命令覆盖）| project | path | fresh_path | extra_root | npx | heuristic | ""（未找到） */
  source: "config" | "project" | "path" | "fresh_path" | "extra_root" | "npx" | "heuristic" | "";
  /** 解析出的启动命令（展示用，含参数） */
  command: string;
  version: string | null;
  /** 人类可读补充（未找到原因 / JDK 缺失等，与后端回喂文案同源） */
  detail: string;
  /** 未启用或未找到时才有值 */
  install: LspInstallHint | null;
}

/** 六语言 server 命令覆盖（留空 = 自动探测）；映射形态让按语言动态读写保持类型安全 */
export type LspCommands = Record<LspLanguage, string>;

/** 语言服务器诊断的全局预算与发现配置（与后端 core/config.rs 的 LspSettings 一一对应；新字段必须 serde default 向前兼容） */
export interface LspSettings {
  commands: LspCommands;
  /** jdtls 使用的 JDK 21+ 路径（留空 = 自动探测；不读 JAVA_HOME，它常指向旧版本） */
  java_home: string;
  /** 额外 SDK 根目录（如 D:\\Sdk；探测 <root>/<lang>/bin） */
  extra_roots: string[];
  /** 写后同步等待诊断的毫秒预算 */
  sync_window_ms: number;
  /** 单次回喂的诊断条数上限 */
  max_diagnostics: number;
  /** 单次回喂的诊断文本字符上限 */
  max_chars: number;
  /** 项目级 server 闲置回收时长（毫秒） */
  idle_ttl_ms: number;
  /** 单项目并发 server 上限 */
  max_servers: number;
  /** 超过该体积的文件跳过语义校验 */
  max_file_bytes: number;
  /** 同一诊断指纹在同一文件最多回喂次数 */
  dedupe_limit: number;
}

/** 后端 `LspSettings::default()` 同形镜像：旧配置缺 lsp 段时的回显与写回基准（数值必须与后端一致） */
export const DEFAULT_LSP_SETTINGS: LspSettings = {
  commands: { typescript: "", rust: "", python: "", go: "", java: "", dart: "" },
  java_home: "",
  extra_roots: [],
  sync_window_ms: 1500,
  max_diagnostics: 20,
  max_chars: 4000,
  idle_ttl_ms: 600000,
  max_servers: 8,
  max_file_bytes: 1048576,
  dedupe_limit: 2,
};

/** 命令覆盖读写助手：按语言取/置 `commands.<lang>`（避免各处手写索引与展开）。 */
export function lspCommandOf(s: LspSettings, lang: LspLanguage): string {
  return s.commands?.[lang] ?? "";
}

export function withLspCommand(s: LspSettings, lang: LspLanguage, value: string): LspSettings {
  return { ...s, commands: { ...s.commands, [lang]: value } };
}
