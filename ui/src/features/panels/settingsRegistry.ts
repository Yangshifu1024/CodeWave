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

/** 设置页的 10 个分区（页 key = 组件内的页标识，也是 ui.settingsTab 的取值域）。
 *  「MCP」/「技能」自本批起从原「工具与集成」页拆成独立页：原页保留页 key `tools`（旧深链
 *  `showSettings("tools")` 仍直达），页面内容与页名改为「写入后检查与校验」。 */
export type PageKey =
  | "appearance"
  | "providers"
  | "network"
  | "security"
  | "mcp"
  | "skills"
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
  "mcp",
  "skills",
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
  // mcp / skills 与设置项同键：这两个页名就是该段分段的名称，复用可保证同一页名只有一个文案真源。
  mcp: "settings.mcp",
  skills: "settings.skills",
  // 页名键跟页 key 走（tools 页保留 key 以保住旧深链），故这里叫 pagePostWrite 而不是 pageTools
  tools: "settings.pagePostWrite",
  agent: "settings.pageAgent",
  logs: "settings.pageLogs",
  about: "settings.pageAbout",
};

/** 左导航三组（组内页序 = PAGE_ORDER 中的相对序；titleKey 是组标题的 i18n 键） */
export const PAGE_GROUPS: { titleKey: string; pages: PageKey[] }[] = [
  { titleKey: "settings.groupUiModel", pages: ["appearance", "providers", "network"] },
  { titleKey: "settings.groupSafetyTools", pages: ["security", "mcp", "skills", "tools", "agent"] },
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
  mcp: "mcp", // 旧「MCP」原是工具与集成页里的一段；现独立成页，深链直达（保留自映射与上方几条同风格）
  skills: "skills", // 同上一行：旧「技能」段
  tools: "tools", // 「工具与集成」页仍在（改名不改 key），旧深链直达写入后检查与校验页
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
 * - `advanced`：进阶项（批③ 的页级开关 + 行内过滤消费它）；
 * - `width`：控件宽度档（窄 180 / 中 240 / 宽 360）——**不参与档位的项必须登记进
 *   `WIDTH_EXEMPT_ITEM_IDS`**，让「每项要么标注 width、要么显式豁免」可被测试断言；
 * - `keywords`：直字符串搜索词（**不进 i18n**：中英混排的常见说法，批③ 搜索用）。
 */
export interface SettingItem {
  id: string;
  labelKey: string;
  page: PageKey;
  group?: string;
  advanced?: boolean;
  width?: SettingWidth;
  keywords?: string[];
}

/** 控件宽度三档（窄 180 / 中 240 / 宽 360；类名见 WIDTH_CLASS，都带 max-width:100% 防窄窗横向溢出） */
export type SettingWidth = "narrow" | "mid" | "wide";

/** 全部设置项（登记制：改动设置项归属时必须同步本表与 PAGE_FIELDS） */
export const SETTINGS_ITEMS: SettingItem[] = [
  // ---------- 界面 ----------
  { id: "ui.theme", labelKey: "settings.theme", page: "appearance", width: "narrow", keywords: ["theme", "dark", "light", "主题", "暗色", "亮色"] },
  { id: "ui.font_sans", labelKey: "settings.uiFont", page: "appearance", keywords: ["font", "sans", "字体", "界面字体"] },
  { id: "ui.font_mono", labelKey: "settings.monoFont", page: "appearance", keywords: ["font", "mono", "等宽", "代码字体"] },
  { id: "ui.language", labelKey: "settings.language", page: "appearance", width: "narrow", keywords: ["language", "语言", "界面语言"] },

  // ---------- 模型与供应商 ----------
  { id: "providers", labelKey: "settings.providers", page: "providers", keywords: ["provider", "供应商", "供应商配置", "模型", "模型配置", "api", "base url", "key"] },
  { id: "active_model_id", labelKey: "settings.active", page: "providers", keywords: ["active", "当前", "活跃模型", "current model", "默认模型"] },
  { id: "ui.ai_language", labelKey: "settings.aiLanguage", page: "providers", width: "mid", keywords: ["ai", "language", "回复语言", "ai 语言"] },

  // ---------- 网络与连接 ----------
  { id: "network.proxy", labelKey: "settings.proxyMode", page: "network", width: "wide", keywords: ["proxy", "代理", "socks", "http"] },
  { id: "network.allow_private_network", labelKey: "settings.allowPrivate", page: "network", keywords: ["private", "内网", "局域网", "本地模型"] },

  // ---------- 安全与审批 ----------
  { id: "approval.enabled", labelKey: "settings.approvalEnabled", page: "security", keywords: ["approval", "确认", "危险命令", "弹窗", "审批"] },
  { id: "approval.confirm_outside_create", labelKey: "settings.confirmOutside", page: "security", keywords: ["workspace", "工作区", "新建路径"] },
  { id: "approval.confirm_git_push", labelKey: "settings.confirmPush", page: "security", keywords: ["git", "push", "确认"] },
  { id: "approval.auto_confirm", labelKey: "settings.autoConfirm", page: "security", keywords: ["auto", "自动确认", "超时", "5 分钟"] },
  { id: "approval.command_allowlist", labelKey: "settings.cmdAllowlist", page: "security", advanced: true, keywords: ["allowlist", "白名单", "允许", "命令"] },

  // ---------- 写入后检查与校验（原「工具与集成」页：MCP / 技能已拆成独立页） ----------
  // 写入后检查命令（[docs/post-write-check-plan](../../../../docs/post-write-check-plan.md)）：取代 LSP 写后语义校验
  { id: "post_write_check.enabled", labelKey: "settings.postWriteEnabled", page: "tools", group: "settings.postWriteCheck", keywords: ["post", "write", "check", "lint", "写入后检查", "检查", "校验"] },
  { id: "post_write_check.command", labelKey: "settings.postWriteCommand", page: "tools", group: "settings.postWriteCheck", keywords: ["command", "命令", "lint", "eslint", "tsc", "ruff", "cargo", "{file}", "写入后检查", "检查命令"] },
  { id: "post_write_check.timeout_seconds", labelKey: "settings.postWriteTimeout", page: "tools", group: "settings.postWriteCheck", width: "narrow", advanced: true, keywords: ["timeout", "超时", "秒"] },
  { id: "post_write_check.tail_chars", labelKey: "settings.postWriteTailChars", page: "tools", group: "settings.postWriteCheck", width: "narrow", advanced: true, keywords: ["tail", "输出", "字符", "尾部", "截断"] },
  // ---------- MCP（独立页：状态表 + 服务器配置） ----------
  // 服务器状态是只读信息与动作类项（`app.*` 前缀：无落盘字段、不进 PAGE_FIELDS，只提供搜索 / 锚点），
  // 与 app.cleanup_status 同类；数据源是既有的 mcp_status 命令与 mcp:status 事件。
  { id: "app.mcp_status", labelKey: "settings.mcpStatus", page: "mcp", keywords: ["mcp", "status", "连接", "状态", "服务器状态", "连接状态", "工具数"] },
  // 「工具与集成」是这两个设置项原来的页名，留作关键词：老用户（含看过旧文档的人）搜旧页名仍能命中
  { id: "mcp.servers", labelKey: "settings.mcp", page: "mcp", width: "narrow", keywords: ["mcp", "mcp server", "mcp 服务器", "server", "服务器", "工具", "工具与集成", "tools & integrations"] },

  // ---------- 技能（独立页） ----------
  { id: "disabled_skills", labelKey: "settings.skills", page: "skills", keywords: ["skill", "skills", "技能", "启用", "禁用", "禁用技能", "工具与集成", "tools & integrations"] },

  // ---------- 工作区与智能体 ----------
  { id: "shell.selection", labelKey: "settings.shell", page: "agent", width: "mid", keywords: ["shell", "bash", "powershell", "终端"] },
  { id: "custom_prompt", labelKey: "settings.customPrompt", page: "agent", keywords: ["prompt", "system prompt", "提示词", "系统提示词", "自定义"] },
  { id: "compact_threshold", labelKey: "settings.compactThreshold", page: "agent", keywords: ["compact", "压缩", "自动压缩", "阈值", "上下文"] },
  { id: "compact_timeout_seconds", labelKey: "settings.compactTimeout", page: "agent", width: "narrow", keywords: ["compact", "压缩", "超时"] },
  // 会话保留期与清理（[docs/session-cleanup](../../../../docs/session-cleanup.md)）：下拉是**页级保存**字段；
  // 后面的只读状态行与动作按钮用 `app.*` 前缀（无落盘字段，不进 PAGE_FIELDS，只提供搜索 / 锚点）
  { id: "sessions.retention_days", labelKey: "settings.sessionRetention", page: "agent", width: "narrow", keywords: ["session", "retention", "cleanup", "会话保留期", "保留期", "清理", "自动清理", "删除会话"] },
  { id: "app.cleanup_now", labelKey: "settings.cleanupNow", page: "agent", keywords: ["cleanup", "清理", "立即清理", "clean up now", "删除会话"] },
  { id: "app.cleanup_status", labelKey: "settings.cleanupStatus", page: "agent", keywords: ["cleanup", "清理", "上次清理", "last cleanup", "清理记录"] },

  // ---------- 日志 ----------
  { id: "log.level", labelKey: "settings.logLevel", page: "logs", width: "narrow", keywords: ["log", "日志", "级别", "debug"] },
  { id: "log.session_verbose", labelKey: "settings.sessionVerbose", page: "logs", advanced: true, keywords: ["log", "verbose", "日志", "详细", "排障"] },

  // ---------- 关于 ----------
  // 只读身份 / 目录入口 / 许可证（批④ 登记）：app.* 前缀 = 无落盘字段（与 app.check_updates 同形），
  // 登记前批③ 搜索在关于页只能命中 2 项；锚点在各行的锚点容器上（settings.page.test.tsx 锚点覆盖用例）
  { id: "app.version", labelKey: "settings.aboutVersion", page: "about", keywords: ["version", "版本", "版本号", "app version"] },
  { id: "app.data_dir", labelKey: "settings.aboutAppData", page: "about", keywords: ["data", "data dir", "数据目录", "配置目录", ".codewave"] },
  { id: "app.logs_dir", labelKey: "settings.aboutLogsDir", page: "about", keywords: ["log", "logs", "日志", "日志目录", "诊断日志"] },
  { id: "app.repo", labelKey: "settings.aboutRepo", page: "about", keywords: ["repo", "repository", "github", "仓库", "代码仓库", "源码"] },
  { id: "app.license", labelKey: "settings.aboutLicense", page: "about", keywords: ["license", "mit", "许可", "许可证", "开源"] },
  { id: "ui.auto_update", labelKey: "settings.updates", page: "about", keywords: ["update", "auto update", "更新", "自动更新", "自动检查"] },
  { id: "app.check_updates", labelKey: "settings.checkForUpdates", page: "about", keywords: ["update", "更新", "检查更新"] },
];

/**
 * 控件宽度三档的类名（定义于 `ui/src/theme/app.css`；三档都带 `max-width:100%`——窄窗下
 * 宽档被 max-width 兜住，不横向溢出）。页体内**不得**再写像素内联 `width`，改挂这三个类。
 */
export const WIDTH_CLASS: Record<SettingWidth, string> = {
  narrow: "w-narrow",
  mid: "w-mid",
  wide: "w-wide",
};

/** 三档档位清单（契约测试遍历用） */
export const WIDTH_TIERS: SettingWidth[] = ["narrow", "mid", "wide"];

/**
 * 宽度档豁免清单：这些设置项**不参与**三档，故不标 `width`。契约测试双向断言：
 * ① 每项要么有合法 `width`、要么在本清单内；② 清单与「已标注 width 的项」不得重叠
 * （重叠 = 清单在掩盖过时登记）。扩容必须写清「为什么它没有宽度档」。
 * 分类：整行 / 多行文本、整行开关、Slider、行内网格控件、整行列表与复合容器 / 动作项。
 */
export const WIDTH_EXEMPT_ITEM_IDS: string[] = [
  // —— 整行 / 多行文本：内容长度不可预知，占满整行（宽度由容器决定） ——
  "ui.font_sans", // 界面字体名（可含多个 family，逗号分隔）+ 行内「恢复默认」按钮
  "ui.font_mono", // 等宽字体名，同上
  "custom_prompt", // TextArea：多行自定义提示词

  // —— 整行开关 / 输入：Switch / 整行命令输入 ——
  "network.allow_private_network", // Switch
  "approval.enabled", // Switch
  "approval.confirm_outside_create", // Switch
  "approval.confirm_git_push", // Switch
  "approval.auto_confirm", // Switch
  "log.session_verbose", // Switch
  "ui.auto_update", // Switch（关于页更新行内的内联开关）
  "post_write_check.enabled", // Switch（写入后检查总开关）
  "post_write_check.command", // 整行命令输入（含 {file} 占位符；说明文字自带）

  // —— Slider：整行滑动条，宽度随容器（无内联宽度） ——
  "compact_threshold", // Slider：自动压缩阈值

  // —— 整行列表 / 复合容器 / 动作与只读标记 ——
  "approval.command_allowlist", // 整行命令列表（每行一条 + 删除按钮）
  "disabled_skills", // 整行技能行（名称 / 来源 / 开关 / 删除）
  "providers", // 供应商列表/新增/编辑三视图复合容器：宽度由 maxWidth 420/560/640 定（本批明确非目标）
  "active_model_id", // 无独立控件：模型列表里的「当前」标记，由删除/首个模型回卷决定
  "app.check_updates", // 动作按钮（检查更新），宽度随文案
  "app.cleanup_now", // 动作按钮（立即清理），宽度随文案；禁用原因说明跟在按钮后，整行不设档
  "app.cleanup_status", // 整行只读信息项：标签 + extra 说明 + 「时间 · 删除条数」回显，无独立控件
  "app.mcp_status", // 整行只读状态表：服务器名 / 状态 / 工具数三列（列宽由 app.css 网格定，无控件宽度档）

  // —— 关于页的只读信息与目录 / 许可证入口（批④）：整行「标签 + extra 说明 + 值/按钮」，无独立控件宽度 ——
  "app.version", // 只读版本串（懒加载）
  "app.data_dir", // 整行：说明（extra）+ 「打开数据目录」按钮
  "app.logs_dir", // 整行：说明（extra）+ 「打开日志目录」按钮
  "app.repo", // 整行：说明（extra）+ 「打开代码仓库」按钮
  "app.license", // 整行：说明（extra）+ 「查看许可证」按钮
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
  "ui.font_sans", // 界面字体：localStorage 缓存 + 后端 config.ui.font_sans（set_font_prefs）
  "ui.font_mono", // 等宽字体：同上
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
  | "post_write_check.enabled"
  | "post_write_check.command"
  | "post_write_check.timeout_seconds"
  | "post_write_check.tail_chars"
  | "mcp.servers"
  | "disabled_skills"
  | "shell.selection"
  | "custom_prompt"
  | "compact_threshold"
  | "compact_timeout_seconds"
  | "sessions.retention_days"
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
    "post_write_check.enabled",
    "post_write_check.command",
    "post_write_check.timeout_seconds",
    "post_write_check.tail_chars",
  ],
  // MCP / 技能各自成页：字段归属随页面迁移（字段归属唯一断言要求只有一页拥有）
  mcp: ["mcp.servers"],
  skills: ["disabled_skills"],
  agent: ["shell.selection", "custom_prompt", "compact_threshold", "compact_timeout_seconds", "sessions.retention_days"],
  logs: ["log.level", "log.session_verbose"],
  // 关于页：只有「启动时自动检查更新」（localStorage 偏好，即时生效）+ 只读身份信息 —— 同样永不亮脏点
  about: ["ui.auto_update"],
};

