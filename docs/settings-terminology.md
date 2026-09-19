# 设置页重整 · 批④：术语与 i18n 键统一（含关于页打磨）

> 状态：已实施（批④）。批① 全屏容器（[settings-fullscreen-shell](./settings-fullscreen-shell.md)）、批② 8 页重划与注册表（[settings-ia](./settings-ia.md)）、
> 批③ 搜索 / 进阶折叠 / 3 档宽度（[settings-search-and-advanced](./settings-search-and-advanced.md)）。
> 本批收口前四批遗留的两类问题——**同义键与跨段借键**、**关于页版式与入口**——并修一处真实缺陷：
> 「工具与集成 → 技能」空列表时显示的是**「暂无会话」**（借用了 `sessions.empty`）。
>
> **后续变更（2026-09-20，[post-write-check-plan](./post-write-check-plan.md)）**：写入后语义校验（LSP）机制整体删除，
> 改为 **写入后检查**（`settings.postWriteCheck` 分组 + `post_write_check.*` 四项）；本文中
> 「写入后**语义**校验」/ `settings.validation` 相关条目均为历史记录，现行定名为「写入后检查」。

本批**不做**（边界）：后端任何改动（`src-tauri/**` 零 diff）；代码标识符（组件名 / 文件名 / 注册表导出名与 id / 配置字段路径 / CSS 类名 / localStorage 键）；
全 8 页说明位大改造；第三方许可清单生成与许可证全文页；构建信息 / 平台 / 架构；批①-③ 的任何能力回退；新设置项功能与配置字段；i18n 机制更换与第三语言；GUI 自动点验。

## 1. 术语表（定名与判定规则）

| 概念 | 中文定名 | 英文定名 | 键 | 判定规则 |
|---|---|---|---|---|
| 删除条目 | 删除 | Delete | `common.delete` | **无宾语、语境相同**（会话 / 项目 / 供应商 / 模型 / 命令白名单 / SDK 根目录 / 计划任务 / 队列） |
| 保存 | 保存 | Save | `common.save` | 同上 |
| 已保存 | 已保存 | Saved | `common.saved` | 同上 |
| 取消 | 取消 | Cancel | `common.cancel` | 同上 |
| 内置来源 | 内置 | Built-in | `common.builtin` | 技能来源标签（右栏技能行与设置页技能行共用同一词） |
| 写后校验 | 写入后**语义**校验 | Post-write **semantic** validation | `settings.validation` | 用户可见名一律「语义」；旧的「语法」写法全批清零 |
| 删除技能 | 删除技能 | Delete skill | `settings.deleteSkill` | **带宾语**且用于 `aria-label`：屏幕阅读器需要完整动作名，**不并入** `common.delete` |

**「同义键」的判定 = 值与语境皆同者。**

- 值同、**语境不同** → 各自留键：`settings.mcpSave`（「保存并重连」是另一个动作）、`settings.cancelHint`（Tooltip 说明，不是按钮文案）、
  `settings.leaveSave / leaveDiscard / leaveStay`（三选拦截专用）。
- 值**不同** → 各自留键：`composer.effortDefault`（`默认` / `Default`，是推理强度档位名）与 `settings.aiLanguagePlaceholder`（同值但语义是「留空 = 跟随对话语言」的输入提示）
  —— 本批只做**拆键**，不做合并（值不变，零可见变更）。

**易混例外（本批明确保留）**：`tools.delete` 是 delete **工具的显示名**（工具卡片上的动作名），与通用「删除」动作无关，不并入 `common.delete`。

**en 同值不合并（登记为例外）**：`settings.skillsEmpty`（设置页技能区空态）与 `rightbar.noSkills`（右栏技能段空态）的 en 值同为 `No skills yet`；
zh 侧本就有别（「暂无技能」/「暂无可用技能」），且两处语境不同（设置页的技能管理区 vs 右栏的当前会话技能段），按本文判定规则属「值同、语境不同」→ 各自留键。
**不为凑差异去改 en 措辞**（那会平白多一处可见变更，也改变不了“两处都是无技能”的事实）。

## 2. 选段规则（何时进 `common`、何时进页面段）

