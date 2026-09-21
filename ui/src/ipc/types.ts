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
  /** 界面字体（逗号分隔的已安装字体名，空 = 默认链）。
   *  **真源在后端配置文件**；WebView 的 localStorage 只当首帧缓存（防闪变）。 */
  font_sans?: string;
  /** 等宽字体（代码与日志），同上。 */
  font_mono?: string;
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
  /** 写入后检查命令设置（[docs/post-write-check-plan](../../../docs/post-write-check-plan.md)） */
  post_write_check: PostWriteCheckSettings;
  ui: UiPrefs;
  /** 用户自定义系统提示词附加段（null = 无） */
  custom_prompt: string | null;
  /** 被禁用的技能名列表 */
  disabled_skills: string[];
  log: LogConfig;
  /** shell 选择（shell-selection-batch：可选 = 后端 serde default 向前兼容；null/缺省 = 自动探测） */
  shell?: ShellConfig;
  /** 会话保留期与清理（[docs/session-cleanup](../../../docs/session-cleanup.md)）：`retention_days` = 保留天数（1/3/7/14/30），
   *  `null` = 不清理（默认）。可选 = 后端 serde default 向前兼容（同 `shell?` 口径），读取请用
   *  `config.sessions?.retention_days ?? null`——老配置里没有这一段。 */
  sessions?: { retention_days: number | null };
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

