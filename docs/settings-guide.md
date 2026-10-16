# 设置页维护指南

核对日期：2026-10-12。本文按当前源码说明页面归属、保存边界、搜索与公共视觉契约，取代设置页分批实施报告。设置现在是 **10 页、3 组导航的全屏覆盖页**；所有设置项默认可见，不再提供进阶折叠开关。供应商、密钥、代理与额度详见[供应商与网络](./providers-and-network.md)，全局主题与窗口交互详见[聊天与界面](./chat-and-interface.md)。

## 页面与源码入口

| 导航组 | 页 key / 页名 | 内容 |
|---|---|---|
| 外观与模型 | `appearance` / 界面 | 界面语言、系统/浅色/深色主题、界面字体与等宽字体 |
| 外观与模型 | `providers` / 模型与供应商 | AI 回复语言、供应商与模型、子代理角色模型覆盖 |
| 外观与模型 | `network` / 网络与连接 | 代理模式与地址、内网访问 |
| 安全与能力 | `security` / 安全与审批 | 审批与确认开关、自动确认、命令白名单 |
| 安全与能力 | `mcp` / MCP | 用户级/项目级配置、服务器状态卡片、编辑与连接操作 |
| 安全与能力 | `skills` / 技能 | 启停、重新扫描、托管技能删除 |
| 安全与能力 | `tools` / 写入后检查与校验 | 写入后检查的开关、命令、超时、输出尾字符数 |
| 安全与能力 | `agent` / 工作区与智能体 | Shell、自定义提示词、上下文压缩、会话保留期与清理、旧格式历史清理 |
| 诊断与其他 | `logs` / 日志 | 日志级别、会话详细日志 |
| 诊断与其他 | `about` / 关于 | 版本与更新、数据/日志目录、仓库与许可证 |

核心入口：

- [settingsRegistry.ts](../ui/src/features/panels/settingsRegistry.ts)：纯数据注册表，集中管理页序、分组、项名、字段归属、搜索词和宽度档；不依赖 React 或 store，页体仍由组件手写。
- [SettingsPage.tsx](../ui/src/features/panels/SettingsPage.tsx)：页壳、草稿、保存、离开拦截、搜索定位及各页组合。
- [stores/ui.ts](../ui/src/stores/ui.ts)、[AppShell.tsx](../ui/src/features/shell/AppShell.tsx)、[ResizeHandle.tsx](../ui/src/features/shell/ResizeHandle.tsx)：打开设置、工作区让位、退出和键盘事件。
- [SettingsTheme.tsx](../ui/src/features/panels/settings/SettingsTheme.tsx)、[settings-theme.css](../ui/src/features/panels/settings/settings-theme.css)：公共视觉配置、表单行、分组、弹框与响应式布局。
- [FontSettings.tsx](../ui/src/features/panels/FontSettings.tsx)、[ProvidersPanel.tsx](../ui/src/features/panels/ProvidersPanel.tsx)、[SubagentModelsPanel.tsx](../ui/src/features/panels/SubagentModelsPanel.tsx)、[AboutSettings.tsx](../ui/src/features/panels/AboutSettings.tsx)：独立页体/子面板。
- [McpStatusTable.tsx](../ui/src/features/panels/McpStatusTable.tsx)、[McpKvTable.tsx](../ui/src/features/panels/McpKvTable.tsx)：服务器卡片、状态/工具说明、参数与键值编辑器；文件名中的 Table 不代表当前 UI 仍是六列表格。
- [TopBar.tsx](../ui/src/features/shell/TopBar.tsx)、[layout.ts](../ui/src/utils/layout.ts)：全屏页顶栏标题和导航分界；[host/commands/project.rs](../src-tauri/src/host/commands/project.rs)、[host/commands/mcp.rs](../src-tauri/src/host/commands/mcp.rs)：真实保存与独立操作边界。

## 页壳、导航与退出