1. `common.*`：**跨页面复用、无宾语、语境相同**的动作 / 状态词。新增键前先问两句——「有超过一个页面用它吗？」「两处宾语一样吗？」——都为「是」才进 `common`。
2. `settings.*`：设置页专属（页壳文案、项名、从属文案）。收敛后**设置页不得引用任何非 `settings.*` / `common.*` 的键**（守门用例 ②）。
3. 其他页面段（`nav.*` / `sessions.*` / `tasks.*` / `queue.*` / `rightbar.*` / `composer.*` / `lsp.*` …）：各自功能的专属文案。
   **跨页借用即缺陷**——本批修的两处（`sessions.empty`、`composer.effortDefault`）都是「设置页借了别的页面的键」。
4. `common` 不是垃圾桶：只有「同一句话被两处以上引用」才收敛；一次性文案留在页面段。

## 3. 键名前缀原则

**键名前缀 = 页面 / 功能归属，不追技术栈名。**

- 因此 `validation*` 一族键名**不改**：它们与后端 config 字段路径同源（`validation.lsp.*` ↔ `ValidationSettings`），注册表的 `id` 就是字段路径，责任边界清楚；
  若改成 `lsp*`，等于把「设置项归属（工具与集成页）」与「实现技术（LSP）」绑死，将来换实现（或 JSON 走内置解析这类非 LSP 路径）就要改键名与注册表 id。
- 同理 `settings.mcp` 留在原段：MCP 是功能名（用户找它的词），不是技术栈名。
- 推论：键名与**实现**解耦、与**页面归属**耦合；重命名只应发生在「归属变了」时（如本批 `about.*` → `settings.about*`）。

## 4. 旧 → 新键映射表（本批全部改动）

### 4.1 删除类 → `common.delete`

| 旧键 | 引用点（全部切换） |
|---|---|
| `settings.remove` | `ProvidersPanel.tsx` 4 处：模型行删除确认（1）、供应商编辑视图删除按钮 + 确认标题（2）、供应商列表删除确认（1） |
| `sessions.delete` | `SettingsPage.tsx` 2 处（命令白名单行按钮、SDK 根目录删除 `aria-label`）+ `ProjectNav.tsx` 1 处（会话行删除 `title`） |
| `nav.delete` | `ProjectNav.tsx` 1 处（删项目确认框 `okText`） |
| `tasks.delete` | `TaskCenterPanel.tsx` 2 处（确认标题 + 按钮） |
| `queue.delete` | `QueuePanel.tsx` 1 处（Tooltip） |

### 4.2 保存 / 取消 → `common.save` / `common.saved` / `common.cancel`

| 旧键 | 引用点 |
|---|---|
| `settings.save` | `SettingsPage.tsx` 操作条保存、`ProvidersPanel.tsx` 模型弹框 `okText` |
| `settings.saved` | `SettingsPage.tsx` 2 处（页级保存成功、MCP 保存成功） |
| `settings.cancel` | `SettingsPage.tsx` 操作条取消、`ProvidersPanel.tsx` 模型弹框 `cancelText` |
| `nav.save` | `ProjectNav.tsx` 2 处（新建 / 编辑项目弹框的保存按钮、重命名会话弹框 `okText`） |
| `nav.saved` | `ProjectNav.tsx` 2 处（项目保存 toast、会话重命名 toast） |
| `nav.cancel` | `ProjectNav.tsx` 1 处（项目弹框取消） |

### 4.3 内置 → `common.builtin`

`rightbar.skillBuiltin`：`RightBar.tsx` 技能行（1）+ `SettingsPage.tsx` 技能行（1）。

### 4.4 借用键拆出（含缺陷修复）

| 旧键（借用方） | 新键 | 引用点 | 说明 |
|---|---|---|---|
| `sessions.empty`（暂无会话） | `settings.skillsEmpty`（暂无技能 / No skills yet） | `SettingsPage.tsx` 技能区空态 | **真实缺陷**：「工具与集成 → 技能」空列表显示「暂无会话」；收敛后 `sessions.empty` 零引用，同批删除 |
| `composer.effortDefault`（默认） | `settings.aiLanguagePlaceholder`（默认 / Default） | `SettingsPage.tsx` AI 语言输入 `placeholder` | 值不变（零可见变更），只是不再跨段借键；`composer.effortDefault` 保留给推理强度档位 |

### 4.5 同义整句合并

`lsp.confirmCost` 与 `settings.lspJavaCost` 是同一句话（启用 Java 校验的代价）。

**保留 `settings.lspJavaCost`，删除 `lsp.confirmCost`**，`LspGuideCard.tsx` 的 `confirm_enable` 卡片改引 `settings.lspJavaCost`。理由：