/**
 * 页字段豁免清单：登记在 PAGE_FIELDS 里但**没有**独立设置项的路径（唯一一处，勿轻易扩容）。
 * 当前为空——写入后检查的四项各自成项，无共用容器。
 */
export const PAGE_FIELD_EXCEPTIONS: string[] = [];

/**
 * 豁免清单：这些 settings.* 键**不是**可配置项，因此不进 SETTINGS_ITEMS。
 * 契约测试把「组件里出现的 t("settings.X")」闭合到
 * SETTINGS_ITEMS.labelKey ∪ 分组标题键 ∪ 页名键 ∪ 本清单；
 * 新增设置项必须登记，只新增文案必须在此显式分类（并说明为何不属可配置项）。
 */
export const SHELL_SETTING_KEYS: string[] = [
  // —— 页壳与离开拦截：全屏页容器自身的文案，与任何设置项无关 ——
  // 保存 / 已保存 / 取消 / 删除已收进 `common` 段（批④；common.* 不是 settings.*，
  // 不进本清单也不进注册表，见 docs/settings-terminology.md）
  "title", // 设置页标题 / 全屏 dialog 的 aria-label
  "cancelHint", // 取消按钮 Tooltip（→ 对应按钮文案 = common.cancel）
  "dirtyHint", // 脏圆点 Tooltip
  "instantApplySuffix", // 「即时生效」的括号后缀（唯一形态：跟在项标题后，如「更新（即时生效）」）
  "backToWorkspace", // 左导航返回工作区
  "runningCount", // 运行中指示数量
  "runningHint", // 运行中指示 Tooltip
  "leaveTitle", // 三选拦截标题
  "leaveDesc", // 三选拦截说明
  "leaveSave", // 三选：保存并离开
  "leaveDiscard", // 三选：放弃改动
  "leaveStay", // 三选：留在原地

  // —— 搜索与进阶折叠的页壳文案（非可配置项；项由 SETTINGS_ITEMS 提供、开关状态存 localStorage） ——
  "searchPlaceholder", // 搜索框占位符
  "searchResults", // 结果列表的 aria-label（role=listbox）
  "searchEmpty", // 无命中空态
  "searchEmptyHint", // 无命中引导（动态条目在各自页面内查找）
  "showAdvanced", // 页级开关文案（含 {{n}} 计数）
  "advancedHint", // 进阶折叠说明（偏好跨页跨会话）

  // —— 界面页的从属文案（项已登记：ui.theme / ui.font_sans / ui.font_mono） ——
  "themeHint", // → ui.theme 的选择说明
  "themeSystem", // → ui.theme 选项
  "themeLight", // → ui.theme 选项
  "themeDark", // → ui.theme 选项
  "fontHint", // → ui.font_sans / ui.font_mono 的输入说明
  "fontReset", // → 字体槽的「恢复默认」按钮

  // —— 模型与供应商页的从属文案（项已登记：ui.ai_language） ——
  "aiLanguageHint", // → ui.ai_language 的输入说明
  "aiLanguagePlaceholder", // → ui.ai_language 的占位符（批④ 拆出，不再借用 composer.effortDefault）

  // —— 供应商编辑器的表单字段与动作（本页主项：providers） ——
  "addProvider", // 动作：添加供应商
  "addProviderHint", // 弹框说明
  "editProvider", // 动作：编辑供应商
  "addModel", // 动作：添加模型
  "editModel", // 动作：编辑模型
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

  // —— 写入后检查与校验 / MCP / 技能 三页的从属文案（项已登记：post_write_check.* /
  //    mcp.servers / disabled_skills / app.mcp_status） ——
  "postWriteHint", // → 写入后检查组说明（在项目根目录执行 / 输出交给模型）
  "postWriteCommandHint", // → post_write_check.command 的说明（{file} 占位符含义 + 各技术栈示例）
  "postWriteCommandPh", // → post_write_check.command 输入框占位
  "mcpConfigHead", // → MCP 页分段小标题：服务器配置（页名已由 PageKey 承担，段标题走轻量小标题）
  "mcpColState", // → 状态表列头：状态
  "mcpColTools", // → 状态表列头：工具数（该服务器暴露的工具个数）
  "mcpStateReady", // → 状态值：已连接
  "mcpStateStarting", // → 状态值：连接中（30s 初始化窗口内，来自轮询）
  "mcpStateError", // → 状态值：连接失败（详情按行展开）
  "mcpStateDisconnected", // → 状态值：未连接（当前没有会话驱动连接）
  "mcpStatusRefresh", // → 刷新按钮 Tooltip：只重读状态，不会重连
  "mcpStatusToggleError", // → 失败行的展开 / 收起错误详情 aria-label
  "mcpStatusHint", // → 状态表下方的全局语义说明（连接由打开会话驱动）
  "mcpStatusRefreshFailed", // → 刷新失败提示（保留旧值，不把一次抖动伪装成「未连接」）
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
  "mcpExtraKeys", // → mcp.servers 条目：表单未展示键的「保存时原样保留」提示
  "mcpIssuesHead", // → mcp.servers：保存前结构校验的问题清单标题
  "mcpSaveBlocked", // → 动作：error 级校验阻止保存
  "mcpScopeGlobal", // → mcp 页：配置作用域切换（全局）
  "mcpScopeProject", // → mcp 页：配置作用域切换（项目）
  "mcpScopeNoProject", // → mcp 页：无项目目录时只读全局配置的提示
  "mcpScopeDirtyHint", // → mcp 页：有未保存改动时禁止切换作用域的说明
  "mcpConfigPath", // → mcp 页：当前作用域 mcp.json 路径回显
  "mcpTest", // → 动作：单 server 临时测试连接
  "mcpTestOk", // → mcp 页：测试成功结果
  "mcpTestFail", // → mcp 页：测试失败结果
  "mcpTestNoReply", // → mcp 页：测试无响应兜底文案
  "skillsHint", // → disabled_skills 的目录来源说明
  "skillsEmpty", // → disabled_skills 的空态（批④ 修缺陷：不再借用 sessions.empty）
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

  // —— 会话保留期与清理的从属文案（项已登记：sessions.retention_days / app.cleanup_now / app.cleanup_status）
  //    （[docs/session-cleanup](../../../../docs/session-cleanup.md) §3 第 12/25/26/27 条） ——
  "sessionRetentionHint", // → sessions.retention_days 的说明
  "cleanupNever", // → sessions.retention_days 选项：不清理（= null）
  "cleanupDays", // → sessions.retention_days 选项：N 天（带 {{n}}）
  "cleanupNeedRetention", // → 保留期为「不清理」时的按钮禁用原因（先选择保留期）
  "cleanupUnsavedFirst", // → 保留期有未保存改动时的按钮禁用原因（先保存）
  "cleanupNonePending", // → 预览 0 条时的轻提示（不弹确认框）
  "cleanupPreviewFailed", // → 预览失败提示（本次不清理，配置照常保存）
  "cleanupConfirmTitle", // → 手动「立即清理」的确认框标题
  "cleanupSaveConfirmTitle", // → 保存前清理的确认框标题
  "cleanupConfirmDesc", // → 确认框说明（删除条数，带 {{n}}）
  "cleanupOrphanDesc", // → 只删索引外残留文件（会话一条不删）时的确认框说明（带 {{n}}）
  "cleanupConfirmListTitle", // → 确认框里的会话标题清单标题
  "cleanupConfirmOk", // → 确认框的确认按钮（清理）
  "cleanupSaveSkip", // → 保存前确认框的取消按钮（写明：仅本次跳过，下次启动仍会清理）
  "cleanupSavedSkipped", // → 取消清理后的保存提示
  "cleanupDone", // → 清理完成提示（已清理 N 个会话，关闭 M 个标签页）
  "cleanupNeverRun", // → app.cleanup_status 的「还没有清理记录」空态
  "cleanupLastRun", // → app.cleanup_status 的回显（时间 + 删除条数）
  "cleanupLastFailed", // → app.cleanup_status 的失败条数补充（带 {{n}}）
  "cleanupFailed", // → 清理后「N 个会话未能清理」的警示（手动 / 保存两条路径共用）
  "cleanupOrphanExtra", // → 会话与残留数据文件同时要删时，确认框里补的一句残留条数
  "cleanupDoneOrphans", // → 同一情形的完成提示后缀（另有 N 个残留数据文件）

  // —— 日志的从属文案（项已登记：log.*） ——
  "logLevelHint", // → log.level 的说明
  "sessionVerboseHint", // → log.session_verbose 的说明

  // —— 关于的从属文案（项已登记：ui.auto_update / app.check_updates / app.version / app.data_dir /
  // app.logs_dir / app.repo / app.license） ——
  "updatesHint", // → ui.auto_update 的说明
  "autoUpdateCheckbox", // → ui.auto_update 的开关内联标签
  "aboutVersionHint", // → app.version 的说明
  "aboutSlogan", // → 身份块的一句简介（非设置项，无锚点）
  "aboutAppDataHint", // → app.data_dir 的说明
  "aboutOpenAppData", // → app.data_dir 的动作按钮
  "aboutLogsDirHint", // → app.logs_dir 的说明
  "aboutOpenLogsDir", // → app.logs_dir 的动作按钮
  "aboutRepoHint", // → app.repo 的说明
  "aboutOpenRepo", // → app.repo 的动作按钮
  "aboutLicenseHint", // → app.license 的说明
  "aboutViewLicense", // → app.license 的动作按钮
];