/** [docs/office-and-pdf-support](../../../docs/office-and-pdf-support.md)：文档修改前留下的备份（界面「回退」入口的数据源）；at 是备份时刻（RFC3339） */
export interface DocumentBackupEntry {
  path: string;
  at: string;
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
  /** 最近打开时间（RFC3339；加载会话时由后端刷新，旧数据缺省 = null）。
   *  只参与会话清理的「更早」判定（max(updated_at, last_opened_at)）；列表排序与行内时间仍用 updated_at。 */
  last_opened_at?: string | null;
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

// ---------- 会话保留期与清理（[docs/session-cleanup](../../../docs/session-cleanup.md)） ----------

/** 清理预览：按给定保留期算出将删除的会话条数 + 前几条标题，以及索引外的残留文件数
 *  （保存前的确认框数据源；纯读，不删任何东西）。
 *  `count` = 会话数；`orphan_count` = 本次会删掉的「索引外残留文件」数（会话列表里早已看不到的历史 / 边车文件）。
 *  判定「有没有事可做」看两者之和：`count + orphan_count === 0` 才不打扰用户。 */
export interface CleanupPreview { count: number; titles: string[]; orphan_count: number }

/** 清理结果：被删会话 id 列表 + 成功/失败条数（单条失败不中断整批，失败的那条下次清理会再试） */
export interface CleanupOutcome { ids: string[]; deleted: number; failed: number }

/** 上次清理记录（存后端，重启后仍在；启动时的自动清理也计入） */
export interface CleanupStatus { last_run_at: string | null; last_deleted: number; last_failed: number }

// ---------- 高频 Channel 帧 ----------

/** 流式帧契约（start_chat 的 Channel 下发）：gen 为代际阈值（run:retry 后低于该代际的半帧丢弃），
 *  帧到达顺序 = timeline 展示顺序；sub 信封借用父会话通道区分来源（零新增事件键）。 */
export type Frame =
  | { type: "delta_text"; gen: number; text: string }
  | { type: "delta_thinking"; gen: number; text: string }
  | { type: "tool_progress"; batch: string; index: number; chunk: string; name?: string }
  | { type: "usage"; input: number; output: number; cache_read: number; cache_write: number;
      /** 该 LLM step **成功尝试**的生成耗时（请求发出 → 流失结束）；缺省 = 无数据（旧后端） */
      duration_ms?: number | null;
      /** 首个任意类型增量（text 或 thinking）的延迟；缺省 = 无数据 */
      ttft_ms?: number | null }
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

/** 工具开始事件（tool:start）：同一 call_key 可能先 waiting 再 running（写工具在审批/范围确认门期间 waiting）；只读工具只收一次 running */
export interface ToolStartEvent {
  session: string; batch_id: string; call_index: number;
  /** 工具卡锚点键（= batch:index，与 tool_progress 帧 / tool:result 一致） */
  call_key: string; tool: string; args_preview: string;
  phase: "waiting" | "running";
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

/** 按维度聚合的 token 用量。四把耗时字段可选（serde default）：
 *  旧记录（[docs/composer-token-rate] 之前）没有它们 → 统计面板总览的加权三项必须**把这类记录整条排除**
 *  （分子分母同域），否则「分子含全部 output、分母只含部分耗时」会把速率算虚高。 */
export interface ModelAgg {
  input: number; output: number; cache_read: number; cache_write: number; runs: number;
  /** Σ 各步生成耗时（同一 step 内多次尝试只计成功那次） */
  gen_ms?: number;
  /** Σ TTFT */
  ttft_ms?: number;
  /** TTFT 样本数（= 计入的步数，用于平均） */
  ttft_count?: number;
  /** 计入的 LLM 步数（用于均步耗时） */
  steps?: number;
}

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

// ---------- 额度与余额 / 打开器（[docs/rightbar-info-refactor-and-subscription-quota](../../../docs/rightbar-info-refactor-and-subscription-quota.md)）----------

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
  /** 窗口级状态（后端原样透传：ok / rate-limited / exceeded / …）；null = 无状态信息，未知值前端原样显示 */
  status: string | null;
}

/**
 * 一家供应商的快照状态（行集合 = CodeWave 供应商配置，见 [docs/quota-from-provider-config](../../../docs/quota-from-provider-config.md)）：
 * - ok：查到了额度；error：查询失败（可重试）；invalid：密钥读取失败（重试无用）；
 * - rejected：查询被拒（401/403/404 且该家**从未成功过**，密钥或套餐被拒，重试无用）——这条定义决定了
 *   它的 `last_ok_at` 恒为 null；反过来「曾成功过的 401/403/404」一律归 `error`，历史成功记录就是两者的分界；
 * - no_key：未配置密钥；unsupported：不支持额度查询（reason 给出原因：no_adapter / empty_base_url）。
 */
export type QuotaStatus = "ok" | "error" | "invalid" | "rejected" | "no_key" | "unsupported";

/** 一家供应商的额度快照 */
export interface QuotaSnapshot {
  /** CodeWave 供应商 uuid（与 config.providers[].id 同源） */
  provider_id: string;
  /** 后端给的 provider.name（空则已回落主机名） */
  display_name: string;
  status: QuotaStatus;
  /** 状态补充说明：unsupported 时为 "no_adapter" | "empty_base_url"，其余状态为 null */
  reason: string | null;
  entries: QuotaEntry[];
  error: string | null;
  /** 密钥来源："keyring"（系统钥匙串）|"config"（配置文件）；null = 未知，不显示 */
  key_source: string | null;
  fetched_at: string;
  /**
   * 该供应商**上次成功查询**的时刻（RFC3339 秒）：`ok` 行 = 本次成功时间（= `fetched_at`），
   * 其它状态 = `~/.codewave/quota.json` 里持久化的上次成功时间；从未成功过为 null。
   */
  last_ok_at: string | null;
}

// ---------- 写入后检查命令（[docs/post-write-check-plan](../../../docs/post-write-check-plan.md)） ----------
// 取代 LSP 写后语义校验：create/edit 成功后执行用户配置的一条命令，结论进工具结果 data。

/** 写入后检查设置（与后端 core/config.rs 的 PostWriteCheckSettings 一一对应） */
export interface PostWriteCheckSettings {
  /** 总开关（默认关闭） */
  enabled: boolean;
  /** 检查命令（默认空串 = 未配置；含 {file} 时每个被写文件执行一次） */
  command: string;
  /** 单次执行超时秒数（默认 30） */
  timeout_seconds: number;
  /** 交给模型的输出尾部字符数（默认 3000） */
  tail_chars: number;
}

/** 后端 `PostWriteCheckSettings::default()` 同形镜像：旧配置缺该段时的回显与写回基准（数值必须与后端一致） */
export const DEFAULT_POST_WRITE_CHECK: PostWriteCheckSettings = {
  enabled: false,
  command: "",
  timeout_seconds: 30,
  tail_chars: 3000,
};