1. 它落在 `settings.*` 段 → 受注册表引用闭包守护；`lsp.*` 段不在闭包扫描范围内，留着等于无人拦；
2. 句子**自含主语**（「启用 **Java** 语义校验…」），而引导卡只在 Java 的 `confirm_enable` 场景出现，直接引用即可读完整；
3. 术语与 `settings.validation`（写入后语义校验）同源。

**这不是纯键名搬家：引导卡上这句的中英双语都换了措辞**（保留的那个键的值与 `lsp.confirmCost` 本就不同值），
逐条记账见 §4.7 第 2 条。

### 4.6 关于页键迁入 `settings.about*` 与新增

| 旧键 | 新键 |
|---|---|
| `about.slogan` | `settings.aboutSlogan` |
| `about.appData` | `settings.aboutAppData` |
| `about.repo` | `settings.aboutRepo` |

新增（关于页只读入口 —— 批② 起它们既没有独立键也没有注册项，导致批③ 搜索在关于页只能命中 2 项）：

`settings.aboutVersion` / `aboutVersionHint` / `aboutLogsDir` / `aboutLogsDirHint` / `aboutLicense` / `aboutLicenseHint` +
四个动作按钮文案 `aboutOpenAppData` / `aboutOpenLogsDir` / `aboutOpenRepo` / `aboutViewLicense`。

迁入理由：注册表的引用闭包**只扫 `settings.*` 前缀** —— `about.*` 段的键不在守护范围内，迁入后才受契约测试约束（这是本批最容易被忽略的一步）。

### 4.7 可见变更记账（en 为主，含一处双语）

| # | 位置 | 变更 | 语言 |
|---|---|---|---|
| 1 | `ProvidersPanel.tsx:233` / `:573` / `:632`（三个 Popconfirm 标题）+ `:576`（删除按钮文案）= **4 处** | `Remove` → `Delete` | 仅 en |
| 2 | `LspGuideCard.tsx:101` 引导卡 Java 代价**整句**（改引 `settings.lspJavaCost`） | 句子换措辞（补上「Java」主词，与 `settings.validation` 同源） | **中英双语** |

成因：① 统一到 `common.delete` 的 en 写法；② 同义整句合并保留的是 `settings.lspJavaCost`（§4.5），它的值与 `lsp.confirmCost` 本就不同——
引导卡上这句**中英双语都变了**（旧：`启用后会启动 jdtls 并解析依赖树…` / `Enabling starts jdtls and resolves…`；
新：`启用 Java 语义校验会启动 jdtls 并解析依赖树…` / `Enabling Java semantic checks starts jdtls and resolves…`）。

**除上表第 2 条（引导卡 Java 代价整句）外**，其余改动的值**逐字未变**（`settings.save` → `common.save` 等只是键名搬家；
`about.appData` → `settings.aboutAppData` 同理），中文侧零变化。

## 5. 守门用例（四条，全部做实）

1. **已删键不得复活**（`ui/src/__tests__/i18n.keys.test.ts`）：对本批删除的 17 个键逐个断言在 **zh-CN 与 en-US 双侧都不存在**；
   并断言 `about.*` 段**整体退役**（前缀扫描不留残留）；姊妹用例断言**新增键全量对偶** ——
   段级（`common` 整段键集写死为 5 把）+ 前缀级（`settings.about*` 两侧集合逐把相等、数量写死 15 把）+ 两个拆出键点名，
   不再手工维护「13 把抽样」（批④ 返工：`aboutVersionHint` / `aboutOpen*` 这些漏改另一语言原本不会红）。
