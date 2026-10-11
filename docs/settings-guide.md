# 设置页维护指南

核对日期：2026-10-11。本文按当前源码说明页面归属、保存边界、搜索与公共视觉契约，取代设置页分批实施报告。设置现在是 **10 页、3 组导航的全屏覆盖页**；所有设置项默认可见，不再提供进阶折叠开关。供应商、密钥、代理与额度详见[供应商与网络](./providers-and-network.md)，全局主题与窗口交互详见[聊天与界面](./chat-and-interface.md)。

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
| 主配置草稿 | 顶部保存全量提交 `ConfigState`，成功后留在设置页。供应商、网络、审批、技能启停、写入后检查、Shell/提示词/压缩、保留期和日志等走此路径 |
| 供应商弹框 | 新增与编辑均先保存弹框局部表单，确认/完成才合并到主草稿；取消丢弃局部改动，不等于已经落盘。模型编辑复用同一弹框，不叠第二层 Modal |
| 即时偏好 | 界面语言、主题、两种字体与自动检查更新独立生效，不参与脏点；取消主配置草稿不回滚它们 |
| 子代理模型覆盖 | `SubagentModelsPanel` 选择后直接调用 settings store 保存 `subagent_models`；不是等待顶部保存的普通草稿控件 |
| MCP 配置 | 独立 `mcp.json`，通过编辑弹框的“保存并重连”调用 `mcpSaveConfig`；顶部主配置保存不覆盖它 |
| 操作与只读信息 | 技能重扫/删除、清理、连接操作、打开目录与检查更新走各自 IPC；`app.*` 注册项没有落盘字段 |

- 普通脏点由 `PAGE_FIELDS` → `fieldSlice` / `pageSlice` 对比草稿与已保存配置；即时项和 MCP 从主配置比较中排除。MCP 另比当前序列化文本与基线，脏点只挂在 `mcp` 页。
- 比较归一化空值（`null`、`undefined`、空串、纯空白），对象键排序，过滤数组空条目；`proxy: null` 等价于系统代理默认配置。新增有缺省语义的字段时同步检查比较逻辑，避免“改回原值仍有脏点”。界面与关于页没有待保存草稿，不应亮脏点。
- 切页、返回按钮、运行中指示、页内 Esc 与关窗请求共用“保存并离开 / 放弃改动 / 留在原地”。顶部取消直接丢弃未保存草稿并返回工作区，无二次确认。
- 代理地址或供应商校验失败时不落盘、不离开，转到问题页并丢弃待执行的搜索定位。保存成功或放弃后执行原导航意图；留在原地时清除该意图。MCP 仍须按其独立保存入口处理，不能把通用保存当作 MCP 保存。
- 关窗的“留在原地”必须向后端 `respondExitRequest("cancel")` 应答，不能只清前端 store。设置草稿处置完毕后，运行中会话的退出确认才接管。
- 字体提交覆盖回车、失焦、停输约 600ms 和卸载，避开输入法组合态；本地应用/缓存与后端字体偏好持久化由 [fonts.ts](../ui/src/utils/fonts.ts) 和字体组件负责。
- 字体真源是 `config.ui.font_sans` / `font_mono`，localStorage 仅供 React 挂载前首帧应用。配置加载后以后端为准回写缓存；仅当后端两槽都空而缓存有值时尝试迁回后端。`set_font_prefs` 先保存快照再更新内存，页面保存保护这两字段，但独立保存与全量配置保存目前没有共同文件写锁，极端交错仍需验证。字体落盘失败仅 `console.warn`、本地暂时生效，不能承诺下一启动必定自动恢复（非空后端值会优先于缓存）。字体名只引用本机安装字体，不安装字体、不实现全局字号缩放。
- 后端页级保存先解掩码，并保留独立命令维护的两种字体；随后先写配置、再尝试 keyring 迁移、成功则以占位重写，最后更新日志级别/共享 HTTP client 并清 key 冷却。迁移失败可能保留明文，只记警告；不能把顶部成功提示理解为“钥匙串迁移必定成功”。
- 主配置保存是全量覆盖，独立保存项与页面旧快照需保持一致。当前子代理覆盖直接 `useSettings.save`，页面草稿不会随 store 自动重建，后端 `apply_page_save_shape` 仅专门保护字体：同次打开中“先改子代理覆盖、再保存另一项”存在旧 `subagent_models` 被写回的静态风险，需独立回归，不能把它宣称为事务化合并。
- 配置 `RwLock` 保护内存访问，`atomic_write` 保护单次文件替换完整性，两者不等于跨入口读改写串行化。不要从名称里的“原子写”推导出并发偏好保存会合并所有字段；字体提交也没有错误 toast、回滚或可靠重试队列。

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
- 字体使用 `--ws-font-sans` / `--ws-font-mono`，颜色走 `--ws-*` 与 antd token，不新增硬编码字体链或彩色强调色。外观页按语言、主题、字体排列；主题使用预览卡，两种字体各带对应预览。
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
- **测试按钮当前测试已保存配置**：IPC 只发 scope/name/session，后端 `mcp_test` 重新 `load_scope` 后按名取服务器，临时启动、列工具后回收，不并入正式连接池。尚未保存的新名字或字段不能视作已被该测试验证，这是当前界面的限制。

源码：[SettingsPage.tsx](../ui/src/features/panels/SettingsPage.tsx) 的 `saveMcp/deleteMcpEntry/runMcpAction/testMcpServer/readMcpStatus`、[McpKvTable.tsx](../ui/src/features/panels/McpKvTable.tsx)、[McpStatusTable.tsx](../ui/src/features/panels/McpStatusTable.tsx)、[host/commands/mcp.rs](../src-tauri/src/host/commands/mcp.rs)。

## 新增或移动设置项

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
10. MCP 同名作用域隔离、工具说明截断问号、敏感值连续输入/失焦遮罩、同名行显示后删除、原始 JSON 修复、已保存配置测试、删除不夹带未保存编辑，都需要单独验证。

## 不应恢复的旧行为与待验证边界

- SettingsModal/通用页重置、六列 MCP 状态表、嵌套模型弹框、所有表单强制 vertical、内容仅占右栏一半、进阶折叠及固定跟随工作区 navWidth 的全屏分界，均不是当前契约。
- MCP 顶部主配置保存与弹框保存是两条路径；不能把通用“保存并离开”描述成同时保存所有独立文件。
- 子代理覆盖与页级旧草稿的覆盖风险、MCP 测试不消费未保存字段、字体持久化失败但本地暂时生效等，应按源码实际边界排查；不要以“即时生效”推导出所有失败/并发路径都已同步落盘。
