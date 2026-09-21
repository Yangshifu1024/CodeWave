# 界面「辅助信息可见性」审计与修复批次

> 状态：已实施（2026-09-22）。起因：用户反馈「设置 → 关于 → 更新」行尾的**「即时生效」标注在最右侧，很难看到**。
> 修掉那两处后顺势全仓扫了一遍同类问题（辅助标注被推到远侧 / 与控件脱开 / 弱化到读不到 / 位置歧义），本文件记录口径、发现与改动。
>
> 相关：[settings-ia](./settings-ia.md) §9（同批的 MCP / 技能拆页）、[settings-fullscreen-shell](./settings-fullscreen-shell.md)（「即时生效」不参与脏标记）、
> [settings-search-and-advanced](./settings-search-and-advanced.md)（说明位走 `Form.Item` 的 `extra` 是既有范式）、[composer-token-rate](./composer-token-rate.md)。

## 1. 审计口径（避免误读结论）

- **事实** = app.css 的样式声明 + TSX 的 DOM 父子/兄弟关系（可 grep、可复现）。
- **推断** = 「实际会被推到多远 / 能不能一眼看到」：测试环境 happy-dom **没有布局引擎**，像素级观感只能人工确认，
  故本批的分级与改动都建立在「事实 + CSS 声明的效果」上，交付时附手动验证清单。
- 全仓只有 `ui/src/theme/app.css` 与 `ui/src/theme/native.css` 两个样式表（后者只有基座与字体 token）——
  「类名在 app.css 查不到」即「该处没有任何样式」。

## 2. 四类问题与发现

### A. 自动外边距把**信息**推到远侧

| 位置 | 现状（事实） | 处理 |
|---|---|---|
| `.settings-update-row .settings-update-label` + `AboutSettings` | 标题带 `margin-right:auto`，行尾那枚「即时生效」被推到整行最右 | **已修**：标注写进标题括号 |
| 右栏 `.rb-skill-origin` | `margin-left:auto` + 45% 限宽 + 10.5px + dim 四重弱化，来源被推到行尾；设置页同一信息（`.skill-origin`）没有 auto margin | **已修**：去 auto margin、字号 11px，与设置页统一 |
| `.composer-toolbar .toolbar-right` | `margin-left:auto` 把**含信息**的 `.ctx-label`（上下文 / 命中 / 速率）连同控件一起推到最右 | **已修**：信息段独立成 `.toolbar-info`，控件段保持靠右 |
| `.project-row .row-action` | 同块已有 `position:absolute; right:8px`，`margin-left:auto` 是死声明 | **已修**（删） |
| `.settings-update-row .settings-update-label`（残留） | 改完标题括号后该 auto 外边距已无效果，但留着下次加元素就复发 | **已修**（删） |

**刻意保留（动作按钮靠右属设计）**：`.settings-actions-buttons`（保存/取消）、`.ask-pager`（分页器）、
`.settings-subhead` / `.skills-toolbar` / `.rb-files-tools` / `.plan-head` 的 `space-between`、
`.session-nav-row .session-time`（定高时间槽 + hover 让位给行操作）、搜索结果行的「所属页名」右置。

### B. 说明与控件脱开

设置页整体已是良好范式（说明走 `Form.Item` 的 `extra`）。本批唯一改动的是**两处「即时生效」**：

- 关于·更新行：`.settings-instant` 作为 flex 行第三个子节点挂行尾 → 改为标题括号（`settings.instantApplySuffix`）。
- 界面·界面语言：`.settings-instant` 作为 `Form.Item` 的兄弟节点挂在控件下方 → 同样改为标题括号。

两处的 `.settings-instant` 类与 `settings.instantApply` 键随之成为死代码，已一并删除（CSS 规则 + i18n 键 + 注册表豁免条目）。

### C. 过小 / 过淡到读不到，且承载关键信息

