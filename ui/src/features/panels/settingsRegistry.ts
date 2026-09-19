// 设置项注册表（[docs/settings-ia](../../../../docs/settings-ia.md)）：页集合 / 导航分组 / 页名键 /
// 旧页 key 别名 / 设置项清单 / 页字段归属，全部集中在本模块。
//
// 设计要点：
//   · 纯数据（无 React、无 store 依赖）——stores/ui.ts 因此可直接引用它做页 key 归一；
//   · 页面 JSX 仍手写（**不做数据驱动渲染**）：本表供左导航、脏标记与契约测试消费；
//   · 新增一项设置的登记流程：先在 SETTINGS_ITEMS 里登记该项（id / labelKey / page / group），
//     再把它的配置字段路径补进 PAGE_FIELDS —— 漏登记会被 `__tests__/settings.registry.test.ts` 拦住
//     （引用闭包 / 键存在性 / 页合法 / 分组完备 / 页字段覆盖 / 字段归属唯一 / 「项 ↔ 页字段」双向闭合 /
//      豁免清单不重叠）。末条是结构性断言：删掉任一条字段登记都会立刻变红，不再是「删了也全绿」。

/** 设置页的 8 个分区（页 key = 组件内的页标识，也是 ui.settingsTab 的取值域） */
export type PageKey =
  | "appearance"
  | "providers"
  | "network"
  | "security"
  | "tools"
  | "agent"
  | "logs"
  | "about";

/** 页序（= 左导航逐组展开后的顺序；脏标记与分组完备性断言都按它走） */
export const PAGE_ORDER: PageKey[] = [
  "appearance",
  "providers",
  "network",
  "security",
  "tools",
  "agent",
  "logs",
  "about",
];

/** 默认页：设置页打开时的落点，也是非法 / 未知页 key 的回退值 */
export const DEFAULT_PAGE: PageKey = "appearance";

/** 页名键（zh-CN / en-US 双侧同名；导航项与操作条标题共用） */
export const PAGE_LABEL_KEY: Record<PageKey, string> = {
  appearance: "settings.pageAppearance",
  providers: "settings.pageProviders",
  network: "settings.pageNetwork",
  security: "settings.pageSecurity",
  tools: "settings.pageTools",
  agent: "settings.pageAgent",
  logs: "settings.pageLogs",
  about: "settings.pageAbout",
};

/** 左导航三组（组内页序 = PAGE_ORDER 中的相对序；titleKey 是组标题的 i18n 键） */
export const PAGE_GROUPS: { titleKey: string; pages: PageKey[] }[] = [
  { titleKey: "settings.groupUiModel", pages: ["appearance", "providers", "network"] },
  { titleKey: "settings.groupSafetyTools", pages: ["security", "tools", "agent"] },
  { titleKey: "settings.groupDiagnostics", pages: ["logs", "about"] },
];

/**
 * 旧页 key → 新页 key 别名表（深链兼容：`showSettings("mcp")` 这类历史调用点仍落到正确页）。
 * 未登记（未知 / 空）一律回退 DEFAULT_PAGE —— 见 normalizePageKey。
 */
export const LEGACY_PAGE_ALIASES: Record<string, PageKey> = {
  general: "appearance", // 旧「通用」按项拆散：界面语言 → 界面，其余 → 工作区与智能体 / 日志
  appearance: "appearance",
  providers: "providers",
  security: "security",
  network: "network",
  mcp: "tools", // 旧「MCP」→ 工具与集成
  skills: "tools", // 旧「技能」→ 工具与集成
};

/** 页 key 归一：合法页 key 原样返回；旧 key 走别名表；未知 / 空回默认页 */
export function normalizePageKey(raw?: string | null): PageKey {
  if (!raw) return DEFAULT_PAGE;
  if ((PAGE_ORDER as string[]).includes(raw)) return raw as PageKey;
  return LEGACY_PAGE_ALIASES[raw] ?? DEFAULT_PAGE;
}