/**
 * 进阶项偏好（localStorage，全局单一偏好、默认收起、跨页跨会话记忆）：
 * 存 "1" = 展开、"0" / 缺省 = 收起。页级开关只影响显示，**不参与脏标记**（PAGE_FIELDS 语义不变）。
 */
export const SETTINGS_ADVANCED_PREF_KEY = "ws_settings_show_advanced";

/** 进阶项 id（`advanced: true` 的全部项；页级开关的计数与行内过滤都从这里派生） */
export const ADVANCED_ITEM_IDS: string[] = SETTINGS_ITEMS.filter((i) => i.advanced).map((i) => i.id);

/** 该页进阶项数量（页级开关文案「显示进阶项（N）」的计数；N = 0 时该行不渲染） */
export function advancedCountByPage(page: PageKey): number {
  return SETTINGS_ITEMS.filter((i) => i.page === page && i.advanced).length;
}

/**
 * 该页该组是否**整组皆为进阶项**（是 → 收起时整组隐藏，而不是逐行隐藏）。
 * 组不存在 / 页不匹配 / 组内有非进阶项 → false（逐行判断由 ADVANCED_ITEM_IDS 承担）。
 */
export function isAdvancedOnlyGroup(page: PageKey, group: string): boolean {
  const items = SETTINGS_ITEMS.filter((i) => i.page === page && i.group === group);
  return items.length > 0 && items.every((i) => i.advanced);
}