- 设置打开时挂载，覆盖 AppShell 内层工作区；关闭设置时卸载设置页，**工作区始终保留挂载**。让位只用 `.workspace-covered` 的 `visibility: hidden`、`pointer-events: none` 与 `aria-hidden`，禁止改成 `display:none`，否则会破坏滚动几何和流式渲染。分隔条同时退出键盘焦点序列。
- 顶栏保留窗口拖动与窗口控制；设置打开时显示设置标题。打开/关闭设置、搜索或按 Esc 都不得停止运行中的会话。运行中会话指示可返回工作区。
- 设置/任务全屏页打开时，顶栏左段与该页左导航共用 `fullscreenNavWidth(windowWidth)`，不再跟随工作区被拖拽或折叠的 `navWidth`；全屏左段禁用单侧宽度动画，相关容器使用 border-box。设置顶栏只显示“设置”，隐藏会话/项目/分支簇及工作区侧栏按钮；Windows decoration 控制条的 CSP 来源与层级不能被覆盖层破坏。
- 默认页为 `appearance`。`showSettings(page)` 归一页 key 后跳页，`showSettings()` 保持当前页；`general` 映射为 `appearance`，未知 key 回退默认页。MCP、技能与 `tools` 的深链保持各自页 key。
- 设置根节点为 `role="dialog"` / `aria-modal="true"`，打开后聚焦返回按钮。导航的 tab 与当前 tabpanel 保持 `aria-controls` / `aria-labelledby` 闭环；方向键移动导航焦点，Enter/Space 才激活。没有额外的焦点陷阱库。
- Esc 优先级：已打开的离开确认/组件浮层 → 清空搜索 → 返回工作区（含未保存拦截）；输入法组合中不接管。AppShell 在设置打开时也必须让过全局停止快捷键。

## 保存边界与脏状态

| 类型 | 当前行为 |
|---|---|
| 主配置草稿 | 顶部提交页级草稿与 `page_version`、操作 UUID、显式凭据意图，后端强制保留即时字段，成功后留在设置页。供应商、网络、审批、技能启停、写入后检查、Shell/提示词/压缩、保留期和日志等走此路径 |
| 供应商弹框 | 新增与编辑均先保存弹框局部表单，确认/完成才合并到主草稿；取消丢弃局部改动，不等于已经落盘。模型编辑复用同一弹框，不叠第二层 Modal |
| 即时偏好 | 界面语言、主题、输出展示、三组字体与字号与自动检查更新独立生效，不参与脏点；取消主配置草稿不回滚它们 |
| 子代理模型覆盖 | `set_subagent_model` 只 patch 单角色，携带角色版本；同角色在途禁用，错误保留重试选择，顶部保存/取消均不覆盖已提交角色值 |
| MCP 配置 | 独立 `mcp.json`，通过编辑弹框的“保存并重连”调用 `mcpSaveConfig`；顶部主配置保存不覆盖它 |
| 操作与只读信息 | 技能重扫/删除、清理、连接操作、打开目录与检查更新走各自 IPC；`app.*` 注册项没有落盘字段 |