/**
 * 一个设置项（导航 / 搜索 / 契约校验的最小单元）。
 * - `id`：字段路径（config 字段如 `network.proxy`、localStorage 偏好如 `ui.theme`）
 *   或 `app.*`（只读信息与动作，如版本、检查更新）——全表唯一；
 * - `labelKey`：该项显示名键（zh-CN / en-US 双侧必须存在）；
 * - `group`：页内分组标题键（仅在有分组的页上出现：tools 的校验 / 预算 / 发现 / MCP / 技能）；
 * - `advanced`：进阶项（批③ 的「显示进阶」折叠将消费它，本批只登记不渲染）；
 * - `keywords`：直字符串搜索词（**不进 i18n**：中英混排的常见说法，批③ 搜索用）。
 */
export interface SettingItem {
  id: string;
  labelKey: string;
  page: PageKey;
  group?: string;
  advanced?: boolean;
  keywords?: string[];
}

/** 全部设置项（登记制：改动设置项归属时必须同步本表与 PAGE_FIELDS） */
export const SETTINGS_ITEMS: SettingItem[] = [
  // ---------- 界面 ----------
  { id: "ui.theme", labelKey: "settings.theme", page: "appearance", keywords: ["theme", "dark", "light", "主题", "暗色", "亮色"] },
  { id: "ui.font_sans", labelKey: "settings.uiFont", page: "appearance", keywords: ["font", "sans", "字体", "界面字体"] },
  { id: "ui.font_mono", labelKey: "settings.monoFont", page: "appearance", keywords: ["font", "mono", "等宽", "代码字体"] },
  { id: "ui.language", labelKey: "settings.language", page: "appearance", keywords: ["language", "语言", "界面语言"] },

  // ---------- 模型与供应商 ----------
  { id: "providers", labelKey: "settings.providers", page: "providers", keywords: ["provider", "供应商", "模型", "api", "base url", "key"] },
  { id: "active_model_id", labelKey: "settings.active", page: "providers", advanced: true, keywords: ["active", "当前", "活跃模型"] },
  { id: "ui.ai_language", labelKey: "settings.aiLanguage", page: "providers", keywords: ["ai", "language", "回复语言", "ai 语言"] },

  // ---------- 网络与连接 ----------
  { id: "network.proxy", labelKey: "settings.proxyMode", page: "network", keywords: ["proxy", "代理", "socks", "http"] },
  { id: "network.allow_private_network", labelKey: "settings.allowPrivate", page: "network", keywords: ["private", "内网", "局域网", "本地模型"] },

  // ---------- 安全与审批 ----------
  { id: "approval.enabled", labelKey: "settings.approvalEnabled", page: "security", keywords: ["approval", "确认", "危险命令", "弹窗"] },
  { id: "approval.confirm_outside_create", labelKey: "settings.confirmOutside", page: "security", keywords: ["workspace", "工作区", "新建路径"] },
  { id: "approval.confirm_git_push", labelKey: "settings.confirmPush", page: "security", keywords: ["git", "push", "确认"] },
  { id: "approval.auto_confirm", labelKey: "settings.autoConfirm", page: "security", keywords: ["auto", "自动确认", "超时", "5 分钟"] },
  { id: "approval.command_allowlist", labelKey: "settings.cmdAllowlist", page: "security", advanced: true, keywords: ["allowlist", "白名单", "允许", "命令"] },

  // ---------- 工具与集成（页内三组：校验 / 预算 / 发现 + MCP + 技能） ----------
  // 六语言各一行（开关 + 命令覆盖），行序与后端 Lang::all() 同源
  { id: "validation.typescript", labelKey: "settings.validationLangTypescript", page: "tools", group: "settings.validation", keywords: ["typescript", "javascript", "vue", "校验"] },
  { id: "validation.rust", labelKey: "settings.validationLangRust", page: "tools", group: "settings.validation", keywords: ["rust", "校验"] },
  { id: "validation.python", labelKey: "settings.validationLangPython", page: "tools", group: "settings.validation", keywords: ["python", "校验"] },
  { id: "validation.go", labelKey: "settings.validationLangGo", page: "tools", group: "settings.validation", keywords: ["go", "golang", "校验"] },
  { id: "validation.java", labelKey: "settings.validationLangJava", page: "tools", group: "settings.validation", keywords: ["java", "jdtls", "校验"] },
  { id: "validation.dart", labelKey: "settings.validationLangDart", page: "tools", group: "settings.validation", keywords: ["dart", "flutter", "校验"] },
  { id: "validation.json", labelKey: "settings.validationLangJson", page: "tools", group: "settings.validation", keywords: ["json", "校验"] },
  // 预算组
  { id: "validation.lsp.sync_window_ms", labelKey: "settings.lspSyncWindow", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["sync", "诊断等待", "毫秒"] },
  { id: "validation.lsp.max_diagnostics", labelKey: "settings.lspMaxDiagnostics", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["diagnostics", "诊断条数"] },
  { id: "validation.lsp.max_chars", labelKey: "settings.lspMaxChars", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["chars", "诊断字符"] },
  { id: "validation.lsp.idle_ttl_ms", labelKey: "settings.lspIdleTtl", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["idle", "回收", "闲置"] },
  { id: "validation.lsp.max_servers", labelKey: "settings.lspMaxServers", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["server", "并发上限"] },
  { id: "validation.lsp.max_file_bytes", labelKey: "settings.lspMaxFileBytes", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["file", "体积", "字节", "跳过"] },
  { id: "validation.lsp.dedupe_limit", labelKey: "settings.lspDedupeLimit", page: "tools", group: "settings.lspBudget", advanced: true, keywords: ["dedupe", "重复回喂"] },
  // 发现组
  { id: "validation.lsp.extra_roots", labelKey: "settings.lspExtraRoots", page: "tools", group: "settings.lspDiscovery", keywords: ["sdk", "root", "目录", "发现"] },
  { id: "validation.lsp.java_home", labelKey: "settings.lspJavaHome", page: "tools", group: "settings.lspDiscovery", advanced: true, keywords: ["jdk", "java home", "java"] },
  // MCP 与技能
  { id: "mcp.servers", labelKey: "settings.mcp", page: "tools", group: "settings.mcp", keywords: ["mcp", "server", "服务器", "工具"] },
  { id: "disabled_skills", labelKey: "settings.skills", page: "tools", group: "settings.skills", keywords: ["skill", "技能", "启用", "禁用"] },

  // ---------- 工作区与智能体 ----------
  { id: "shell.selection", labelKey: "settings.shell", page: "agent", keywords: ["shell", "bash", "powershell", "终端"] },
  { id: "custom_prompt", labelKey: "settings.customPrompt", page: "agent", keywords: ["prompt", "提示词", "自定义"] },
  { id: "compact_threshold", labelKey: "settings.compactThreshold", page: "agent", keywords: ["compact", "压缩", "阈值", "上下文"] },
  { id: "compact_timeout_seconds", labelKey: "settings.compactTimeout", page: "agent", keywords: ["compact", "压缩", "超时"] },

  // ---------- 日志 ----------
  { id: "log.level", labelKey: "settings.logLevel", page: "logs", keywords: ["log", "日志", "级别", "debug"] },
  { id: "log.session_verbose", labelKey: "settings.sessionVerbose", page: "logs", advanced: true, keywords: ["log", "日志", "详细", "排障"] },

  // ---------- 关于 ----------
  { id: "ui.auto_update", labelKey: "settings.updates", page: "about", keywords: ["update", "更新", "自动检查"] },
  { id: "app.check_updates", labelKey: "settings.checkForUpdates", page: "about", keywords: ["update", "更新", "检查更新"] },
];