/** 项序（注册表原序）：结果排序的最后一级，保证稳定输出 */
const ITEM_RANK = new Map<string, number>(SETTINGS_ITEMS.map((item, idx) => [item.id, idx]));
/** 组序（注册表内首次出现顺序）：同页内组按页体渲染顺序登记，故可直接当排序键 */
const GROUP_RANK = new Map<string, number>();
for (const item of SETTINGS_ITEMS) {
  if (item.group && !GROUP_RANK.has(item.group)) GROUP_RANK.set(item.group, GROUP_RANK.size);
}

/** 搜索结果的稳定排序：页序（PAGE_ORDER） → 组序 → 注册表原序 */
function compareHits(a: SettingItem, b: SettingItem): number {
  const byPage = PAGE_ORDER.indexOf(a.page) - PAGE_ORDER.indexOf(b.page);
  if (byPage !== 0) return byPage;
  const groupOf = (item: SettingItem) => (item.group ? GROUP_RANK.get(item.group)! + 1 : 0);
  const byGroup = groupOf(a) - groupOf(b);
  if (byGroup !== 0) return byGroup;
  return ITEM_RANK.get(a.id)! - ITEM_RANK.get(b.id)!;
}

/**
 * 设置项搜索：命中范围 = 该项 i18n 显示名 + `keywords`（直字符串，中英混排）+ 所属页名 + 所属组名。
 * 规则：query 先小写归一 + trim + 按空白切分为多词，**词之间 AND**（每个词都要命中同一项的命中范围）；
 * 不做拼音 / 首字母 / 模糊 / 权重 / 词内高亮（非目标）。
 * 输出按「页序 → 组序 → 注册表原序」稳定排序；空串 / 仅空白返回 `[]`（调用方据此回到常规导航）。
 */
export function matchSettings(query: string, t: (key: string) => string): SettingItem[] {
  const terms = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  if (terms.length === 0) return [];
  return SETTINGS_ITEMS.filter((item) => {
    const haystack = [
      t(item.labelKey),
      ...(item.keywords ?? []),
      t(PAGE_LABEL_KEY[item.page]),
      item.group ? t(item.group) : "",
    ]
      .join(" ")
      .toLowerCase();
    return terms.every((term) => haystack.includes(term));
  }).sort(compareHits);
}
