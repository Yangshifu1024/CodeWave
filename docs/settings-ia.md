# 设置页重整 · 8 页重划与设置项注册表

> 状态：已实施（批②）。批① 把设置从弹窗改为常驻全屏页（[settings-fullscreen-shell](./settings-fullscreen-shell.md)）；
> 本批把 7 页按「用户找东西的顺序」重划为 8 页 + 3 组导航，并把「哪一项住在哪一页」这件事
> 从组件 JSX 里抽成可被测试断言的数据（设置项注册表）。术语统一留批④。
>
> **后续变更（2026-09-20，[post-write-check-plan](./post-write-check-plan.md)）**：工具与集成页的
> 「写入后语义校验」（六语言 LSP 开关 + 命令覆盖 + 预算 / 发现共 19 项）已整体删除，改为
> **写入后检查**一项命令（`post_write_check.enabled / command / timeout_seconds / tail_chars` 四项）。
> 本文中凡涉及 `validation.*` / `validation.lsp.*` / LSP 行的描述均为历史记录，现行实现见上述方案文档。

## 1. 页集合与 3 组导航

| 组 | 页（key） | 页名 | 内容 |
|---|---|---|---|
| 外观与模型 | `appearance` | 界面 | 主题、界面字体 / 等宽字体双槽 + 预览、界面语言 |
| 外观与模型 | `providers` | 模型与供应商 | AI 回复语言、供应商列表 / 新增 / 编辑（含活跃模型） |
| 外观与模型 | `network` | 网络与连接 | 代理模式与地址、允许访问内网地址 |
| 安全与能力 | `security` | 安全与审批 | 危险命令确认、工作区外新建路径确认、git push 前确认、5 分钟自动确认、命令白名单 |
| 安全与能力 | `tools` | 工具与集成 | 写入后检查（[post-write-check-plan](./post-write-check-plan.md)：一条检查命令 + 开关 / 超时 / 输出尾部字符数）、MCP 服务器、技能 |
| 安全与能力 | `agent` | 工作区与智能体 | Shell、自定义提示词、自动压缩阈值、压缩请求超时 |
| 诊断与其他 | `logs` | 日志 | 日志级别、会话详细日志 |
| 诊断与其他 | `about` | 关于 | logo / 名称 / 简介、版本、数据目录、日志目录、代码仓库、开源许可证、自动更新开关、手动检查更新 |

- 默认页 `appearance`；`ui.settingsTab` 的类型是 `PageKey`（不再是 `string`）。
- 左导航自建（替掉 antd `Tabs`）：组标题（复用 `.nav-section-title` 度量：11px dim）+ 页行
  （`.settings-nav-item[data-page]`，选中态仅换背景、脏圆点 `.settings-dirty-dot`），
  键盘可达（行是 `<button role="tab">`，打开设置时焦点仍在返回工作区按钮上，批① 行为不变）。
- 导航语义（批② 审查返工补全）：`role="tablist"` 的直接子节点除 tab 外只有标了 `role="presentation"` 的
  组标题；每个页行 `aria-controls="settings-panel"`，页体 `id="settings-panel" role="tabpanel"` 且
  `aria-labelledby` 指向当前页行；↑/↓（兼认 ←/→）在页行间**移动焦点**，Enter/Space 或点击才激活
  （手动激活模式）——不动 `tabIndex`，Tab 键可达与批① 的「打开设置即聚焦返回按钮」都不变。
- 非法 `settingsTab`（绕过 `setSettingsTab` 直写 store 的旧状态 / 深链）在渲染处经 `normalizePageKey`
  收口：否则 `aria-selected` 与 active 类全 false、页体回退首页而导航一片无高亮。
- 工作区让位语义（`.workspace-covered` 只藏可见性、不卸载、不 `display:none`）、Esc 链、三选拦截、
  运行中指示全部保持批① 的实现，本批未动。

## 2. 逐项归属表（新 → 旧）