- 普通脏点由 `PAGE_FIELDS` → `fieldSlice` / `pageSlice` 对比草稿与已保存配置；即时项和 MCP 从主配置比较中排除。MCP 另比当前序列化文本与基线，脏点只挂在 `mcp` 页。
- 比较归一化空值（`null`、`undefined`、空串、纯空白），对象键排序，过滤数组空条目；`proxy: null` 等价于系统代理默认配置。新增有缺省语义的字段时同步检查比较逻辑，避免“改回原值仍有脏点”。界面与关于页没有待保存草稿，不应亮脏点。
- 切页、返回按钮、运行中指示、页内 Esc 与关窗请求共用“保存并离开 / 放弃改动 / 留在原地”。顶部取消直接丢弃未保存草稿并返回工作区，无二次确认。
- 代理地址或供应商校验失败时不落盘、不离开，转到问题页并丢弃待执行的搜索定位。保存成功或放弃后执行原导航意图；留在原地时清除该意图。MCP 仍须按其独立保存入口处理，不能把通用保存当作 MCP 保存。
- 关窗的“留在原地”必须向后端 `respondExitRequest("cancel")` 应答，不能只清前端 store。设置草稿处置完毕后，运行中会话的退出确认才接管。
- 字体提交覆盖回车、失焦、停输约 600ms 和卸载，避开输入法组合态；本地应用/缓存与后端字体偏好持久化由 [fonts.ts](../ui/src/utils/fonts.ts) 和字体组件负责。
- 字体真源是 `config.ui.font_sans` / `font_content` / `font_mono` 及各自的 `font_size_*`，localStorage 只供首帧应用。配置加载以后端为准；后端三槽字体均为空而缓存有值时尝试安全迁回。字体按顺序提交字段 patch，与所有配置写者共用提交锁，后端成功回执才更新已保存快照；失败显示后端原因并提供同值重试按钮，不能宣称下次启动必定恢复。字体名只引用本机字体，不安装字体。
- 后端 `ConfigCoordinator` 统一所有 config 写者，页保存保护三槽字体与字号与 `subagent_models`。`page_version` 仅在普通字段变化时递增；角色分别有 `role_versions`，全局 `{boot_id, sequence}` 排序快照。技能独立 patch 和命令“始终允许”追加属于页面域，旧草稿会冲突而不是撤销新配置；命令持久化失败不影响本次批准，但不扩大未来授权。
- 页面只接收脱敏权威回执。保存等待中继续编辑时按提交基线合并，保留后来的输入为 dirty；角色即时保存也不会重建整页草稿。不同角色可并行合并，同角色陈旧版本要求刷新核对；只有明确加载建立新 boot_id，迟到旧响应不能覆盖较新状态。
- 密钥先写不可变钥匙串账户并核验，再提交引用；失败保留输入且不显示保存成功，启动发现未安全整理的当前明文时明确警告。详细安全/恢复/回滚范围见 [供应商与网络](./providers-and-network.md#2-api-key-与-key-池)。
- 请求结果不确定时按操作 UUID 查询回执；提交后的清理失败单独显示，不能把已提交当作未提交而重复执行。过期/未知操作要求刷新并人工核对。页面冲突保持普通草稿，不自动重放旧全量配置。
- `subagentModels` 明确归类为即时项，UI 别名映射到 `subagent_models`。角色选项只来自已保存模型，新加未保存模型须先顶部保存；删除模型会在同次提交把受影响角色归一为继承并返回提示。已运行子代理的 spawn 模型不受影响。

## 搜索与锚点

- 搜索框位于返回按钮下方并独占一行。非空查询时结果列表替代导航 tablist，不能同时渲染两套导航。
- `matchSettings(query, t)` 搜索显示名、直字符串 `keywords`、页名与组名；大小写归一、空白分词、**多词 AND**，按页序 → 组序 → 注册表原序稳定排列。空查询返回空结果，由界面恢复普通导航。
- 只搜注册的静态设置项，不搜具体供应商/模型、MCP 服务器或技能名称；不做拼音、模糊匹配或权重排序。旧词“工具与集成”仍是 MCP/技能项的搜索词。
- 搜索框使用 combobox/listbox/option 语义，`aria-activedescendant` 指向高亮结果。↑/↓ 从未选中态都落到第一项，两端停止；Enter 仅在已选中结果时跳转，焦点留在搜索框；Esc 先清查询。
- 每项使用 `.setting-anchor` 与精确的 `data-setting-id="<注册表 id>"`。跨页命中经过普通离开拦截，待页体渲染后滚动居中并高亮约 1.5 秒。重复点击同一项仍须重新定位；换项和卸载都清旧高亮及定时器。
- 搜索定位被“留在原地”或保存校验失败中止时，丢弃待定位项；保存成功或放弃改动后允许定位继续。
- 找不到锚点，或 `offsetHeight === 0` 且没有 client rect 时，退化为高亮页体。当前覆盖测试允许 `active_model_id`（模型编辑视图才有）与 `app.mcp_status`（依赖服务器数据）缺席；不要为规避测试随意扩充例外。
- 外部 `showSettingsAt(page, anchorId)` 通过一次性的 `ui.settingsHit` 定位。供应商卡片的 `providers.<uuid>` 是动态行级锚点，**不进注册表、不参与搜索**；与静态 `providers` 精确区分。消费后清空请求，并等草稿/锚点就绪，避免首次打开时落空或重开时重放。此通道由 store 直接打开/切页，不等同于搜索的离开拦截路径。
- **进阶折叠已移除**：没有页级开关、临时展开或 localStorage 展开偏好；注册表残留的 `advanced` 标记目前不影响渲染，不应据此恢复旧折叠行为。

## 共享视觉与各页边界

- `SETTINGS_VISUAL` 是设置区字号、密度和尺寸入口：内容最大宽度 960px，窄/中/宽控件基准 180/240/360px。使用 `SettingsThemeProvider` 的局部 antd `ConfigProvider` / `App`；Portal 弹框通过 `SettingsThemeScope` 继承公共样式与变量。
- `SettingsForm`、`SettingsFormItem`、`SettingsSection` 统一左右式行与带边框分组：标题/说明在左，控件在右，校验反馈靠近控件，关闭自动冒号。内容容器宽度不超过 680px 时变成上下排列；MCP 编辑器继续标签在上，窄键值行可堆叠。
- 宽度类 `.w-narrow` / `.w-mid` / `.w-wide` 都有 `max-width:100%`；局部页体适配由共享 CSS 收口。不要在页体增加像素内联宽度，或重新引入旧“内容只占右栏 50%”/所有表单强制 vertical 的规则。
- 字体使用 `--ws-font-sans` / `--ws-font-content` / `--ws-font-mono`，颜色走 `--ws-*` 与 antd token，不新增硬编码字体链或彩色强调色。外观页按语言、主题、字体排列；主题使用预览卡。字体分组三行（界面 / 内容 / 代码），每行左侧只标槽名、与右侧控件垂直居中且取**定宽窄列**（`SETTINGS_VISUAL.fontSlotLabelWidth`，不再是三档宽度档或半行宽），行内并排字体名输入与定宽字号 Select（`fontSizeSelectWidth`）、行末一个 `fontResetWidth` 宽的重置按钮（一按清本槽字体族与字号），下方一行预览。三个控件 `align-items: center` 等高：字号 Select 不得挂 `size="small"`（会比同行 Input 矮一截），重置不得再藏在控件的 suffix / suffixIcon 里。共性说明只写在分段标题右侧一次，不逐项重复。默认字号 14/14/12px；作用范围见[界面指南](./chat-and-interface.md#12x-字体与字号三槽)。
- 供应商当前是响应式卡片：标题为名称与协议，正文为端点和模型摘要（上下文/输出、图片/视频、active 标记），卡片底部编辑/删除。编辑先在局部表单暂存，不能退回旧“字段变化直接污染页级草稿”行为；字段校验与输出预算见 [供应商与网络](./providers-and-network.md)。
- 网络代理卡片的 `.proxy-mode-list` 必须显式 `font-size: var(--ws-font-size)`，变量由 theme bridge 桥接 antd fontSize。Radio.Group 自带 font-size:0，单靠 DOM 文本存在不能保证标题肉眼可见。manual 校验只看支持协议前缀，空串允许保存；界面默认显示系统代理不等于后端把 null 自动改为显式配置。
- MCP 当前是服务器卡片与编辑弹框，支持用户级/项目级、测试、断开/重连和工具说明。状态按作用域及服务器名匹配；刷新只重读状态。结构化编辑须保留未知字段，解析不了时保留原始 JSON 供修复，不能降成空列表覆盖原文。详细界面约束见下一节，后端入口为 [mcp.rs](../src-tauri/src/host/commands/mcp.rs)。
- 技能启停修改草稿；仅后端标记可删的托管技能提供删除，兼容目录/内置技能不可删。设置空态使用 `settings.skillsEmpty`，不得借用会话空态。
- 保留期修改走主配置保存；“立即清理”使用已保存保留期，有未保存保留期改动时禁用。旧格式历史清理必须保留没有可读新格式副本的旧文件，详见 [会话与项目指南](./sessions-and-projects.md)。
- 关于页的版本与检查更新并排且各有锚点；目录/仓库/许可证操作失败在触发行提示。版本、目录及许可证是可搜索的只读入口。写入后检查使用 `post_write_check.*` 四项，旧 LSP 配置和引导卡已经退役，详见 [写入后检查](./tools-and-safety.md)。

## MCP 页与编辑器

### 卡片和状态

- “用户级/项目级”使用作用域页签；无项目路径时项目级不可用，有未保存 MCP 草稿时不允许切换。服务器列表来自当前作用域配置，连接状态来自当前会话快照，按作用域和服务器名匹配；不能只按名称合并同名用户级/项目级服务器。
- 卡片标题行从左到右为名称、居中的作用域/工具数/PID、连接状态；窄卡元信息换到标题第二行。正文为已过滤且可供模型使用的工具名称/说明，不显示环境变量值或输入 schema。未连接、无工具、缺少工具详情和无说明各有占位。
- 只有说明实际截断时显示可聚焦问号按钮，hover/focus Tooltip 展示全文。错误可展开。连接绿、启动/淘汰橙、错误红、未连接灰；颜色不能替代状态文本。
- 非当前会话生效的配置卡片禁用断开/重连，防止按名操作误碰另一作用域。无活跃会话时快照为空，不沿用上个会话的连接状态；读失败保留已有快照并提示，不把失败伪装成“所有服务器未连接”。
- 断开/重连以“会话 + 作用域 + 服务器”在 UI store 记进行中操作：被点击按钮 loading，同卡两项互斥，失败释放；设置关闭再打开也不能重复发起。快照请求序号只接受最新结果，避免旧刷新覆盖新连接态。刷新只是重读，不触发重连。

### 编辑、保存和测试

- 新建/编辑使用一个弹框，包含基本信息与按 stdio/streamable_http 切换的连接字段；参数、环境变量和请求头使用紧凑键值行，窄窗口可上下堆叠，长值软换行且最多六行。
- 环境变量/请求头的常见凭证名称默认遮罩，可临时显隐；新增敏感值输入不中断，失焦再遮罩。增删行重置显隐状态，避免同名下一行继承上一行明文状态。遮罩只影响显示，不加密或改动落盘值。
- 保存并重连只处理 MCP 文档；后端 `saved:false` 时不移动已保存基线、不关闭弹框，保留输入。保存成功再更新基线、关闭，并在有会话时发起连接与刷新。取消恢复打开时的已保存内容。
- 删除是底栏左侧描边危险按钮，按钮旁 Popconfirm 的确认按钮使用危险色；取消/保存在右侧。删除从已保存文档移除条目后保存，不顺带提交弹框未保存字段。
- 原始 JSON 无法解析时仍保留原文修复入口，新建不能拿空列表覆盖它；结构化序列化保留未知顶层/条目字段。
- **测试当前草稿**：保存和测试共用条目序列化；IPC 发送完整 name/config，新建或重命名无需先保存。测试只临时启动命令/连接地址并 initialize/tools-list，不读取磁盘同名项、不改变正式池；未保存不代表没有启动副作用。单份草稿单飞，改字段、关闭重开或切上下文会丢弃旧结果。关闭编辑器不立即取消进程；测试总预算60秒，另加最多5秒回收。保存使用文件版本检查，冲突保留草稿；saved表示已写文件并接纳差量应用，Ready/Error仍由状态事件与快照确认。详见[MCP 与技能](./mcp-and-skills.md)。

源码：[SettingsPage.tsx](../ui/src/features/panels/SettingsPage.tsx) 的 `saveMcp/deleteMcpEntry/runMcpAction/testMcpServer/readMcpStatus`、[McpKvTable.tsx](../ui/src/features/panels/McpKvTable.tsx)、[McpStatusTable.tsx](../ui/src/features/panels/McpStatusTable.tsx)、[host/commands/mcp.rs](../src-tauri/src/host/commands/mcp.rs)。

## 新增或移动设置项

「界面 → 输出展示」注册为 `ui.output_display`，与聊天区顶部切换器共用 store 和 localStorage 偏好；默认详细，切换即时生效，不修改模型回复长度。简洁的聚合/文件统计/预览与历史边界由[聊天与界面](./chat-and-interface.md#52x-简洁与详细展示)主维护。

1. 在 `SETTINGS_ITEMS` 登记唯一 `id`、双语 `labelKey`、所属页、必要的分组及中英搜索词。`keywords` 是直字符串，不是 i18n 键。
2. 普通配置字段加入 `SettingFieldPath` / `PAGE_FIELDS`，并核对 `fieldSlice` 缺省折叠及实际保存路径。即时项加入 `INSTANT_APPLY_FIELD_IDS`；动作/只读信息使用 `app.*`，不进入 `PAGE_FIELDS`。已有复合项如 `subagentModels` 须同时核对真实字段和独立保存行为，不能只靠注册名推断。
3. 每项标注 `width`，或在 `WIDTH_EXEMPT_ITEM_IDS` 写明不设档的原因（整行文本、开关、Slider、复合容器、动作等）。两组不得重叠或漏项；项与页字段双向闭合，例外显式登记。
4. 页体挂对应锚点和公共宽度类，使用共享表单/分组。移动页面时同时迁移字段归属、脏点、深链和测试，不能只搬 JSX。
5. 中英文同步添加文案。跨页共用、语义相同的无宾语动作使用 `common.save/saved/cancel/delete/builtin`；设置专属文案放 `settings.*`，从属说明/动作在 `SHELL_SETTING_KEYS` 分类登记。带宾语的动作、不同空态及不同保存动作保留独立键，不为字面相同而跨段借键。
6. 维护引用闭包测试：设置面板使用 `settings.*` / `common.*` 或明确的文件级豁免；其他 feature 的跨段借用及纯动态 `t(...)` 调用按测试清单登记原因。清单不得悬空，已退役键不得复活。

## 验证与手工验收

相关自动化入口：

- [settings.registry.test.ts](../ui/src/__tests__/settings.registry.test.ts)：字段闭合、i18n 引用、搜索、宽度、锚点命名空间与术语。
- [settings.page.test.tsx](../ui/src/__tests__/settings.page.test.tsx)：覆盖层、运行不中断、脏状态、离开拦截、导航可达性、搜索定位与关于页。
- [settings.style.test.ts](../ui/src/__tests__/settings.style.test.ts)、[app.smoke.test.tsx](../ui/src/__tests__/app.smoke.test.tsx)：共享布局与挂接。
- [settings.mcp.test.tsx](../ui/src/__tests__/settings.mcp.test.tsx)、[settings.skills.test.tsx](../ui/src/__tests__/settings.skills.test.tsx)、[i18n.keys.test.ts](../ui/src/__tests__/i18n.keys.test.ts)：专页行为与双语键。
- [mcp.status-table.test.tsx](../ui/src/__tests__/mcp.status-table.test.tsx)、[mcp.kv-table.test.tsx](../ui/src/__tests__/mcp.kv-table.test.tsx)：卡片状态/工具说明与敏感值显隐；[proxyCard.style.test.ts](../ui/src/__tests__/proxyCard.style.test.ts)：代理标题字号；标题栏相关测试守全屏分界。

改前端后执行 `pnpm --dir ui test`、`pnpm --dir ui run lint`、`pnpm --dir ui build`；涉及保存/连接后端再跑 `cargo test`（src-tauri）。本文核对源码，没有把旧批次测试数量视为本次实跑。界面按仓库约定由用户手工点验，不把静态 CSS/DOM 断言写成 GUI 已验收：

1. 在运行中的会话打开/关闭设置，确认消息继续、滚动位置保留、运行指示可返回；标题栏拖动和窗口控制正常，覆盖后的分隔条不能获得键盘焦点。
2. 修改主配置，分别测试切页、返回、Esc 和关窗的三选；测试保存失败留页、放弃、顶部取消，以及退出“留在原地”后仍可再次退出。
3. 切换语言、主题、字体、自动更新，确认独立生效、无脏点且重开保留；单独验证子代理覆盖、MCP 保存与技能删除，不把它们当作普通可回滚草稿。
4. 中英文搜索静态项、旧页名与多词；检查无结果、方向键、Enter、Esc、IME、重复定位、快速换项、离开拦截和缺锚点降级。外部供应商跳转需精确落到卡片，关闭重开不重放。
5. 宽/窄窗口、浅/深主题查看十页与弹框，确认内容居中、控件及长路径不溢出、窄屏堆叠；检查模型卡片摘要、字体预览、技能长说明和危险操作位置。
6. MCP 验证作用域切换、同名服务器状态隔离、原始 JSON 修复、未知字段保留、保存失败保留草稿、取消不落盘、刷新与重连区别；连接操作进行中关闭重开也不能重复发起。
7. 清理先核对预览与确认内容；保留期未保存时立即清理不可用，旧历史唯一副本不可删。关于页目录/仓库/许可证入口成功可打开，失败在原行可见。
8. 工作区左栏拉到最宽/最窄和折叠后，打开设置/任务、最大化/还原、缩窄窗口，确认顶栏分界持续对齐，设置标题和窗口控制可见；回工作区恢复原布局。
9. 供应商弹框改字段/模型后取消，不应改变页级草稿；完成后尚需顶部保存。分别核对缺 key、删空模型、空 wire id、请求头保留名、manual 空串/非法前缀。浅深主题下肉眼确认三个代理模式名。
10. MCP 同名作用域隔离、工具说明截断问号、敏感值连续输入/失焦遮罩、同名行显示后删除、原始 JSON 修复、当前草稿精确载荷/迟到结果、跨会话状态及快照顺序、删除不夹带未保存编辑，都需要单独验证。

## 不应恢复的旧行为与待验证边界

- SettingsModal/通用页重置、六列 MCP 状态表、嵌套模型弹框、所有表单强制 vertical、内容仅占右栏一半、进阶折叠及固定跟随工作区 navWidth 的全屏分界，均不是当前契约。
- MCP 顶部主配置保存与弹框保存是两条路径；不能把通用“保存并离开”描述成同时保存所有独立文件。
- 子代理即时字段与页级旧草稿隔离、字体失败提示/重试和迟到回执已有自动回归，见[专项状态](./reliability-remediation-plan.md#实施与验收状态2026-10-12)及[本次测试记录](./testing.md#可靠性修复自动验证记录2026-10-12)。仍须按上文手验设置页保存/取消/重开；MCP 草稿握手成功不等于正式连接 Ready，字体持久化失败时本地预览可能暂时生效，“即时生效”不代表成功落盘。