| 位置 | 问题 | 处理 |
|---|---|---|
| `SettingsPage.tsx` 4 处 + `ProvidersPanel.tsx` 2 处 `Form.Item tooltip=` | shellHint / 保留期 / 日志级别 / 会话详细日志 / key 掩码 / 请求头说明，都是含限制条件的 50–120 字长文案，**只能悬停问号才看到**（同一页的关于页却明示「说明走 extra」） | **已修**：全部改 `extra`；`apiKeys` 的 placeholder 同时移除（`extra` 已常驻，避免同一句话出现两次） |
| `TokenStatsModal` 的 `.stats-summary dim` | 面板**主结论数字**（合计 / runs / 平均速率 / 平均 TTFT）与次要说明同色 | **已修**：容器走 `--ws-text-2`，只有标签（`span.dim`）降到 `--ws-dim` |
| `.bar-col .lbl` | x 轴日期 9px | **已修**：10.5px |
| `.sub-card-meta` 的「提前退出原因」 | done 态的诊断信息与普通 meta 同色（旁边图标却是橙色） | **已修**：新增 `.sub-card-warn` 走 `--ws-warn` |
| `ProvidersPanel` 内联 `opacity: 0.45 / 0.55 / 0.65` | 承载「为什么加不了」等约束说明，且不跟主题走 | **已修**：改 `.dim` / 类样式 |
| 硬编码色 `#d46b08` / `#cf1322` / `#ff4d4f` / `#e5484d`；死类名 `.provider-models-error`（app.css 无规则） | 暗色下对比不足 / 类名无样式 | **已修**：改 `--ws-warn` / `--ws-err`，并补 `.provider-models-error` 规则 |

**不动（装饰性副文本）**：`.msg .role .ts`、`.nav-section-title`、`.rb-label`、`.gitdiff .stats`、
`.tool-card .dur`、`.rb-file-meta`、右栏日志 10.5px 正文等——小字号 + dim 是刻意设计。

### D. 条件渲染 + 位置歧义

| 位置 | 问题 | 处理 |
|---|---|---|
| `AboutSettings` 四个入口行共用一个 `actionError`，渲染在 `<Form>` 末尾 | 点第 2 行失败，错误可能出现在 3–4 行之外 | **已修**：错误带「触发行 id」，由 `entryRow` 渲染在自己那行下方 |
| `TaskCenterPanel` 计划表达式语法**只作为 placeholder**（敲第一个字符即消失），创建按钮禁用无就地原因 | 语法无处可查、禁用原因不可知 | **已修**：语法说明落成输入框下的常驻 `.hint`；禁用原因显示在按钮旁 |

## 3. 相邻发现（同批一并修）

- `ProjectNav` 的「等待确认」用 `Tag color="success"`（预设绿），违反「色彩强度只映射风险等级、无色彩=默认」——
  改为中性默认标签；`TaskCenterPanel` 任务状态标签同问题（`success` → `default`，失败态仍为红）。
- `TaskCenterPanel` 的 `{task.last_status ?? "待触发"}` 是硬编码中文（英文界面会露中文）→ 新增 `tasks.pending`。
- mermaid 流式占位提示原为 CSS `content: "⏳ 图表将在回复定稿后渲染"`（中文写死在 CSS，i18n 不可达）→
  改由 markdown 渲染器写进 `data-pending`（i18n 键 `chat.diagramPending`），CSS 用 `content: attr(data-pending)`；
  `renderCached` 的缓存键含该文案（否则切语言会取回旧语言的提示）。

## 4. 连带改动的测试断言

改动了可见结构，三个测试文件同步收紧/调整（不是放宽），并补了两个新文件（见 §5.1）：

| 文件 | 调整原因 |
|---|---|
| `settings.page.test.tsx` | 关于页版式：断言标题含「更新（即时生效）」、行内不再有 `.settings-instant`；界面页断言「界面语言（即时生效）」；错误下放后改为断言 `.about-error` 落在 `[data-setting-id="app.data_dir"]` 内（旧写法会红） |
| `tokenstats.modal.test.tsx` | 摘要行的标签与数字现在是两个节点，改取整行文本（`closest(".stats-summary")`）——检查面反而变大 |
| `providers.panel.test.tsx` | `apiKeys` 的 `placeholder`（掩码说明）移除，改为按 Form.Item 标签定位该 textarea |
| `projectnav.row-states.test.tsx` | 新增「等待确认」徽标不许是预设绿/红 |
| `subagent.card.test.tsx` | no_report 例补断言：收尾原因文本带 `.sub-card-warn`（与警示图标同档） |