| 新页 | 项（config 字段路径 / 本地偏好） | 旧页 |
|---|---|---|
| 界面 | `ui.theme`、`ui.font_sans`、`ui.font_mono`（localStorage）、`ui.language` | 外观 + 通用·界面语言 |
| 模型与供应商 | `ui.ai_language`、`providers`、`active_model_id` | 通用·AI 语言 + 供应商 |
| 网络与连接 | `network.proxy`（= config.proxy）、`network.allow_private_network` | 网络 + 安全·内网访问 |
| 安全与审批 | `approval.enabled / confirm_outside_create / confirm_git_push / auto_confirm / command_allowlist` | 安全（迁出内网访问与语义校验） |
| 工具与集成 | `post_write_check.enabled / command / timeout_seconds / tail_chars`、`mcp.servers`（独立 mcp.json）、`disabled_skills` | 安全·写入后检查 + MCP + 技能 |
| 工作区与智能体 | `shell.selection`、`custom_prompt`、`compact_threshold`、`compact_timeout_seconds` | 通用（这 4 项） |
| 日志 | `log.level`、`log.session_verbose` | 通用（这 2 项） |
| 关于 | `ui.auto_update`（localStorage）、`app.check_updates`（动作）；批④ 追加：`app.version` / `app.data_dir` / `app.logs_dir` / `app.repo` / `app.license`（只读信息与入口，均为 `app.*` 无落盘字段） | AboutModal 全部内容 + 通用·自动更新开关 |

设置项的行为语义（字段名 / 默认值 / 保存时机 / 校验规则）**一字未改**；关于页只是把弹框内容搬进页面。

## 3. 注册表：`ui/src/features/panels/settingsRegistry.ts`

纯数据模块（无 React、无 store 依赖），导出：

- `PageKey` / `PAGE_ORDER` / `DEFAULT_PAGE` / `PAGE_LABEL_KEY` / `PAGE_GROUPS`；
- `LEGACY_PAGE_ALIASES` + `normalizePageKey`（旧页 key 归一，未知 / 空 → 默认页）；
- `SETTINGS_ITEMS: SettingItem[]`（`{ id, labelKey, page, group?, advanced?, keywords? }`，
  `keywords` 是**直字符串**、不进 i18n，供批③ 搜索用；`advanced` 供批③ 折叠用，本批只登记不渲染）；
- **`PAGE_FIELDS: Record<PageKey, SettingFieldPath[]>`**：每页拥有的配置字段路径（脏标记的数据源）；
- `MCP_FIELD_ID`、`INSTANT_APPLY_FIELD_IDS`、`SHELL_SETTING_KEYS`（豁免清单）、
  `PAGE_FIELD_EXCEPTIONS`（页字段里没有独立设置项的路径；写入后检查四项各自成项后该清单为空）。

### 新增一项设置的登记流程

1. 在 `SETTINGS_ITEMS` 加一条（`id` = config 字段路径 / `ui.*` 本地偏好 / `app.*` 动作，全表唯一；
   `labelKey` 指 i18n 项名键；`page` 归属页；`group` 仅在有分组的页上写）。
2. 若它是**可保存的**配置项，把字段路径加进 `PAGE_FIELDS[page]`，并在 `SettingsPage.tsx` 的
   `fieldSlice()` 里补缺省折叠（若该字段有 serde default / null 折默认对象语义）；
   即时生效项（localStorage / 立即生效）加进 `INSTANT_APPLY_FIELD_IDS`。
3. 双侧 i18n 加键（zh-CN + en-US，`i18n.keys.test.ts` 守护）。
4. 跑 `pnpm --dir ui test`：`settings.registry.test.ts` 会拦住漏登记
   （引用闭包（扫 `features/**/*.tsx`：panels 目录看是否属于 `settings.*` / `common.*`，其余目录看「文件 → 允许段」白名单；
   单/双/反引号三种写法都认，变量拼出的键名另需登记）/ 键存在性 / 页合法 / 分组完备 /
   PAGE_FIELDS 覆盖 / 字段归属唯一 / **「项 ↔ 页字段」双向闭合** / 豁免清单不重叠）。
   双向闭合的语义：① 每条设置项的 `id` 必须出现在 `PAGE_FIELDS[item.page]` 里（`app.*` 动作 / 只读信息项
   除外——它们本就没有落盘字段）；② `PAGE_FIELDS` 里每条路径必须有同页的设置项，例外只能写进
   `PAGE_FIELD_EXCEPTIONS`。删掉任一条字段登记都会立刻变红，不再是「删了也全绿」。