2. **不得跨段借键**（`ui/src/__tests__/settings.registry.test.ts`，覆盖面 `features/**/*.tsx`）：
   - `features/panels/*.tsx`：闭包从「只认 `t("settings.X")`」**扩到**「所有字面 `t("...")` 键」，断言属于 `settings.*` / `common.*` 段，
     或在显式豁免清单 `NON_SETTINGS_PANEL_SEGMENTS`（粒度：「**文件 → 段**」而不是「整文件」）内；
   - **非 panels 目录**（批④ 返工补的逃逸面）：每个文件进「**文件 → 允许段**」白名单（`FEATURE_FILE_SEGMENTS`，**精确到文件，不做整目录放行**），
     允许段只能是共享段（`app.` / `common.`）、本目录自有段（`DIR_OWNED_SEGMENTS`）或登记在案的跨段借用（`CROSS_SEGMENT_BORROWINGS`，逐条写理由）——
     本批自己新增的 `chat/LspGuideCard.tsx → settings.lspJavaCost` 就是最后一类（首条登记项）；
   - **变量拼出的键名**（批④ 返工补）：`t(` 调用点分三类（字面 / 内联字面 / 纯动态）并要求对账，
     纯动态的调用点（`t(key)`、`t(LANG_LABEL_KEY[lang])`）必须进 `DYNAMIC_KEY_CALLS` 逐条登记理由 —— 否则改引他段键也不会红；
   - 反向守卫：白名单 / 豁免清单都不悬空（登记了却已不存在 = 清单过时）；**正则失效守卫** —— 源码里出现 `t(` 的页体必须被扫到 ≥1 个字面键。
3. **术语一致性**：断言 i18n 双语**全字典**不再出现「语法校验 / syntax validation」写法、设置项名恰为「写入后语义校验」/「Post-write semantic validation」；
   并对文档做一条轻量同步检查（批④ 返工把检查数组补全）—— 命名现状的文档（`settings-ia.md` / `settings-terminology.md` / **`technical-design.md`**，
   后者是 AGENTS.md 列为必读的技术基准，此前不在数组里、写着旧名字也没人拦）整篇不得留旧写法；
   历史实施报告（`lsp-post-write-diagnostics.md` / `builtin-tools-source-comparison.md` / `p1-plan.md` / `tools-optimization-and-gap-fill-plan.md`）
   里对旧路径历史文案的引用必须标注「**历史文案**」并点明现行定名。
4. **技能空态（DOM 层）**：`ui/src/__tests__/settings.skills.test.tsx` 与 `rightbar.skills.test.tsx` 各断言空态文案
   （「暂无技能」/「暂无可用技能」）且**不得退回「暂无会话」** —— 批④ 修的缺陷正是这条跨段借用键，
   而两侧此前都没有 DOM 断言（把文案换成他段键全量零变红，与本批缺陷同族）。

## 6. 关于页打磨（仅本页页内）

### 6.1 版式对齐（5 处）

| # | 原状 | 现在 |
|---|---|---|
| 1 | 更新行说明位用 `tooltip`（**悬停才可见**，与其余 7 页的 `extra` 不一致） | `Form.Item` 的 `extra={settings.updatesHint}` |
| 2 | 「数据目录」「代码仓库」只有按钮文案、**没有说明位** | 每个入口行补 `extra` 说明（`aboutAppDataHint` / `aboutLogsDirHint` / `aboutRepoHint` / `aboutLicenseHint`） |
| 3 | 版本号是身份块里的**裸文本**（无标签行、无锚点、不可搜） | 独立 `Form.Item` 行：标签「版本」+ `extra` 说明 + 值，并挂锚点 `data-setting-id="app.version"` |
| 4 | 两个入口按钮挤在身份块内的 `.about-actions` 按钮排 | 各自成 `Form.Item` 行（标签 + 说明 + 行内按钮），与其余 7 页同形；`.about-actions` 与「居中 + 覆盖式反居中」两条兜底 CSS 同批移除 |
| 5 | 就地错误提示 `.about-error` 夹在身份块内部（按钮下方） | 移到 `Form` 末尾（四个入口共用一条提示路径，不再插在身份信息与入口之间） |

标签行 + 说明文本的标准形：`<Form.Item label={…} extra={…}>` 内放 `<div className="setting-anchor" data-setting-id="<item.id>">控件</div>`（与其余 7 页一字不差）。

### 6.2 两个新入口（零后端改动）

| 入口 | IPC | 说明 |
|---|---|---|
| 打开日志目录 | `open_logs_dir`（既有 `ipc.openLogsDir()`） | 此前只在右栏「日志」Tab 用；**右栏入口保留**（场景不同：右栏是看当前会话日志，设置页是找全局日志目录） |
| 开源许可证（MIT） | `open_url`（既有 `ipc.openUrl()`） | 打开仓库根 `LICENSE`：`https://github.com/Yangshifu1024/CodeWave/blob/main/LICENSE`（默认分支 main，不复制许可证全文 —— 本批明确非目标） |

两者的可点性由 `settings.page.test.tsx` 断言（mock IPC 被调用 + 失败时就地提示且不离开设置页）。