/** MCP 的字段路径（不在 config 内：独立 mcp.json，脏判定走组件内的文本基线 + 折叠比较） */
export const MCP_FIELD_ID = "mcp.servers";

/**
 * 即时生效项：改完立刻写 store / localStorage，**不参与脏标记**（否则永远显示「未保存」）。
 * 界面语言同时写 config.ui.language（镜像），但同样即时生效。
 */
export const INSTANT_APPLY_FIELD_IDS: string[] = [
  "ui.language", // 界面语言：useUi.setLanguage
  "ui.theme", // 主题：localStorage ws_theme
  "ui.font_sans", // 界面字体：localStorage
  "ui.font_mono", // 等宽字体：localStorage
  "ui.auto_update", // 启动时自动检查更新：localStorage ws_auto_update
];

/** 页拥有的配置字段路径（SettingFieldPath 联合类型把拼写错误挡在编译期） */
export type SettingFieldPath =
  | "ui.theme"
  | "ui.font_sans"
  | "ui.font_mono"
  | "ui.language"
  | "ui.auto_update"
  | "ui.ai_language"
  | "providers"
  | "active_model_id"
  | "network.proxy"
  | "network.allow_private_network"
  | "approval.enabled"
  | "approval.confirm_outside_create"
  | "approval.confirm_git_push"
  | "approval.auto_confirm"
  | "approval.command_allowlist"
  | "validation.typescript"
  | "validation.rust"
  | "validation.python"
  | "validation.go"
  | "validation.java"
  | "validation.dart"
  | "validation.json"
  | "validation.lsp.commands"
  | "validation.lsp.sync_window_ms"
  | "validation.lsp.max_diagnostics"
  | "validation.lsp.max_chars"
  | "validation.lsp.idle_ttl_ms"
  | "validation.lsp.max_servers"
  | "validation.lsp.max_file_bytes"
  | "validation.lsp.dedupe_limit"
  | "validation.lsp.extra_roots"
  | "validation.lsp.java_home"
  | "mcp.servers"
  | "disabled_skills"
  | "shell.selection"
  | "custom_prompt"
  | "compact_threshold"
  | "compact_timeout_seconds"
  | "log.level"
  | "log.session_verbose";