### 豁免清单 `SHELL_SETTING_KEYS` 说明

只收「不是可配置项」的 `settings.*` 键，逐条带注释，分五类：
页壳与离开拦截（`title`/`cancelHint`/`leaveXxx` 等）；从属文案（hint / 占位 / 内联标签 / 选项文案，
已注明其所属项）；供应商编辑器的表单字段与动作；保存校验文案（`vRequired` 等）；
状态徽标与反馈提示（`lspFound`/`reloadSkills` 等）。豁免清单与注册表**不得重叠**（测试断言）。

批④ 起 `save` / `saved` / `cancel` / `remove` 已从本清单移除（收敛进 `common` 段；`common.*` 不是 `settings.*`，
既不进注册表也不进本清单），空态与占位符的拆出键（`skillsEmpty` / `aiLanguagePlaceholder`）与关于页只读入口的从属文案按同类登记。

## 4. 脏标记：`PAGE_FIELDS` 驱动

- `pageSlice(config, page)` 只取 `PAGE_FIELDS[page]` 的字段；两类字段刻意跳过：
  `INSTANT_APPLY_FIELD_IDS`（改完立即生效，纳入会永远显示「未保存」）与 `MCP_FIELD_ID`
  （MCP 不在 config 内，脏判定走 mcp.json 文本基线，结果并在 `tools` 页上）。
- 归一语义保持不变：空值三态折叠（`null`/`undefined`/`""`/纯空白）、
  `proxy: null` 折默认对象（`mode = system`）、数组内空条目与全空对象视作空值。
  （历史：`validation.java` / `validation.dart` / `validation.lsp.*` 的缺省折叠已随写入后检查的引入删除。）
- 由此：**「界面」与「关于」两页永不亮脏点**——它们只有即时生效项（主题 / 字体 / 界面语言 / 自动更新开关）
  与只读身份信息，没有可保存的改动。这是有意为之，`settings.page.test.tsx` 有对应用例守护。

## 5. 旧 → 新页 key 映射（深链兼容）

| 旧 key | 新 key | 说明 |
|---|---|---|
| `general` | `appearance` | 旧「通用」按项拆散到 4 页；深链（外部调用方）只可能指「进来看看」，落首组第一页 |
| `appearance` | `appearance` | — |
| `providers` | `providers` | 认证错误卡「打开模型设置」仍落此页 |
| `security` | `security` | — |
| `network` | `network` | 代理地址校验失败跳页仍走此页 |
| `mcp` | `tools` | 旧 MCP 页并入工具与集成 |
| `skills` | `tools` | 旧技能页并入工具与集成 |
| 未知 / 空 | `appearance` | `normalizePageKey` 回退默认页 |

`showSettings(tab?)` 语义未变：带参跳页（先归一）、无参保持当前页不重置（批① 的有意变更）。
macOS 应用菜单 `menu-about` 改为 `showSettings("about")`。

## 6. 退役与清理

- 删除 `ui/src/features/panels/AboutModal.tsx`、`ui.aboutOpen`、左下角状态区的「关于」入口
  （`InfoCircleOutlined` 按钮）；关于页用例并入 `__tests__/settings.page.test.tsx`。
- 删除零引用 CSS：导航列内的 `.settings-nav .ant-tabs*` 规则。
  （历史：`.lsp-budget-grid` 曾随写入后检查的引入被删除，现行页体用 `.postcheck-grid` 两列网格类。）
- 用户可见名历史：批④ 曾把「写入后**语法**校验」改叫「写入后**语义**校验」；2026-09-20 起整套语义校验
  被**写入后检查**取代（[post-write-check-plan](./post-write-check-plan.md)），`validation*` 一族键名与设置项已删除。