### 6.3 注册表登记（A6）

关于页 5 个只读条目登记进 `SETTINGS_ITEMS`（`app.version` / `app.data_dir` / `app.logs_dir` / `app.repo` / `app.license`），各带中英关键词，
并全部进 `WIDTH_EXEMPT_ITEM_IDS`（整行「标签 + 说明 + 值/按钮」）。`app.*` 前缀 = 无落盘字段，因此不进 `PAGE_FIELDS`（与 `app.check_updates` 同形），
关于页仍然**永不亮脏点**。锚点覆盖用例（`settings.page.test.tsx`）从 40 项涨到 45 项，逐项守护。

## 7. 测试与文件

| 文件 | 内容 |
|---|---|
| `ui/src/i18n/zh-CN.ts` / `en-US.ts` | 新增 `common` 段（5 键）；删除 17 个旧键 + `about` 段；新增 `settings.skillsEmpty` / `aiLanguagePlaceholder` / 15 个 `settings.about*`；`settings.validation` 值改「写入后语义校验」 |
| `ui/src/features/panels/settingsRegistry.ts` | 关于页 5 个只读入口登记（含 keywords）+ `WIDTH_EXEMPT_ITEM_IDS` 扩容；`SHELL_SETTING_KEYS` 去掉 `save/saved/cancel/remove`、补 about 从属文案 |
| `ui/src/features/panels/AboutSettings.tsx` | 版式重排（5 处）+ 两个新入口 + `entryRow` 复用 |
| `ui/src/features/panels/SettingsPage.tsx` / `ProvidersPanel.tsx` / `TaskCenterPanel.tsx` / `ui/src/features/shell/ProjectNav.tsx` / `RightBar.tsx` / `ui/src/features/chat/QueuePanel.tsx` / `LspGuideCard.tsx` | 引用点切换（见 §4） |
| `ui/src/theme/app.css` | 关于页：`.about-body` 直接左对齐、移除 `.about-actions` 与覆盖式兜底规则（类名不动） |
| `ui/src/__tests__/{i18n.keys,settings.registry}.test.ts` | 四条守门（§5）；批④ 返工：非 panels 目录的「文件 → 允许段」白名单 + 变量键名登记 + 文档检查数组补全 + 新增键全量对偶 |
| `ui/src/__tests__/{settings.page,app.smoke,lsp.guide-card}.test.tsx` | 受术语与文案变更影响的断言更新 + 关于页四入口 IPC 用例 + 关于页只读条目可搜用例 |
| `ui/src/__tests__/{settings.skills,rightbar.skills}.test.tsx` | 技能空态的 DOM 断言（批④ 返工补：设置页「暂无技能」/ 右栏「暂无可用技能」，且不得退回「暂无会话」） |

## 8. 人工验证清单

1. 「工具与集成 → 技能」空态显示**「暂无技能」**（不再是「暂无会话」）。
2. 各处删除按钮 / 确认框文案一致（会话行、项目、供应商、模型、命令白名单、SDK 根目录、计划任务、队列）。
3. 供应商 / 模型删除按钮**英文**已为 `Delete`（中文不变）——共 **4 处**：三个确认框标题（模型行 / 供应商编辑视图 / 供应商列表行）+ 1 个删除按钮（供应商编辑视图）。
4. 设置项标题为**「写入后语义校验」**，与文档一处不差。
5. **引导卡 Java 代价整句（中英双语都变了）**：未启用 Java 校验时点「启用」前的引导卡正文应分别为
   「启用 Java 语义校验会启动 jdtls 并解析依赖树，可能耗时数分钟、占用 GB 级内存。」/ `Enabling Java semantic checks starts jdtls and resolves the dependency tree; it may take minutes and use gigabytes of memory.`
   （切界面语言各看一遍；旧文案是「启用后会启动 jdtls…」）。
6. 右栏信息页「技能」段为空时显示**「暂无可用技能」**（英文 `No skills yet`），不是「无活动会话」。
7. 关于页：五行只读入口版式与其余 7 页一致（标签 + 灰字说明），「打开日志目录」「查看许可证」可点并生效。
8. 批③ 搜索能命中关于页的**版本 / 数据目录 / 日志目录 / 代码仓库 / 许可证**五个条目（此前只有 2 项）。
9. 批①-③ 既有能力零回退（全屏覆盖、Esc 链、脏点、三选拦截、搜索、进阶折叠、3 档宽度）。