/**
 * 页拥有的配置字段路径（脏标记的唯一数据源，见 SettingsPage 的 pageSlice + fieldSlice）。
 * 断言它有三个作用：① 每页都有条目（防「漏一页 → 该页脏点永不亮」）；
 * ② 同一字段不得归属两页；③ 路径拼写受 SettingFieldPath 约束（写错即编译期报错）。
 * 即时生效项与 MCP 在此登记但**不参与比较**。
 */
export const PAGE_FIELDS: Record<PageKey, SettingFieldPath[]> = {
  // 界面页：主题与双字体槽是 localStorage 偏好、界面语言是即时生效项 ——
  // 本页没有可保存的改动，故**永不亮脏点**（有意为之，见 docs/settings-ia.md）。
  appearance: ["ui.theme", "ui.font_sans", "ui.font_mono", "ui.language"],
  providers: ["providers", "active_model_id", "ui.ai_language"],
  network: ["network.proxy", "network.allow_private_network"],
  security: [
    "approval.enabled",
    "approval.confirm_outside_create",
    "approval.confirm_git_push",
    "approval.auto_confirm",
    "approval.command_allowlist",
  ],
  tools: [
    "validation.typescript",
    "validation.rust",
    "validation.python",
    "validation.go",
    "validation.java",
    "validation.dart",
    "validation.json",
    "validation.lsp.commands",
    "validation.lsp.sync_window_ms",
    "validation.lsp.max_diagnostics",
    "validation.lsp.max_chars",
    "validation.lsp.idle_ttl_ms",
    "validation.lsp.max_servers",
    "validation.lsp.max_file_bytes",
    "validation.lsp.dedupe_limit",
    "validation.lsp.extra_roots",
    "validation.lsp.java_home",
    "mcp.servers",
    "disabled_skills",
  ],
  agent: ["shell.selection", "custom_prompt", "compact_threshold", "compact_timeout_seconds"],
  logs: ["log.level", "log.session_verbose"],
  // 关于页：只有「启动时自动检查更新」（localStorage 偏好，即时生效）+ 只读身份信息 —— 同样永不亮脏点
  about: ["ui.auto_update"],
};

/**
 * 页字段豁免清单：登记在 PAGE_FIELDS 里但**没有**独立设置项的路径（唯一一处，勿轻易扩容）。
 * 只有 `validation.lsp.commands`：它是六语言行共用的命令覆盖容器，每语言的设置项是行本身
 * （`validation.<lang>`，见 SETTINGS_ITEMS 的校验组）——再给容器登记一项会与「六语言各一行」的
 * 导航 / 搜索语义重复。契约测试反向断言：PAGE_FIELDS 里每条路径要么有对应项（且与项登记同页），
 * 要么在本清单内；清单里的路径若又有了设置项，同样报错（清单不得与 SETTINGS_ITEMS 重叠）。
 */