- 删除零引用 i18n 键（删前全仓 grep 确认）：`settings.general`、`settings.appearance`、
  `settings.security`、`settings.network`、`settings.accent`、`about.title`、`about.checkUpdates`、`app.about`。
  保留 `settings.providers` / `settings.mcp` / `settings.skills`（分别是供应商列表、MCP 服务器、
  技能三个设置项的显示名，**不做同义键收敛**——那是批④）。
- 新增键：`settings.pageAppearance/pageProviders/pageNetwork/pageSecurity/pageTools/pageAgent/pageLogs/pageAbout`
  与 `settings.groupUiModel/groupSafetyTools/groupDiagnostics`（中英双侧同步）。

## 7. 术语表

术语与键统一的全文（定名与判定规则、选段规则、键名前缀原则、旧→新键映射表、en 侧可见变更记账、三条守门用例）
已随**批④**落到 [settings-terminology](./settings-terminology.md)；批② 曾在此留了占位。

批④ 的浓缩口径（细则见上篇）：

- 同义键收敛到 `common.*`（`save` / `saved` / `cancel` / `delete` / `builtin`），带宾语的动作（`settings.deleteSkill`）与不同动作（`settings.mcpSave`）不并入；
- 跨段借键清零：`sessions.empty` → `settings.skillsEmpty`（**修缺陷**：技能空态曾显示「暂无会话」）、`composer.effortDefault` → `settings.aiLanguagePlaceholder`；
- `about.*` 段整体迁入 `settings.about*`（否则不在引用闭包的守护范围内）；
- 用户可见名「写入后**语法**校验」→「写入后**语义**校验」（en: `Post-write semantic validation`）；
- 键名前缀原则：**前缀 = 页面 / 功能归属，不追技术栈名** —— 故 `validation*` 键名一律不改。

## 8. 批③ 追加的登记要求（宽度档与进阶标记）

批③ 起「新增一项设置」的流程（§3）多出三项硬要求（细则与完整映射表见
[settings-search-and-advanced](./settings-search-and-advanced.md)）：

- **宽度档**：可调宽度的 `Input` / `Select` / `InputNumber` 在 `SETTINGS_ITEMS` 里标 `width`
  （`narrow` 180 / `mid` 240 / `wide` 360），并在页体挂 `.w-narrow` / `.w-mid` / `.w-wide`
  （三档都带 `max-width:100%`，窄窗不横向溢出）；整行控件（TextArea / Switch / Radio / Slider /
  网格内控件 / 复合容器 / 动作按钮）**必须**进 `WIDTH_EXEMPT_ITEM_IDS` 并写清「为什么没有宽度档」。
  两个清单不得重叠、相加恰好覆盖 `SETTINGS_ITEMS`（契约测试双向断言）；
  **页体内不得再写像素内联 `width`**（容器级 `maxWidth` 不属本批范围）。
- **进阶标记**：`advanced: true` 的项由页级「显示进阶项（N）」开关（`.settings-advanced-toggle`，
  偏好存 localStorage `ws_settings_show_advanced`、默认收起、跨页跨会话）按行过滤——收起时只加类
  `.settings-advanced-hidden`（`display:none`），**零 DOM 搬迁**，进阶行留在原分组内；
  整组皆为进阶项的组整组隐藏（组容器标 `data-setting-group-id`）。折叠切换**不产生未保存改动**
  （不碰 draft），进阶项自身的改动仍由 `PAGE_FIELDS` 判定。
 - **锚点**：每项需带 `data-setting-id="<item.id>"`（`.setting-anchor` 包裹层）供搜索命中定位；
   唯一退化项 `active_model_id`（其标记只在「编辑供应商」视图出现）已在上述文档登记；
   2026-09-19 起它同时**不再是进阶项**（列表视图没落点，页级开关对它无意义），详见 [lsp-detection-and-settings-ux.md](./lsp-detection-and-settings-ux.md)。

搜索能力本身也在注册表里：`matchSettings(query, t)` 的命中范围 = 显示名 + `keywords`（直字符串、不进 i18n）
+ 页名 + 组名，多词 AND、空串返回空、按「页序 → 组序 → 注册表原序」稳定输出。