## 5. 验证

### 5.1 自动化

- `pnpm --dir ui test` → **86 文件 / 946 用例全绿**；`pnpm --dir ui build` 通过；`pnpm --dir ui run lint` 0 error（1 条既有 warning 在 `Composer.tsx`）。
- **本批新增三个测试文件**（补审查指出的新行为覆盖缺口）：
  - `__tests__/tasks.center.test.tsx`（5 例）：语法说明常驻 + placeholder 仅为示例、三字段未齐时禁用且就地给原因、填齐后可创建（参数顺序 name/instruction/schedule）并清空重拉、空态、「待触发」走 i18n、`ok` 中性 / 非 ok 红。
  - `__tests__/rightbar.style.test.ts`（5 例，CSS 契约，与 `ask.style.test.ts` 同路）：右栏技能来源不再 auto margin 且 11px、关于·更新标题无 auto margin、`.settings-instant` 不复存在、`.stats-summary` 走 `--ws-text-2`、右栏日志与 `.sub-card-warn` 走 token 且无硬编码色。
  - `__tests__/mcp.status-table.test.tsx`（8 例）：抽出 `panels/McpStatusTable.tsx` 后的组件级用例——四态渲染（含 spinner）、空表整段不渲染、只有失败行可点（role/tabIndex/aria-expanded）、点击与**键盘 Enter/Space** 展开收起、同时只展开一行、刷新按钮 aria-label 与 loading 态、`mcpStatusRow` 四态映射（含未知 state 落到「未连接」）。与整页集成用例（`settings.mcp.test.tsx`）分层：那边管名单并集 / 刷新接线 / 切页 refetch，这边管展示与交互。
- MCP 状态表另补：`starting` 态（spinner + 「连接中」+ 不可点）、刷新失败**保留旧值**、刷新前后 `connect_mcp` 计数不变（原「全仓为空」写法是恒真空断言）；mermaid `data-pending` 与「缓存键含提示文案」在 `markdown.math.test.ts` 各一条。
- 样式契约测试（`composer.rate.style.test.ts` 按字面选择器抓 app.css 规则）保持不变：`.composer-toolbar .ctx-label …`
  等选择器前缀未动，只新增 `.toolbar-info` 一条规则。

### 5.2 手动验证清单（界面改动不做 GUI 自动点验）

1. 关于·更新 与 界面·界面语言 两行标题带括号（「更新（即时生效）」「界面语言（即时生效）」），行内无孤立标注。
2. Composer 工具条改为「左：+/权限/子代理 ｜ 中：上下文/命中/速率 ｜ 右：压缩/模型/力度/发送」——信息段紧跟左侧段，不再贴右端。
3. 设置页 Shell / 保留期 / 日志级别 / 会话详细日志，与供应商的 API Key / 请求头说明：**常驻可见**，不需要悬停问号。
4. 统计面板：主结论数字比标签明显、x 轴日期可读。
5. 子代理提前退出（预算耗尽 / 未出汇报）时，收尾原因文本呈橙色，与警示图标同档。
6. 暗色主题下：右栏日志的 warn / err、供应商表单错误行对比正常（都走 `--ws-warn` / `--ws-err`）。
7. 关于页四个入口按钮失败时，错误出现在**被点的那一行下方**（不再统一堆在表单末尾）。
8. 任务中心：计划语法说明常驻输入框下、创建按钮禁用时旁边就有原因、状态标签无绿色。
9. 会话导航「等待确认」徽标不再是绿色；MCP 页刷新失败时弹「读取 MCP 状态失败（显示的是上一次结果）」且状态不变。
10. 英文界面下 mermaid 流式占位提示为英文（`data-pending` 走 i18n）。

## 6. 非目标

- 不做真实像素级布局回归（happy-dom 无布局引擎，最终观感仍需人工看）。
- 不改动作按钮靠右的一族版式（刻意设计）。
- 不引入新的颜色 token（只把硬编码色收敛到既有 `--ws-warn` / `--ws-err` / `--ws-dim` / `--ws-text-2`）。
- 不改 Composer 工具条的数据源与三段口径（[composer-token-rate](./composer-token-rate.md) 契约不变，仅位置）。