export const PAGE_FIELD_EXCEPTIONS: string[] = ["validation.lsp.commands"];

/**
 * 豁免清单：这些 settings.* 键**不是**可配置项，因此不进 SETTINGS_ITEMS。
 * 契约测试把「组件里出现的 t("settings.X")」闭合到
 * SETTINGS_ITEMS.labelKey ∪ 分组标题键 ∪ 页名键 ∪ 本清单；
 * 新增设置项必须登记，只新增文案必须在此显式分类（并说明为何不属可配置项）。
 */
export const SHELL_SETTING_KEYS: string[] = [
  // —— 页壳与离开拦截：全屏页容器自身的文案，与任何设置项无关 ——
  "title", // 设置页标题 / 全屏 dialog 的 aria-label
  "save", // 操作条「保存」
  "saved", // 保存成功提示
  "cancel", // 操作条「取消」
  "cancelHint", // 取消按钮 Tooltip
  "dirtyHint", // 脏圆点 Tooltip
  "instantApply", // 「即时生效」标注（挂在即时生效项旁）
  "backToWorkspace", // 左导航返回工作区
  "runningCount", // 运行中指示数量
  "runningHint", // 运行中指示 Tooltip
  "leaveTitle", // 三选拦截标题
  "leaveDesc", // 三选拦截说明
  "leaveSave", // 三选：保存并离开
  "leaveDiscard", // 三选：放弃改动
  "leaveStay", // 三选：留在原地

  // —— 界面页的从属文案（项已登记：ui.theme / ui.font_sans / ui.font_mono） ——
  "themeHint", // → ui.theme 的选择说明
  "themeSystem", // → ui.theme 选项
  "themeLight", // → ui.theme 选项
  "themeDark", // → ui.theme 选项
  "fontHint", // → ui.font_sans / ui.font_mono 的输入说明
  "fontReset", // → 字体槽的「恢复默认」按钮

  // —— 模型与供应商页的从属文案（项已登记：ui.ai_language） ——
  "aiLanguageHint", // → ui.ai_language 的输入说明

  // —— 供应商编辑器的表单字段与动作（本页主项：providers） ——
  "addProvider", // 动作：添加供应商
  "addProviderHint", // 弹框说明
  "editProvider", // 动作：编辑供应商
  "addModel", // 动作：添加模型
  "editModel", // 动作：编辑模型
  "remove", // 动作：删除条目
  "providerName", // 供应商表单字段（亦用于保存校验文案）
  "providerNamePh", // 同上，占位符
  "apiFormat", // 供应商表单字段
  "apiKeys", // 供应商表单字段
  "baseUrl", // 供应商表单字段（亦用于保存校验文案）
  "modelId", // 模型表单字段
  "maxTokens", // 模型表单字段
  "maxTokensHint", // 模型表单字段说明
  "contextWindow", // 模型表单字段
  "contextWindowHint", // 模型表单字段说明
  "reasoning", // 模型表单字段
  "inputTypes", // 模型表单字段
  "outputTypes", // 模型表单字段
  "typeText", // 输入/输出类型选项
  "typeImage", // 输入/输出类型选项
  "typeVideo", // 输入/输出类型选项
  "modelList", // 模型列表分组标题（亦用于保存校验文案）
  "modelsCount", // 模型数量徽标
  "noModels", // 空态
  "noProviders", // 空态
  "providerNeedsModel", // 空态引导
  "keyMaskedHint", // key 掩码说明
  "customHeaders", // 请求头分组标题（亦用于保存校验文案）
  "customHeadersHint", // 请求头说明
  "addHeader", // 动作：添加请求头

  // —— 保存校验文案（供应商 / 代理）：错误提示，不是设置项 ——
  "vRequired", // 必填
  "vBaseUrl", // URL 形态
  "vHeaders", // 请求头校验
  "vProblem", // 单条问题模板
  "vProblemSep", // 问题分隔符
  "vSaveBlocked", // 保存被拦前缀

  // —— 网络与连接的从属文案（项已登记：network.proxy / network.allow_private_network） ——
  "proxyNone", // → network.proxy 模式卡片
  "proxySystem", // → network.proxy 模式卡片
  "proxyManual", // → network.proxy 模式卡片
  "proxyNoneDesc", // → network.proxy 模式卡片说明
  "proxySystemDesc", // → network.proxy 模式卡片说明
  "proxyManualDesc", // → network.proxy 模式卡片说明
  "proxyDetected", // → network.proxy 的系统代理探测回显
  "proxyNotDetected", // → network.proxy 的系统代理未探测到
  "proxyUrl", // → network.proxy 的自定义地址子字段标签
  "proxyUrlHint", // → network.proxy 的自定义地址说明
  "proxyUrlInvalid", // → network.proxy 的保存校验文案

  // —— 安全与审批的从属文案（项已登记：approval.*） ——
  "autoConfirmHint", // → approval.auto_confirm 的说明
  "cmdAllowlistCwd", // → approval.command_allowlist 的悬浮目录标注

  // —— 工具与集成的从属文案（项已登记：validation.* / mcp.servers / disabled_skills） ——
  "validationHint", // → 校验组说明（探测结果 / 命令覆盖留空 = 自动探测）
  "lspCommandPh", // → 语言行命令覆盖输入框占位
  "lspFound", // → 状态徽标：已找到
  "lspFoundVersion", // → 状态徽标：已找到 vX
  "lspMissing", // → 状态徽标：未找到
  "lspDisabled", // → 状态徽标：已关闭
  "lspJavaCost", // → validation.java 的启用代价说明
  "lspExtraRootsHint", // → validation.lsp.extra_roots 的说明
  "lspAddRoot", // → 动作：添加额外 SDK 根目录
  "lspJavaHomeHint", // → validation.lsp.java_home 的说明
  "lspRedetect", // → 动作：重新探测
  "lspRedetected", // → 重新探测成功提示
  "lspRedetectFailed", // → 重新探测失败提示
  "mcpHint", // → mcp.servers 结构化编辑说明
  "mcpRawHint", // → mcp.servers 文本兜底模式说明
  "mcpName", // → mcp.servers 条目字段
  "mcpTransportStdio", // → mcp.servers 传输方式选项
  "mcpTransportHttp", // → mcp.servers 传输方式选项
  "mcpCommand", // → mcp.servers 条目字段
  "mcpArgs", // → mcp.servers 条目字段
  "mcpEnv", // → mcp.servers 条目字段
  "mcpUrl", // → mcp.servers 条目字段
  "mcpAdd", // → 动作：添加服务器
  "mcpSave", // → 动作：保存并重连（MCP 独立文件，不走页级保存）
  "skillsHint", // → disabled_skills 的目录来源说明
  "reloadSkills", // → 动作：重新加载技能
  "skillsReloaded", // → 重载成功提示
  "skillsReloadFailed", // → 重载失败提示
  "deleteSkill", // → 删除技能按钮 aria-label
  "deleteSkillConfirm", // → 删除技能确认文案
  "deleteSkillSuccess", // → 删除成功提示
  "deleteSkillFailed", // → 删除失败提示

  // —— 工作区与智能体的从属文案（项已登记：shell.selection） ——
  "shellHint", // → shell.selection 的说明
  "shellAuto", // → shell.selection 选项：自动（推荐）
  "shellAutoWithDefault", // → shell.selection 选项：自动（默认：X）
  "shellLimited", // → shell.selection 选项后缀：（有限支持）
  "shellDetectFailed", // → 探测失败警示
  "shellNotDetected", // → 所选 shell 已卸载警示
  "shellNoPath", // → 无固定可执行文件路径的占位文案

  // —— 日志的从属文案（项已登记：log.*） ——
  "logLevelHint", // → log.level 的说明
  "sessionVerboseHint", // → log.session_verbose 的说明

  // —— 关于的从属文案（项已登记：ui.auto_update / app.check_updates） ——
  "updatesHint", // → ui.auto_update 的说明
  "autoUpdateCheckbox", // → ui.auto_update 的开关内联标签
];
