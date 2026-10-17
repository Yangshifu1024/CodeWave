# 聊天与界面指南

本文记录当前聊天输入、转录展示、工具卡、窗口与通知的行为，以及修改这些功能时必须保留的契约。以当前源码为准，不把旧批次报告的验收数字或已被替换的界面方案当作现状。

相关主题：[会话与项目](./sessions-and-projects.md)、[运行与调度](./runtime-and-scheduling.md)、[计划与批准](./plan-and-approval.md)、[子代理](./subagents.md)、[供应商与网络](./providers-and-network.md)、[设置](./settings-guide.md)、[文档与文件预览](./documents.md)。

## 1. 界面结构与主题

### 1.1 导航、标题栏和右栏

- 左栏承载临时会话、项目与会话入口；工作区文件树和内联编辑器已移除。文件引用走 Composer 的 `@` 或附件入口，会话产物走右栏「文件」。左下角提供 git 身份及任务、统计、设置入口。
- 顶栏左段是与左栏同宽同色的背景带；右段承载 Logo 左栏开关、当前会话标题、目录与分支胶囊、右栏开关。目录胶囊显示工作目录 basename，悬停显示完整路径；临时会话显示对应标记；分支胶囊只读。旧的会话历史前进/后退按钮、双栈和 `Alt+←/→` 已移除。
- 设置、任务和运行总览打开时，标题反映覆盖页而非后台会话。设置页隐藏工作区两侧栏开关。工作区仍保持挂载，覆盖页的详情见各主题指南。
- 左栏和右栏折叠宽度均为 **0**，没有窄轨。左栏保留 antd `Sider` 外壳和内容，右栏保留 Tabs 实例；以宽度过渡裁切，避免折叠时把布局从横向变成纵向或销毁面板状态。右栏隐藏同时关闭其 Portal 查看弹框，防止弹框逃出隐藏外壳。
- 右栏顶部使用 antd Segmented 分段导航，顺序固定为「信息 / 日志 / 文件」，默认「信息」，切换保留挂载的 Tabs 内容。「变更」只保留信息概览中的入口，打开宽弹窗，关闭后回到之前的视图；`showChanges()` / `rbTab="changes"` 的现有外部调用仍有效。栏底使用 antd 布局色，内容由 antd Card 承载 12px 圆角与边框，卡片内边距 16px；折叠区为零圆角的直线分隔，展开箭头放在标题前，与聊天工具和思考入口一致。信息页顶部显示项目名、主目录及文件管理器/编辑器入口，下方为变更、当前会话子代理、会话文件摘要；会话文件、计划、额度与余额、技能为可折叠区，复用同一 antd Collapse；通过语义样式将内容顶部内边距设为 0、底部保留 14px，避免 ghost 默认 paddingBlock 叠加标题与内容之间的留白。会话文件默认展开，和技能、计划、额度区一样记忆折叠状态；计划仅在有 todo 时出现。技能名占可收缩的主列，来源在固定宽度槽位对齐；技能列表不显示 hover tooltip，点击技能行在详情弹窗查看完整名称、说明及来源路径。数据目录入口仍在设置「关于」。
- 变更摘要与 [ChangesModal](../ui/src/features/workspace/ChangesModal.tsx) 共用 [useWorkspaceChanges](../ui/src/features/workspace/useWorkspaceChanges.ts) 的同一份 `git_diff` 结果；只有右栏展开且位于信息或请求查看变更时才读取，信息→弹窗不重复查询。进入/切会话及手动刷新会查询，不增加持续轮询；请求单飞，隐藏、换会话或卸载后的迟到响应被丢弃。初次加载与失败不显示虚构的 `+0/−0`，同会话刷新失败保留上次成功结果并明确报错，切换会话同步屏蔽旧摘要。
- 临时会话以 `Tab.projectId` 缺失判定，信息页隐藏「项目目录」标签、路径及文件管理器 / 编辑器目录入口，保留临时会话身份与说明；不能把全局工作区当作项目目录展示。切回项目会话恢复目录区域，其余右栏功能不受影响。
- 变更弹窗宽度为 `min(1200px, 视口宽度−48px)`，左侧按根目录分组选择文件，右侧只渲染选中文件的完整文本 diff；长行保留原始换行并支持横向滚动，文件列表与 diff 各自有界滚动。空变更、无会话、没有文本 patch、加载、失败与后端 `truncated` 标记分别显示对应状态，不把没有文本 patch 伪装成无变更。关闭后同会话保留文件选择；换会话清空选择并关闭弹窗，隐藏右栏也关闭弹窗且不在重开时复活。原来的窄栏全量 diff 展示已移除。
- 子代理摘要仅消费当前 Tab 的 `subs`，不另拉跨会话运行总览。点击条目调用原 `openSubDrawer(sessionId, subId)`；`partial` 单独标记需注意，`budget/no_report` 与 error 标为异常结束，不能混入正常完成数。error 桶仍包含现有聊天态未区分的失败/停止，详见[子代理](./subagents.md#ended-四象限)。会话文件摘要与文件页签共用产物边车列表和扩展名图标：Python、代码/脚本、Markdown、图片、表格、Word、PDF、演示、压缩包、文本，未知或无扩展名使用普通文件图标；匹配不区分大小写，仅用于展示。摘要展开时显示最近三项；标题与箭头负责折叠/展开，右侧数量按钮切换到「文件」Tab，点击数量不改变折叠状态，不再显示「查看全部」；原地引用附件不属于该列表，不新增上传/来源管理。删除项不可预览，文件页签的文档回退入口保留；文件与技能查看弹层均绑定打开时的会话，并随换会话/隐藏右栏关闭，打开变更弹窗时关闭其他右栏查看弹层。
- git 身份是本地只读配置读取，仓库配置优先、失败回退全局配置；头像由邮箱/名字本地生成，不请求远程头像服务。不要把该入口扩展成隐式 git 写操作。

宽度真源在 [layout.ts](../ui/src/utils/layout.ts)：左栏默认 280、范围 180–480；右栏默认 328、范围 328–640；中栏目标保底 480。窗口缩窄时先压右栏再压左栏，**只夹显示值，不回写用户记忆宽度**。分隔条支持拖动、方向键 16px 微调、双击复位；显示受夹取时禁止拖动，仍可复位。标题栏、Sider、右栏必须消费同一套显示宽度，不能各自计算。布局最小值不意味着任意异常尺寸下中栏绝不会被挤压。

**拖动期必须禁选**：分隔条按下后浏览器会落一个选区锚点，而选区延展不跨 flex 块级边界设障，于是 pointermove 扫过的路径上的文字全部进选区，表现为「拖完整个窗口全是高亮」且松手后残留（连 Composer 输入框都会被扫进去）。因此拖动期由 `<html>` 上的 `ws-resizing` 类（`ResizeHandle` 在 `pointerdown` 挂、`pointerup`/`pointerCancel`/`onLostPointerCapture`/卸载兜底摘）对**整棵子树**加 `user-select: none !important`——只禁选分隔条自身不够，子选择器 `html.ws-resizing *` 不能省。作用域仅限拖动这一瞬间，松手后正文的正常选中与复制不受影响。回归入口：[layout.resize.test.tsx](../ui/src/__tests__/layout.resize.test.tsx)（CSS 文本契约断言，happy-dom 不解析外链 CSS）。

实现入口：[AppShell.tsx](../ui/src/features/shell/AppShell.tsx)、[TopBar.tsx](../ui/src/features/shell/TopBar.tsx)、[RightBar.tsx](../ui/src/features/shell/RightBar.tsx)、[ResizeHandle.tsx](../ui/src/features/shell/ResizeHandle.tsx)。

### 1.2 主题、字体与可见性约束

主题有「跟随系统 / 亮色 / 暗色」三档，偏好同步读 localStorage；`App.tsx` 的 `effectiveDark` 是 antd 算法、前景 token 和根元素 `dark` class 的统一来源。应用 chrome 的主色保持中性墨色：亮 `#1f1f1f`、暗 `#424242`；`--ws-accent` 取 **`colorPrimaryText`**，适用于文字、图标、边框和流光，不能直接当作暗色白字按钮的背景。

当前亮色导航底是 `#eef2f5`，主内容底是 `#FFFFFF`；右栏底消费布局 token `--ws-bg`（亮色 `#f5f7f9`），卡片背景由 antd 容器 token 管理，暗色随同一主题算法变化。不要复用旧报告中的 `#ececee`、`#f8f8f8`。通用控件 / 导航选中态使用 6px 圆角，卡片与 Composer 使用 12px；自定义容器通过主题桥的 `--ws-radius` / `--ws-radius-lg` / `--ws-shadow` 消费 antd token。亮度判定集中在 [colors.ts](../ui/src/theme/colors.ts)，必须同时接受 `#000`/`#fff` 与六位 hex；主题桥、代码高亮和 HTML 预览须同判据，避免暗色文字配亮色背景。

- 字体分为界面、内容、代码三槽，分别消费 `--ws-font-sans` / `--ws-font-content` / `--ws-font-mono`；antd 的 `fontFamily` / `token.fontSize` 跟随界面槽，`fontFamilyCode` 接代码槽。用户字体名经过清洗，再与各自默认回退链组合。字号默认 **14 / 14 / 12px**；antd 6 的 `font-size: 0` 容器问题见下节，保存与失败边界见[设置指南](./settings-guide.md)。
- 控件由 antd 6 接管；自定义样式消费主题桥的 `--ws-*`，不要重新引入默认蓝主色或多套调色板。菜单选中底使用中性半透明填充，悬停选中再加深；`controlOutline: transparent` 去除输入控件双重外晕，聚焦仍以边框等既有反馈表示。

#### 1.2.x 字体与字号三槽

每槽独立保存字体族与字号，配置为 `config.ui.{font_sans, font_content, font_mono, font_size_sans, font_size_content, font_size_mono}`，WebView 的 `localStorage` 只作首帧防闪缓存。设置页按「**界面 → 内容 → 代码**」三行排列，每行左侧只标槽名（定宽窄列，与右侧控件垂直居中）、行内并排放字体族输入与定宽字号 Select、**行末一个重置按钮**，下方一行预览。三个控件等高——字号 Select 不挂 `size="small"`（会比 Input 矮一截、下边线对不齐），重置也不再藏在两个控件的 `suffix` / `suffixIcon` 里。字号为连续 **12–20px** 整数；默认及还原值是 **14 / 14 / 12px**。已有合法显式值保留，新增内容字段缺失时默认 14px；还原只清除本槽缓存覆盖并同步本槽默认值。

行末重置**一按清两样**（字体族清空 + 字号回默认档），因为两者同属一槽、分开两个按钮只会让人猜哪个管哪个；实现上顺序调用 `commitFontSlot` 与 `commitFontSizeSlot`（两者都基于 `readStoredFonts()` 叠加、返回全量六字段），但**只提交最终结果一次**，避免在串行提交队列里连发两轮。

合并成三行是因为同槽字体族与字号原本各带一个预览行、而两者样本完全相同（同字体族 + 同字号），属纯重复。**两个控件的锚点仍各自独立**（`ui.font_<slot>` / `ui.font_size_<slot>`），设置搜索能分别定位。公共说明只在「字体」分段标题右侧说一次（`settings.fontHint`），不再逐项重复：字号范围、槽间独立性、默认值与生效方式都由控件本身、当前选中值、还原按钮和预览行体现；唯一无法自明的是输入格式（逗号分隔多个、留空用默认），保留在这行说明里——注意是**即时生效**（回车 / 失焦 / 停手约 600ms / 卸载四条路径都会提交），不是「需按回车」。

界面槽覆盖标题栏、导航、设置、输入控件、工具标题、状态和元数据；内容槽覆盖用户/助手正文，以及子代理、计划、技能、发布说明、Markdown 与纯文本预览中的正文。代码槽覆盖行内代码、代码块、源码预览、diff 与日志，包括内容中的代码。按钮（含正文内复制按钮）仍取界面槽。正文、代码基础字号直接取对应槽；标题和辅助文字保留相对层级，界面/内容偏移基准各为 14px、代码为 12px。三槽不互相继承已选字体或字号，不添加统一 +1/导航 +2 补偿；图标、布局尺寸、图片、PDF 与文档自身排版不随字体设置整体缩放。

`fonts.ts` 是三槽字号 CSS 变量的唯一写入方，`native.css` 提供默认值。`App.tsx` 订阅已应用的界面字号；首帧使用缓存，选择立即生效。六字段经同一串行队列和后端 Fonts patch 原子保存；后端回执只更新持久化状态，迟到旧回执和保存失败不回退新预览，失败可同值重试。启动配置加载后再对账；缺省字号不阻止既有缓存字体族的导入。前后端同用 12–20 整数白名单；未知配置数值按槽回落 14/14/12，非法保存请求拒绝。旧缓存语义档位仅接受明确键，继承属性名等非法缓存不能生成 NaN。字体与字号不递增页级草稿版本，普通设置保存保护全部六字段。

antd 6 把 `.ant-radio-group` 等容器设了 `font-size: 0`（消除 inline-block 间隙），挂在其中且未显式声明 font-size 的自定义内容会继承成 0 号字（DOM 里有文本、肉眼看不见）；需要还原字号的界面容器统一取 `--ws-font-size` / `--ws-font-size-sans`。ThemeBridge 仅从当前 antd token 桥接 `--ws-font-size`，不覆盖三槽偏好。回归入口：`font.sizes.test.tsx`、`fonts.test.ts`、`font.settings.test.tsx` 和配置提交器的即时字段测试。真实视觉、跨平台字体回退与应用重启仍需手动验收。
- 当前权限按钮与菜单文字均为中性，旧报告的蓝/橙/红和中途绿/黄/红映射已退役。风险/用量类内容可用语义色：工具等待橙、错误红、上下文进度圈四色；代码高亮和身份头像不属于品牌强调色。
- 帮助说明、错误原因、限制条件应靠近对应控件常驻显示，不能仅靠 placeholder、低透明度或远端标签表达。「即时生效」放在标题旁；动作按钮靠右与信息被推远是不同问题。
- `prefers-reduced-motion` 下流光隐藏、思考滚动与等待指示降动效；新动效必须保留静态可读结果。

实现入口：[App.tsx](../ui/src/App.tsx)、[bridge.tsx](../ui/src/theme/bridge.tsx)、[fonts.ts](../ui/src/utils/fonts.ts)、[app.css](../ui/src/theme/app.css)。

## 2. Composer 输入、草稿与附件

### 2.1 每个会话的独立草稿

草稿结构是 `{ text, images, refs }`，放在 run store 顶层的 `drafts[sessionId]` 平行分片，**不放进 `TabRunState`**。这样击键只更新 Composer 订阅，避免每次输入触发整个聊天区、右栏和抽屉重渲染。

切换 Tab 保留各自草稿并清理旧菜单/历史浏览状态。发送先捕获不可变快照并显式路由到原会话；后端受理后仅消费该快照：正文/refs 的修订未变才清空，图片只移除已发送 id。等待期间新输入、追加引用或读完的图片保留在原草稿，失败不消费，重复受理幂等。等待期间正文有任意修改时完整保留新正文（可能含已发送前缀），不猜测文字差分。异步发送、队列编辑、文件引用的目标会话必须在发起时捕获，不能 await 后再读 `activeKey`。ask 出现时只渲染 AskPanel，暂时覆盖 Composer 与队列，回答后恢复草稿。

**草稿已不是仅内存态。** 现场状态通过 [uiState.ts](../ui/src/utils/uiState.ts) 写 `~/.codewave/ui-state.json`，包括文本、引用、受预算限制的图片、队列、滚动锚点等；关闭有未发送内容的 Tab 会给保留/丢弃选择。保存为 1.2 秒防抖加 2 秒最长等待，退出应答前强制 flush；写入失败会提示并保留重试机会，不能承诺强杀零损失。草稿与队列图片总 base64 超过 3,000,000 字符时，快照整批去掉图片、保留文本与引用，以避开 UI 状态文件 8MB 上限。这个上限**不是聊天历史上限**；完整历史规则见[会话与项目](./sessions-and-projects.md)。

覆盖和追加应保持区别：

| 入口 | 当前行为 |
|---|---|
| 用户消息「修改」/ `ws:composer-fill` | 覆盖正文、refs、图片；无图片就清空旧图；聚焦末尾，不自动发送 |
| 队列「编辑」 | 覆盖正文与 refs；队列项有图时回填，无图时保留现有图片 |
| 历史召回 | 回填正文、引用与图片；退出浏览还原进入前完整草稿 |
| 技能详情 `ws:composer-insert` | 空格分隔追加正文，保留原 refs 和图片，不自动发送 |

附件导入在粘贴/拖放事件或打开原生选择框之前捕获 `{ sessionId, instanceId, attachmentEpoch }`，每个 await 后验证身份；切 Tab 不取消 A 的读取，关闭/重开同 id、主动清空和整份图片替换令旧结果失效。图片追加在 store 中原子合并最新数组：最多 4 张、单图 5 MiB、base64 总长 20 MiB；张数截取本批，总量超限拒绝本批新增而保留原图。单图删除不取消其他在途导入。多个外部目录询问串行排队，目标失效自动以取消收尾，不继续放行或读取。

发送时仍在读取的图片会明确提示保留在草稿，不补入已发送消息；合法在途读取不会被快照消费取消。身份、在途读取及发送票据仅在内存中，不写 ui-state。回归入口为 `run.attachments.test.ts`、`composer.attachments.race.test.tsx`，原生 WebView/文件选择框的实机行为仍需按人工清单验证。

chip/图片不在 textarea 文本中，任何覆盖链路只调用 `setText` 都不完整。外部改写还需作废触发片段并同步光标。

### 2.2 快捷键和光标触发

- `Enter` 发送，`Shift+Enter` 换行。运行中有正文或引用时提交入队；运行中空输入的按钮是「停止」。当前 `send()` 要求合成正文非空，因此**只有图片、没有正文或引用时不能发送**。
- 输入框 placeholder 仅说明用途；`/` 技能、`@` 文件、`$` 子代理和发送 / 换行快捷说明常驻输入卡片下方，窄栏可换行。附件、权限菜单、子代理运行入口、上下文进度圈、速率、模型、力度以及发送 / 停止控件继续保留；模型仍显示真实名称，空间不足时省略且悬停保留供应商与完整名称。
- `Shift+Tab` 只在输入框内循环：**计划 → 逐项确认 → 自动编辑 → 完全访问 → 计划**，与权限菜单顺序一致，菜单打开也可切换。走 `updatePrefs` 乐观更新及失败回滚。权限与运行中切档的安全语义见[计划与批准](./plan-and-approval.md)。
- 纯文本空输入按 `↑` 进入已发送用户消息历史；浏览中 `↑/↓` 翻页，最旧处不环回，越过最新或 `Esc` 恢复进入前草稿。编辑召回内容会退出浏览，失焦不退出，避免点发送前文本被还原。
- 召回与历史恢复共用 [internalHints.ts](../ui/src/utils/internalHints.ts) 判定内部提示：完整保留的提示标签、运行器逐字生成的 `[system]` 批准 / 权限切档消息不作为用户草稿召回；恢复时显示为 notice，已加载的旧 user 项也在召回入口过滤。只有纯文本的完整模板命中才过滤，普通 `[system]` 文本、引用提示的提问及带图用户消息保留；不删除落盘消息、不修改 provider 内容或权限状态。
- 菜单打开时 `↑/↓` 导航、`Enter` 选中；`Tab` 选中目前只接在 `@` 和 `$` 菜单，不能笼统写三个菜单都支持 Tab。
- IME 组合期优先放行，守卫包含 `isComposing`、`keyCode === 229`、composition 事件记录；特别防 WKWebView 的中文确认 Enter 被误当发送。

`@` 是文件/上下文提及，`/` 是技能，`$` 是内置子代理。检测和回填只看**光标之前的片段**：`@` 必须位于消息开头或空白后；`/`、`$` 必须从消息 offset 0 开始，这是模型点名提示词的配套约束。邮箱 `me@x.com` 不触发；正文中间的 `/`、`$` 静默不弹。菜单固定在输入框上方，不跟随光标。

回填用当前文本、真实光标重新检测，只替换 `[片段起点, 光标)`，后半段正文不动。`onChange` 之外，点击、选择、方向键也要同步 caret；fill、召回、队列编辑、发送清空必须作废旧触发片段。`+` 菜单插入符号同样以光标为锚。禁止回到锚定全文末尾的正则。

实现入口：[Composer.tsx](../ui/src/features/chat/Composer.tsx)、[composerTriggers.ts](../ui/src/features/chat/composerTriggers.ts)、[useComposerHistory.ts](../ui/src/features/chat/useComposerHistory.ts)、[useComposerEvents.ts](../ui/src/features/chat/useComposerEvents.ts)。

### 2.3 文件原地引用与图片传输

附件按钮走原生选择框，拖入走 Tauri 文件路径事件，二者拿到真实路径。普通文件只记录 refs，chip 显示文件名、悬停完整路径；不复制文件、不预读全文。发送前由 `mergeRefs(text, refs)` 去重并把 `@路径` 追加到正文末尾；仅有 refs 也能发送。引用路径本身仍会占少量文本上下文，**文件内容**只在实际读取时进入工具结果。

- 图片才读取 base64 上 wire：单张不超过 5MiB、最多 4 张、base64 合计不超过 20MiB。粘贴支持图片；纯文本保留浏览器默认粘贴，其他文件提示用附件或路径引用。剪贴板 files/items 双通道去重。
- 当前模型 `vision` 是布尔能力项，不支持图片的模型会在发送前被拦截；旧报告「未声明 vision 也放行」不适用于现行配置。
- 项目外路径先由后端判断，再询问「仅本次 / 始终允许 / 取消」，放行后重新判断并取得规范引用。项目边界与持久授权详见[工具与安全](./tools-and-safety.md)。
- `@` 第一级根目录候选继续写 `@目录/ ` 供补路径；文件候选进 chip。搜索接口的结果没有目录标志，因此搜索命中目录仍按文件 chip 处理。
- Windows `\\?\C:\…`、`\\?\UNC\…` 只在展示/引用出口去前缀，内部 canonical 安全比较不变；不能改成跨根短名以求好看，否则同名路径可能解析到错误目录。

历史、队列与修改三种回填都用 `recoverRefs`：只有解析后重新合成**逐字节等于原文**才抽成 chip。句中引用、独占行、含空格路径等不满足门禁时保持原文，宁可不美化也不改写用户内容。当前 `@` token 对含空格路径仍有歧义，不能把 UI chip 写成协议级转义已解决。

实现入口：[composerRefs.ts](../ui/src/features/chat/composerRefs.ts)、[useComposerAttachments.ts](../ui/src/features/chat/useComposerAttachments.ts)、[pathutil.rs](../src-tauri/src/tools/pathutil.rs)。

## 3. 模型、上下文与生成指标

模型按供应商分组，会话覆盖优先于全局 active 模型；模型名 tooltip 保留供应商和完整名称。推理力度有默认/低/中/高/最高；协议映射、输出截断与思考预算见[供应商与网络](./providers-and-network.md)。

当前工具条将上下文与缓存命中放在进度圈的 hover Popover，内含用量/窗口、命中分子分母、有效压缩阈值与压缩按钮；**速率仍直接展示在工具条**。卡片实测宽度低于 600 为 narrow，600–820 为 medium，至少 820 为 normal：中窄档隐藏供应商名，窄档进一步隐藏模式/力度文字；模型名空间不足时省略并以 title 兜底。速率不能随整块 `.toolbar-info` 被隐藏。不要继续写旧版三段长文本同时排列或窄窗按 `<wbr>` 折行的布局。

### 3.1 三类数据不可混用

| 指标 | 数据与生命周期 | 口径 |
|---|---|---|
| 上下文 | 当前 `breakdown` | total/context_window；达到阈值红，阈值的 70% 起橙、50% 起黄，其下绿；无 breakdown 显示空态 |
| 缓存命中 | `TabRunState.usage`，按 Tab 跨 run 累加、不持久化 | Anthropic = read / (input + read + write)；OpenAI Chat/Responses = read / input |
| 生成速率 | `runMetrics`，新 run 清零、每步 usage 后更新、结束保留终值 | 本轮有效 output / 各步有效生成耗时之和 |

缓存语义从生效模型的协议解析，与统计面板共用 [models.ts](../ui/src/utils/models.ts) 的 `cacheDenominator`。无可解析协议或分母零时不显示命中，不能猜公式或伪造 0%。usage 必须在 `!running` delta 早退**之前**累计，否则迟到的最后一步 usage 会丢；usage 不创建转录项。跨协议切模型后累计数据没有按协议分桶，读数应理解为当前实现的会话级近似，不能当作跨协议财务核算。

速率依赖后端可选 `duration_ms` / `ttft_ms`，不按字符数估算。只有 `duration_ms > 0` 的帧同时进入 output、耗时与步数；字段缺失时整步排除，不能多分子少分母。计时锚在 provider **每次尝试内部**，成功尝试的重试退避不进入生成时间；空响应已发 usage 后重试的尝试仍按现行计数累计。

TTFT 认首个任意增量，含 thinking 和工具增量；工具条取本轮首个有效 TTFT。主会话工具卡耗时单列，排除工具执行和审批等待对 tok/s 分母的污染；子代理内工具不能再累计一次而双计。子代理、压缩、命名、计划任务的无计时记录不进入工具条速率。取消/失败仅保留已收到 usage 的部分；前端重挂载不回放旧指标，等下一帧。

统计总览的速率、均步耗时、平均 TTFT 必须按 `by_kind` 中 `gen_ms > 0` 的桶聚合，同域取分子和分母；按 `by_model` 会混入无计时的子代理等 output 而虚高。历史混版同日同来源桶仍无法在查询时剔除无计时子集，这是聚合粒度限制。

实现入口：[runFrames.ts](../ui/src/stores/runFrames.ts)、[composerMetrics.ts](../ui/src/features/chat/composerMetrics.ts)、[TokenStatsModal.tsx](../ui/src/features/panels/TokenStatsModal.tsx)、[stream.rs](../src-tauri/src/core/agent/stream.rs)、[stats.rs](../src-tauri/src/core/stats.rs)。

## 4. 流式转录与恢复不变量

### 4.1 顺序、等待指示与重试

正文、思考、工具与子代理锚点按 timeline 顺序显示。后端 64ms 节流以有序 `Text/Reasoning` 段缓存，同 kind 相邻合并，跨 kind 不重排；不能退回两条无序字符串再让前端猜时序。主会话的步骤正文先作为候选保留，按[整轮过程折叠](#44-任务完成后的整轮过程折叠)的确认顺序释放；思考、工具、询问和子代理状态仍即时显示。装配与落盘保留块序，保存路径保留 thinking；各协议是否回传 thinking 由供应商层决定。

同一 Tab **至多一个 streaming assistant 项**，且位于当前列表末尾。notice 插队后新建流式项前必须关闭此前所有流式项；done/error/cancelled 和终态对账必须扫全 Tab 收尾，不只关闭最后一项。简洁展示在消息末尾保留一处次级文字活动提示，按真实状态显示「正在思考 / 正在处理 / 正在回复 / 等待确认」；工具、过程组和子代理使用静态图标与状态词，不再逐层转圈。详细展示的 `LoadingOutlined` 仅在当前消息没有明确的工具、子代理或思考状态时兜底，表示仍在输出而非进度百分比；禁止重新引入全局 `.cursor` 闪烁类。

重试通过 `streamGen` 隔离旧 attempt；旧代次帧不能回填已清场的时间线。Tab/run 生命周期另有 `runEpoch` 与 run 身份守卫，不能与 provider 流代次混为一个计数。迟到 delta 不得在结束后重建流式项，usage 则遵守上一节的独立计数规则。终态丢失恢复与队列调度详见[运行与调度](./runtime-and-scheduling.md)。

### 4.2 正文、思考与 Markdown

- 已释放的正文只有当前末尾流式文本段逐字揭示，游标留在组件局部 state，绝不按动画帧写 store；未确认的步骤候选正文不提前渲染。历史、非尾段、结束/取消和减弱动效下直接显示全文；当前会话刚完成的权威最终答复使用下一节的快速揭示。这样不会把聊天滚动和图表升级推到 60fps。实现中的速度追赶是显示效果，不改变保存文本或 provider 内容。
- 思考默认收起，展开状态不持久化。运行中标题旁展示最新非空行：先左对齐停留、溢出才向左移动，只有真正换行才上翻，同行增量不重新翻页；减弱动效下静态显示。展开思考或工具卡视作阅读意图，暂停外层自动跟随。
- 子代理 `<report>` 标记仅在子代理显示路径剥离，先合并/剥离再逐字切片；不能对主聊天所有正文全局清除，用户可能正在讨论该标记。
- 用户气泡使用保守 Markdown：数学保留可见源串、mermaid 当代码显示；助手/计划/文件 Markdown 才进入图表与公式升级。外部链接统一经 `open_url` 打开系统浏览器，避免 WebView 的 `_blank` 无动作。
- 聊天和子代理正文对「短纯文本标题粘连 `1. **条目标题**`，后面紧接相同编号标记的第 2 条有序列表」补上块间换行。修正基于 block tokens 确认后重解析，仅改变展示；原历史、provider 内容、用户消息和文件预览不改。代码围栏、缩进代码、引用示例、合法版本号和没有后续列表的歧义文本不推断修复，原始 HTML 仍关闭。回归：[markdown.repair.test.ts](../ui/src/__tests__/markdown.repair.test.ts)、[chat.process-display.test.tsx](../ui/src/__tests__/chat.process-display.test.tsx)。
- 宽表由 Markdown renderer 实际生成 `.table-wrap`，CSS 赋横向滚动；测试必须同时守 DOM 与 CSS，不能只测试一段无人消费的样式。文件预览使用独立 `.viewer-md.md`，不能套聊天 `.assistant` 的裁切/行长约束；详见[文档指南](./documents.md)。
- 围栏代码块由 renderer 的 `fence` 规则在完整 `<pre><code>` 外包一层 `.code-wrap`，复制按钮与唯一的 `pre` 同级；不能在 `highlight` 回调中返回以 `div` 开头的包装，否则 markdown-it 会再套一层 `pre/code`。mermaid/数学围栏直接生成独立占位块，不进入代码框；高亮与原文转义、复制载荷保持一致。
- 代码块保留原生横向滚动与拖动；`.code-wrap > pre` / `.code-block` / `pre.hljs` 使用薄滚动条、透明轨道和主题滑块，悬停增强对比，WebKit 宽高为 6px。规则不覆盖主聊天 overlay 或思考正文的隐藏滚动条，也不修改长行、代码复制或 diff 的 `white-space: pre`。系统「始终显示滚动条」偏好与跨平台实际厚度、拖动行为仍需人工验收。
- 聊天区域左右各留中间栏宽度的 5%，内容占中间栏的 90%；正文、Composer、审批区和消息队列共用 `--ws-chat-gutter` 保持对齐。段落与列表间距保留阅读停顿，紧凑列表也继承正文行高；行内代码用 antd 淡填充 token `--ws-code-bg`，代码框颜色使用主题桥的 `--ws-code-block-*`。这些样式不改变数据或分页语义。流式图表占位文案走 i18n，渲染缓存键包含语言相关提示文案。

实现入口：[segments.tsx](../ui/src/features/chat/segments.tsx)、[markdown.ts](../ui/src/utils/markdown.ts)、[throttle.rs](../src-tauri/src/util/throttle.rs)、[dto.rs](../src-tauri/src/provider/dto.rs)。

#### 4.2.x 复制到剪贴板

复制是**跨界面全仓统一行为**，入口只有 `copyText`（[clipboard.ts](../ui/src/utils/clipboard.ts)），任何复制按钮都不得自写 `navigator.clipboard`。

三级链路逐级降级，最终返回布尔：原生插件（[ipc/clipboard.ts](../ui/src/ipc/clipboard.ts)，Tauri 主路径）→ 浏览器 `navigator.clipboard.writeText`（vite dev / 单测环境）→ 隐藏 `textarea` + `document.execCommand("copy")` 末级兜底。

**为什么主路径不能是浏览器接口**：打包版前端跑在 `tauri://localhost` 自定义协议下，该来源下 `navigator.clipboard` 不可靠。这是「用户消息复制按钮点击无反应」的根因。旧的五处写法（`navigator.clipboard?.writeText(t).then(...).catch(() => {})`）在 clipboard 缺失时 `?.` 短路得到 `undefined`，紧接的 `.then` **同步**抛 `TypeError`，而空 `.catch` 又把它吃掉——两条失败路径在界面上都表现为「点了没反应」，无任何痕迹。

**两条反模式不得重犯**：① `navigator.clipboard?.writeText(t)` 后直接 `return true`——`await undefined` 会被当作成功，于是既没写入又报成功，把失败伪装成成功；正确做法是先取 `clipboard` 判存在再写。② 失败后空 `catch`／`.catch(() => {})` 静默——**失败必须有区别于成功的可见反馈**（用户消息与代码块切失败图标，toast 类入口给 error 提示）。

后端依赖 `tauri-plugin-clipboard-manager`，`lib.rs` 注册插件，`capabilities/default.json` 只授 `clipboard-manager:allow-write-text`；官方默认权限集为空，读/图片/html/secret 一律不开。回归：[clipboard.util.test.ts](../ui/src/__tests__/clipboard.util.test.ts)（含全仓 `navigator.clipboard` 唯一性守护）、[codecopy.test.tsx](../ui/src/__tests__/codecopy.test.tsx)、[usermsg.actions.test.tsx](../ui/src/__tests__/usermsg.actions.test.tsx)。真实 WebView 下的剪贴板行为与三平台差异需在**打包版**手工验收，`tauri dev` 通过不算数。

### 4.3 时间、工具出参与文本 ask

消息首次进入转录时才盖 `created_at` UTC RFC3339 时间，保存/重载不能为旧消息补当前时间。用户与助手消息均移除整条身份行（「你」「CodeWave」及日期时间），正文直接显示，不保留空行或时间悬停提示；时间数据继续用于轮次边界与阅读锚点。任务过程保留总耗时，工具与思考保留各自耗时。历史恢复不重播逐字动画。

思考计时仅适用于当前仍在 streaming 的时间线尾段；已记录的 `durationMs` 保持定格，结束或恢复后不能继续增长。主会话与子代理历史重建使用 `appendDelta` 的 `restored` 分支，只合并文本、不写入 `startedAt` 或伪造 `durationMs`。当前历史 thinking 仅持久化文本，真实耗时无法从恢复时刻推算，故缺失耗时显示静态「思考过程」；遗留内存中的 `startedAt` 也不能绕过 streaming 判据。回归入口：[thinking.history.test.tsx](../ui/src/__tests__/thinking.history.test.tsx) 与 [run.interleave.test.ts](../ui/src/__tests__/run.interleave.test.ts)。

历史模型侧工具文本可能已瘦身；无法解析的出参通过完整 `ToolOutcome` sidecar 批量回填。持久键是 **provider `tool_use.id`**，不是实时 `{batch}:{index}`。主工具卡、子代理卡及子代理过程流共用回填判据；内容不能写回模型历史或再次出网。子代理恢复卡从合成 key 改成真实 sub id 后才能继续打开过程流。存储、回填与删除细节见[会话与项目](./sessions-and-projects.md)。

文本 ask 兜底只接受主会话、ask 可用、当前回合无真实调用、每 run 一次，并要求末尾独占完整块、非代码围栏、结构有效；安全语义完全复用正常 AskTool，详见[计划与批准](./plan-and-approval.md)。后端剥掉历史中的块后，前端仍需在 `ask:opened.text_recovered` 到达时及每个后续 text delta 后幂等剥离，因为节流尾帧可能晚于 ask 事件。跨 thinking 段拆开的块不拼接剥除；块外端点控制 token 不猜测清洗。当前 `settleRun` 在正常、错误、取消及终态恢复路径统一清 `textRecovered`，旧文的错误/取消残留事项已关闭。**不得把该兜底推广到 command/edit 等写工具。**

### 4.4 任务完成后的整轮过程折叠

一次任务以 **run** 为单位：运行中保留实时过程，正常结束后将本轮中途正文、思考、工具和子代理过程放入默认收起的「任务过程 · 总耗时」入口，最终答复留在下方。每轮分别展示；运行中注入属于当前轮，排队后新启动的消息属于下一轮。纯文本答复也必须完整可见。收尾 prompt 要求先总结完成内容、验证结果与未完成事项，再给有实际价值的建议；没有建议可以省略，不额外调用模型生成总结。

展示顺序为**先折叠过程，再显示最终总结**。主会话在每个 LLM step 请求前发送 Channel 帧 `step_start`（携带 `gen`）；只有收到该帧的实时 run 启用候选正文暂存。新步骤开始时，上一候选才确认属于过程并释放；当前步骤的正文仍完整保留在 timeline 中，暂不渲染，不靠「总结」标题、有无工具或用户消息时间差猜测最终答复。纯问答的正文同样等待步骤结束；思考、工具进度、AskPanel 与子代理状态不被正文暂存阻塞。

正常终态通过 `task_run.final_answer` 确认最终正文后，当前会话先提交已收起的过程入口，再经过一次绘制机会显示最终正文。候选暂存与交接状态仅用于实时展示，不新增模型调用、不改变模型内容或落盘历史；重开历史直接显示折叠结果，不重播交接。终态事件与 Channel 没有跨通道到达顺序保证：匹配当前实时 run 的权威最终正文可补齐尚未到达的候选尾部，原过程保留，后到旧帧由生命周期守卫拒绝，不能重复追加最终答复。即使终态先于首个 `step_start`，也不能丢失权威最终答复；此时沿用未启用步骤协议的直接展示。失败、取消、缺失可靠最终正文或元数据时释放收到的全部正文并保留原有降级，不能让候选永久隐藏。终态快照对账复用同一收尾，跨 Tab、下一 run 和旧代次帧不能串用交接状态；旧后端未发送 `step_start` 时仍按原流式转录展示。

折叠交接后的实时最终答复使用快速打字机效果：首帧最多显示 12 个字符，以约 500 字符/秒揭示，长答复自适应提高速率，目标在 1.2 秒内完整显示；多段 text 共用一个按原顺序推进的预算，切片不劈开 UTF-16 代理对。游标只在组件局部更新，播放身份按会话与 run 隔离，开始和结束才更新展示标记，不改变真实运行状态、总耗时、模型内容或历史。重开历史、切走再返回、页面隐藏、开始下一轮、减弱动效或播放完成后的普通重渲染均直接显示全文，不重播。揭示期间贴底才跟随，上滚或展开详情后保留阅读位置；图表升级和代码复制入口等待全文就绪，跟进建议 chip 也在本会话的最终答复揭示完成后出现。错误、取消和没有确认最终答复的降级不播放成功总结动画。

步骤边界与节流输出使用同一串行发送边界：先完整发出上一 step 的残余片段，再重置 generation 并发送新 `step_start`，ticker 已取出的片段也不能越过它。重试仍使用原有 `run:retry` 和 `gen` 守卫，不把重试当成新步骤来释放候选正文。`step_start` 属于 Channel 帧，不增加具名事件的 29 键契约面。

每条已确认的最终答复在正文左下方提供一个次级灰色「复制答复」图标，等折叠交接和整条答复的打字机播放结束后才出现；历史恢复直接展示全文时即可复制。复制内容严格取对应 `task_run.final_answer` 的完整原文（包含总结、建议、Markdown、代码与换行），不从 DOM 或逐字揭示的前缀提取，也不混入过程、耗时或其他轮次。复制统一调用 `copyText`；成功短暂显示「已复制」，返回失败或异常时显示「复制失败」并可重试，异步反馈按原答复隔离。未确认的候选正文与过程不新增入口。

正常结束但没有可确认的最终正文时，保留按原顺序展开的任务过程与真实总耗时，不自动收起，也不猜测总结。已经登记最终正文、但当前页尚未找到对应消息时仍按普通转录显示，等待完整分页数据后再聚合。

总耗时由后端记录 run 开始至终态的实际经过时间，包含模型、工具、重试和审批等待，并行子代理不重复累加。它与生成速率的 `runMetrics.genMs`、单工具耗时各有口径，不能相互替代。待回答、待审批均不是完成；失败和取消默认展开过程，保留真实状态与排障信息，不生成成功总结。

折叠只改变展示投影，不删除历史、不回填模型上下文；展开后仍支持工具详情、复制、预览和子代理过程抽屉。过程标题文字与总结正文左对齐，展开箭头紧跟总耗时文字，标题行下方使用主题边框色分隔线；通过 antd Collapse 语义样式取消标题和过程正文的横向内边距，不额外嵌套缩进。展开状态按会话和 run 隔离，用户主动展开后不会被普通重渲染反复收起；折叠变化须保留阅读意图，不能无条件抢回底部。工具调用失败与警告随任务过程一起收起，展开后按原顺序查看；运行级错误/通知与异常子代理结果保留在外层，不能随过程收起而静默消失。

实时转录的一条 assistant 项可以跨越多次 LLM step，不能假定中途正文与最终答复各占一条消息。终态按后端确认的最终消息时间与完整 `final_answer` 精确定位正文后缀；后缀落在同一 text 段内部时也须拆分展示，前缀保留在过程，最终答复仅在折叠外出现一次，原转录不变。轮次在该最终消息处结束，不用 `Date.parse(finished_at)` 猜测后续用户消息是否属于本轮；同毫秒、缺失或非法时间均不得把下一条提问收进过程。注入消息在起始消息与权威最终消息之间时仍归本轮；没有最终消息身份的异常或空答复轮次遇下一条用户消息即停止归组。

重开历史按独立保存的轮次元数据恢复；只有可靠的轮次边界、终态与真实计时才支持整轮折叠。旧记录、缺失或损坏的元数据、尚未加载完整的跨页轮次保留普通转录，不从消息时间差或恢复时刻估算耗时。持久化边界见[会话与项目](./sessions-and-projects.md#任务轮次展示元数据)。真实 WebView 下的折叠、滚动、工具弹层与跨 Tab 行为仍须手动验收，自动测试不能替代。

实现入口：[taskTranscript.ts](../ui/src/features/chat/taskTranscript.ts)、[ChatMessages.tsx](../ui/src/features/chat/ChatMessages.tsx)；投影与边界回归：[task-process.test.tsx](../ui/src/__tests__/task-process.test.tsx)，真实 send / Channel / 工具事件 / 终态与快照 / 渲染链路回归：[run.task-transcript.test.tsx](../ui/src/__tests__/run.task-transcript.test.tsx)，快速总结的预算、格式与动效回归：[final-typewriter.test.tsx](../ui/src/__tests__/final-typewriter.test.tsx)。

## 5. 工具卡与预览

### 5.1 一次调用一张卡

实时 key 在 `execute_batch` 入口重编号：`calls[i].index = i`，所有 start/result/error/progress 与 `ToolCtx` 必须共用 `{batch_id}:{call_index}`。Anthropic 内容块下标可能包含 thinking/text，不能直接拿来当 UI 批内 key。

`tool:start` 提供工具名、args preview、phase；审批/范围门前 `waiting`，通过后 `running`，拒绝直接 error。只读调用只发 running；批层排除、未知工具等硬拒绝不需要先造 start 卡。`tool_progress` 只携带真实输出分片，不能再承担空分片建卡。

主会话的 start、result 和 progress 三条路径都先跨 assistant 项查已有 callKey，找不到才建；notice 插队不能制造第二张卡。已完成卡不能被迟到 start 翻回运行中；run 结束后的 start 不建卡。run 终态把遗留 waiting/running 置 `E_INTERRUPTED`，子代理终态只收其子流。**主会话历史无结果或 repair 的 `[interrupted]` 与子流均显示中性「已中断」**；不能写成「已使用」。`E_ASK_CANCELLED`、`E_ASK_NOT_ANSWERED` 同属中性用户中止；`E_ASK_SKIPPED` 显示中性「已跳过」并停止当前任务；真实失败保留错误信息。

waiting 仅表示审批/范围确认，工具信号量和文件写锁排队仍包含在 running 内。当前 start 事件没有 `run_id`，改事件身份时应整体审查生命周期，不能只在渲染侧补一个未可靠传递的字段。

实现入口：[batch.rs](../src-tauri/src/tools/batch.rs)、[run.ts](../ui/src/stores/run.ts)、[runFrames.ts](../ui/src/stores/runFrames.ts)、[runHandlers.ts](../ui/src/stores/runHandlers.ts)、[ToolCallCard.tsx](../ui/src/features/tools/ToolCallCard.tsx)。

### 5.2 多文件摘要

read/batch_read/edit 一次批量调用本来就是一张卡。头部展示全部文件的最短可区分后缀，避免 `a/ui.ts`、`b/ui.ts` 都退化为 `ui.ts`；结果优先，运行中回退 args preview，长清单省略并在 title 保留全量。create/delete 等单路径工具保留路径语义。大 args preview 可能退化为截断提示，不能把它当完整参数备份。

排查「是不是合并了调用」时查看会话日志：一条执行记录、files 数组多项表示正常批量；多条执行记录才是多个调用。恢复按 provider id 配对，重复/空 id 的处理属于历史修复和协议层问题，不可通过拆 UI 卡掩盖。批量读取的错误策略见[工具与安全](./tools-and-safety.md)。

### 5.2.x 简洁与详细展示

「简洁 / 详细」仅在设置「界面 → 输出展示」提供切换入口，聊天区不另放控件；全局偏好为 `ws_output_display`，默认「详细」，缺失/非法值也回落详细。切换即时生效并保存到 localStorage，不参与设置脏点，也不改回复正文、provider 请求、权限或历史。详细沿用原工具卡；简洁使用 antd 按钮承载中性单行摘要，长命令省略但完整内容可展开，运行中保留加载指示，思考默认收起且省去标题走马灯。

简洁和详细模式均移除用户与助手的整条消息身份行；简洁过程记录也不预留行尾时间槽。落盘记录不合并、不删除；跨消息汇总为源消息的 `data-key` / `data-sig` 保留定位映射，展开态按原会话 / 消息 / 调用身份保存。空状态使用次级文字色，正常可点击工具摘要保持可读，等待 / 失败仍使用原有语义色。

简洁时间线用单一 flex 容器管理 4px 过程行间距；汇总、单工具和思考头部共用最小行高（至少 32px，随界面字号增长），不叠加各自的外边距。按钮的透明边框也计入行高，避免与无边框的思考头部相差 2px。混合消息的最后可见段若是工具 / 思考，也使用同一过程底距；过程后接正文消息仍保留阅读留白。空白正文只在展示层跳过，真实正文与代码缩进不 trim，过滤成功 subagent 工具或缺失工具时不把隐藏段误判为可见尾段；展开组的 `[hidden]` 不能被 flex 布局覆盖。详细工具展示、正文行高与滚动锚点保持原语义。

- 简洁把**连续的思考 + 可入组工具**折叠为一行摘要（如 `思考 8 次 · 已读取 12 个文件 · 已搜索 4 次 · 已运行 3 条命令`），点开才展开。摘要计数按真实段数/调用数统计，不累加已有摘要文字。
- 折叠规则：思考段与运行中的普通工具都可入组（跑批期间也保持一行，思考计时每 500ms 刷新）；成功无警告的 read/batch_read/grep/list_files 与退出码明确为 0 的 command 入组；空白正文不分隔可见过程。subagent 通用工具必须在分组前排除，统一由专属子代理卡承载，不能让 running 兜底绕过隐藏重复卡的规则；spawn 前失败的调用仍显示错误卡。**仍分隔**：真实正文、子代理卡、审批等待、失败与警告、用户消息、通知、压缩分界、正在阅读详情的工具。
- 简洁过程使用次级文字色、静态工具图标和末尾展开箭头，展开组内成员保持扁平列表；运行中工具单列真实运行数量，完成计数不包含它们。子代理简洁行显示名称、状态与任务摘要，步骤/token 放在悬停说明，停止入口和异常收尾警告保留。它们不重复绘制 spinner；活动状态统一留在当前消息末尾，正文与总结保持正常阅读权重。
- 思考时长口径：组内有正在计时（位于尾部且消息仍流式）的思考段时显示 `思考中……（Ns）`；否则只报次数 `思考 N 次`。恢复的历史思考与被中断的思考都没有真实耗时（`appendDelta` 的 restored 分支不记 `startedAt`），**绝不补 0 秒**。
- 锚点：组摘要行的 `data-output-key` 取根键对应成员（优先组内首个工具的 `callKey`，纯思考组用 `thinking:<段下标>`），`data-output-group` 含全部成员键；跨消息组为**每个成员（含思考段）**记源消息映射，`data-anchor-aliases` 覆盖全部源消息，否则滚动定位会掉回上一条消息。展开态按会话 / 原消息键 / 根键保存，来回切换模式不自动关闭；键读不到时回退到组内本地 state（纯思考组无工具即无 origins 可索引，若传受控 false 会把本地展开盖死、点开无反应）。
- 根键稳定性：含工具的组根键是 provider 侧 `tool_use.id`，跨消息摊平后仍稳定；纯思考组的根键是段下标，跨消息摊平后下标仍稳定，但一旦因 sidecar 回填或新成员并入而变成含工具组，根键会从 `thinking:N` 变成某个 callKey，已展开的组会随之收敛。
- 单成员组不画摘要行，直接内联渲染那一个成员（本来就是一行，再包一层只会平白多一次点击）。
- 恢复路径的既有行为：工具出参先以 `{restored:true}` 占位时，`command` 拿不到 `exit_code` 故暂不入组，sidecar 回填后可能重新入组或被踢出独立成行——切换展示模式时同样会重算，本就不新增跳变。
- edit/create 逐文件显示最短可区分后缀及完整路径提示。成功结果可增 `data.line_changes`（数组元素为 `path/added/removed`），由写锁内真实前后内容进行行级 diff；统计是本次操作的净变化，不是传入 patch 行数或相对 HEAD 的工作区变化。CRLF/LF 归一后统计，空文件没有虚构末行；create 的原件不可读或非 UTF-8 时省略统计，失败结果不附统计。旧历史缺统计就不显示数字，不新增迁移或摘要存档。
- 工具卡渲染时，ask 题干与答案、等待审批、失败状态、警告及 widget 预览保持可见；错误码与完整错误信息随工具详情展开/收起，简洁模式也不在收起的卡片外重复展示。失败调用仍独立显示红色摘要，不并入成功工具组。用户取消/中断沿用中性状态。子代理卡保持状态、提前结束警告、停止和现有过程抽屉，抽屉使用同一展示偏好与工具结果回填。
- 切换前捕获当前可见段/工具与所属消息，提交后恢复该段内阅读偏移；折叠工具组只以摘要行作锚点，打开组后各子卡仍有独立锚点。组展开态按会话 / 原首消息键 / 首调用键保存；前翻不会把已打开组的根并入新增历史，保留展开态和阅读位置。单调用不设置组展开边界，关闭详情后仍可重新聚合。主聊天校验 session，过程抽屉校验 subId；原来贴底则继续贴底，上滚阅读则暂停跟随。
- 汇总头与首子卡可能共用输出键，定位同时记录 summary / content 类型；恢复优先匹配同类型。子卡回落到汇总头时清零卡内偏移，避免深层偏移把视口落到后续消息。

实现：[compactTranscript.ts](../ui/src/features/chat/compactTranscript.ts)、[toolPresentation.ts](../ui/src/features/tools/toolPresentation.ts)、[ToolActivityGroup.tsx](../ui/src/features/tools/ToolActivityGroup.tsx)、[thinkingBlock.tsx](../ui/src/features/chat/thinkingBlock.tsx)、[outputAnchor.ts](../ui/src/utils/outputAnchor.ts)、[line_changes.rs](../src-tauri/src/tools/line_changes.rs)。回归：[compactTranscript.test.ts](../ui/src/__tests__/compactTranscript.test.ts)、[chat.process-display.test.tsx](../ui/src/__tests__/chat.process-display.test.tsx)、[output-display.test.tsx](../ui/src/__tests__/output-display.test.tsx)、[thinking.history.test.tsx](../ui/src/__tests__/thinking.history.test.tsx) 与 create/edit 行数测试。仍需手验重启偏好、浅深主题/字体/窄窗口、长消息中部切换、工具完成时保持详情、真实审批与自动预览、历史和子代理抽屉恢复。`thinkingBlock.tsx` 从 `segments.tsx` 抽出：过程组要按原位渲染组内思考段，两者互相 import 会成环。

### 5.3 HTML 小组件

展示偏好与 HTML 预览独立，简洁模式也保留预览按钮及批准后的自动预览。

`render_html` 接受 1–50,000 个 Unicode 字符，工具卡头部「预览」打开大弹框，卡内不再嵌一个默认约 150px 高 iframe。没有可恢复 html 的旧卡显示不可用说明，不提供坏入口。

弹框约 90vw × 85vh，内容滚动在 iframe 内；支持原样复制、重挂 iframe 重新加载、浅/深画布，复制失败有反馈。每次打开按当前主题选画布；画布色与 `color-scheme` 不改写 `srcDoc`，HTML 自带背景照常覆盖。关闭即销毁 iframe，切会话卡片卸载也关闭。自动预览是一轮内的一次性信号，不是每张 render_html 都自动弹；方案卡「查看方案 / 查看预览」同行；「查看预览」固定入口的授权语义见[计划与批准](./plan-and-approval.md)。

iframe 使用 `sandbox="allow-scripts"`、不授予 `allow-same-origin`；资源访问还受应用 CSP 限制。不要仅凭 sandbox 声称任意网络都被独立封禁，也不要为预览偷偷放宽 CSP/同源能力。源码复制保持字节原样不等于任意脚本在当前 CSP 下都能运行。完整出参恢复由通用 sidecar 完成，旧会话没有备份时无法凭空补全。

实现入口：[WidgetPreviewModal.tsx](../ui/src/features/tools/WidgetPreviewModal.tsx)、[render_html.rs](../src-tauri/src/tools/render_html.rs)、[tauri.conf.json](../src-tauri/tauri.conf.json)。

## 6. 滚动、分页与抽屉

### 6.1 阅读意图优先

贴底时新增内容自动跟随；上滚、拖滚动条/键盘离底、展开工具/思考卡均可暂停。只有滚回底部、点击「回到底部」或新发送消息的明确路径恢复跟随。禁止在每帧 effect 里用 near-bottom 几何强制抢回控制权。

所有程序化跳底统一走 `jumpToBottom`：实际目标必须是 `max(0, scrollHeight-clientHeight)`，不是 `scrollHeight`。后者会被浏览器钳位，使豁免目标与真实 scrollTop 永远相差一个视口。瞬时跳底只豁免目标点附近，150ms；平滑回底才使用 from→target 区间，700ms。`BOTTOM_EPS=40` 共用于贴底与自家滚动判定。上滚立即清豁免窗，避免高频流式不断续窗吞掉用户输入。平滑窗内恰好落在区间的小幅操作仍有被当自家事件的边界，需要人工验证。

跟随先同步跳底，再在图表升级后补一次；图表 promise 失败不得卡死后续滚动。展示动画不得每帧写 store 驱动该链路。

### 6.2 锚点与长历史

恢复记录「稳定消息 key + 指纹兜底 + 段内偏移」，不保存绝对 scrollTop 作为唯一依据。分段消息 key 是 `s<段>:<序>`，实时项为 `live:<序>`；前插更早历史不能用数组下标 key，否则已有卡片重挂、折叠状态和位置一起漂移。

无锚点或上次贴底则直接贴底；有锚点则暂停跟随，等消息加载/布局就绪有界校正，切会话或用户接管作废旧恢复。找不到锚点才降级贴底。前翻先捕获当前锚点，插入后还原；压缩边界只是显示分隔线，不参与锚点身份，也不代表旧历史已删除。分页页数与装载边界见[会话与项目](./sessions-and-projects.md)。

主聊天现在隐藏原生滚动条，使用 [ChatScrollbar.tsx](../ui/src/features/chat/ChatScrollbar.tsx) 的 overlay：滚动/悬停显示，停止约 1 秒后淡出，支持点轨道与拖 thumb，内容不溢出时不显示 thumb。它只是 scrollTop 的可视化入口，不能另建一套滚动状态或绕过阅读意图判定。

### 6.3 子代理抽屉的高度链

子代理过程抽屉仅由用户通过聊天卡或右栏主动打开；运行总览提供独立的摘要与报告详情抽屉，可从中返回父会话，不直接打开过程流。子代理启动不再自动弹出或切换当前查看对象。Composer 不显示子代理运行计数及查看菜单，`$` 点名角色的输入菜单仍可使用。状态与过程恢复契约见[子代理](./subagents.md#6-实时过程抽屉与恢复)。

唯一滚动体是 `.sub-drawer-body`。wrapper/section 必须有界，中间 `.ant-drawer-body` 是 `min-height:0` 的透传层且 overflow hidden；滚动体直接子项 `flex-shrink:0`，否则带 overflow hidden 的流块会被压扁到视口内，DOM 仍有内容却永不溢出。

抽屉开关时 DOM 可能晚于 open 状态挂载，callback ref 需在真实挂载下一帧执行 viewport clamp 和首次贴底。resize 按窗口高减 header 高钳制；仅 CSS effect 可能第一次空跑。自动跟随复用主聊天的实际落点/用户接管 helper；切 sub id 重置，signature 必须包含 thinking 增量而非只看段数。

分配任务默认全文展开，可手动收起；切换子代理重置展开。dev-only 几何探针在 CSS 链未定界/内容被裁时输出各层 computed geometry。遇到「不能滚/过程消失」先干净重启 dev 排除陈旧 HMR，再核对视口高度、scrollHeight、直接子项是否被压扁和探针输出，不能反复盲加 overflow。

实现入口：[ChatMessages.tsx](../ui/src/features/chat/ChatMessages.tsx)、[scrollAnchor.ts](../ui/src/utils/scrollAnchor.ts)、[SubagentDrawer.tsx](../ui/src/features/subagent/SubagentDrawer.tsx)。

## 7. ask、窗口与通知

### 7.1 询问区域

ask 是活跃会话唯一底部输入区域；主 Composer 和队列暂时不显示。选项标题和推荐 chip 同行，说明独占下一行、与标题左对齐、完整折行；整行包括说明都可点击，键盘/ARIA 状态不能因排版容器改变而丢失。长说明不限高，会占更多垂直空间。

补充说明内 `Enter` 走下一题/提交，IME 不提交；`Shift+Enter` 不提交，但当前补充控件是单行 Input，不能承诺它提供可见多行。批准、预览、多选与跳过的决定逻辑以[计划与批准](./plan-and-approval.md)为准；「跳过」在任意页明确停止当前任务，不提交答案或自动批准。计划卡已展示全文时，单题批准形的题干可精简为确认问句，多题/普通询问不能丢原题信息。

### 7.2 自绘标题栏

`tauri-plugin-decoration` v3 提供窗口控制，host 层激活，core 不依赖 Tauri。窗口以 `visible:false`、`decorations:true` 启动，先恢复几何，再由 `activate_and_show` 激活和亮窗；失败恢复原生装饰，前端命令错误也按 native 撤掉 clearance。后端 5 秒 watchdog 为前端启动失败兜底亮窗。

Windows 控制条让位取插件 right clearance，macOS 红绿灯在展开时让左段、折叠时让右段；native 回退撤销让位。顶栏拖拽层必须有 `position:relative` 的宿主，否则 absolute inset 可能覆盖整个窗口，吃掉聊天和面板点击。空白目标标 `data-tauri-drag-region`，按钮/胶囊不标，保证点击不拖窗。

系统原生编辑菜单在 input/textarea/contenteditable 内放行；其余区域屏蔽 WebView 默认浏览器菜单。macOS 应用 Edit 菜单不能代替 Windows 的右键编辑路径。

### 7.3 通知回到正确会话

应用内通知位于标题栏下方顶部居中，限制最大宽度，当前 z-index 高于 Modal，弹框期间仍可见。会话通知携带 session id；点击已开会话直接切 Tab，未开则重开，删除后不切错误会话。普通无 session toast 不导航。原生通知回调由后端先 show/unminimize/focus，再发 `notify:activate`；前端只负责 reveal 和成功后的按会话清通知。

- **Windows**：运行时只读检测 AUMID 注册，存在则使用应用 identifier；不存在的 dev/便携环境回退 PowerShell 归属，只保显示，不能保证会话回跳。AUMID 由 NSIS/WiX 在安装时写 HKCU，`IconUri` 必须指向真实 PNG，不能写 exe。禁止为补通知在运行时拉 PowerShell、写注册表或创建快捷方式。MSI 的 HKCU 落在执行安装的用户账户下。
- **macOS**：发送前显式 `set_application(bundle_id)`，避免库的 `use_default` AppleScript 解析引出「Choose Application」模态框。dev 未注册 bundle 时允许 Finder 兜底；展示期点击/查看回调可导航，通知中心历史点击不保证。macOS 原生发送在后台阻塞任务内完成，IPC 提前返回成功不等于系统已显示通知，后续失败不能可靠触发前端 fallback。
- **Linux/原生路径失败**：回退通知插件；通知可显示，但没有相同的系统点击回跳能力。原生与插件路径互斥，避免双发。只要有会话 id 就可走原生路径，不区分项目/临时会话。
- 失焦完成和等待回答会触发相应通知；计划任务没有会话 id 的通知不具备同样的回跳契约。

实现入口：[notify.rs](../src-tauri/src/host/notify.rs)、[runHandlers.ts](../ui/src/stores/runHandlers.ts)、[installer-hooks.nsh](../src-tauri/windows/installer-hooks.nsh)、[toast-aumid.wxs](../src-tauri/windows/toast-aumid.wxs)。安装验证见[发布指南](./release.md)。

Windows dev/便携版没有安装器注册时，可由用户在自己的命令提示符中检查并按需注册。先将第二条写入命令的图片路径替换为**确实存在的 PNG 绝对路径**（例如仓库的 `src-tauri\icons\128x128.png`），不要填 exe。操作只影响当前用户的应用通知归属；应用运行期不会自动执行这些命令。

```bat
reg query "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave"
reg add "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /v DisplayName /t REG_SZ /d "CodeWave" /f
reg add "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /v IconUri /t REG_SZ /d "C:\完整路径\CodeWave\src-tauri\icons\128x128.png" /f
```

注册后在 `src-tauri/` 执行 `cargo test --lib probe_own_aumid_toast -- --ignored --nocapture`，按探针要求点击真实 toast；探针检测原生点击回调，主窗口和会话回跳仍需在应用中人工复核。撤销手工注册可执行 `reg delete "HKCU\Software\Classes\AppUserModelId\xyz.yangshifu.codewave" /f`，会回退到上述未注册行为。不要在已安装版本仍依赖该键时随意删除。

## 8. 排查与回归验证

| 症状 | 优先核对 |
|---|---|
| 暗色文字但背景仍亮 | `effectiveDark` → antd 算法 → `isDarkBase` 简写 hex → ThemeBridge；预览与高亮共源 |
| 草稿串会话、发送后别的 Tab 被清 | 异步闭包是否捕获 sessionId；是否完整处理 text/images/refs；驻留回填是否覆盖新草稿 |
| 有正文时 `/`、`$`、`@` 不弹或回填重复 | caret 是否同步，trigger 是否在外部改写后作废，回填是否现算片段；先确认 `/`/`$` 位于消息开头 |
| 流式不能上滚/发送后不跟随 | 用户意图是否被 effect 覆盖，豁免目标是否用实际落点，激活分支是否走统一 jumpToBottom |
| 同工具出现运行和完成两张卡 | batch index 是否重编号；start/result/progress 是否同 key 且跨项查找 |
| 结束后仍转圈或提问卡锁住输入 | 三类终态及对账是否调用共享 settleRun，而非只改 `running=false` |
| 预览消失/旧工具结果空白 | 历史模型瘦身文本是否可解析，sidecar 是否存在、键是否为 provider call id，三处消费是否共用回填 |
| 抽屉/右栏内容有但不能滚 | 从 root 到 scroller 检查有界高度、`min-height:0`、真实 antd 6 DOM 类名、子项 flex-shrink；先排除陈旧 dev |
| Windows 通知是 PowerShell或点不回去 | 安装用户下 AUMID 是否存在、PNG 路径是否有效、原生回调是否 emit；不要从前端 `setFocus` 单独下结论 |

自动化验证按[测试指南](./testing.md)执行，文档不固定历史用例数。核心测试入口：

- 输入：`composer.per-tab`、`composer.history`、`composer.paste`、`composer.refs`、`composer.fileref`、`composer.triggers`、`composer.trigger-caret`、`uiState`。
- 指标：`runUsage`、`utils.models`、`composerMetrics`、`composer.rate`、`composer.toolbar.style`、`tokenstats.modal`；后端计时与统计 merge 测试。
- 流式与恢复：`run.interleave`、`run.streaming-indicator`、`chat.streaming-indicator`、`run.text-ask-fallback`、`typewriter`、`chat.history-boundaries`、`scrollAnchor`、`subagent.drawer`。
- 工具与预览：`toolcallcard`、`toolcallcard.args`、`widget.preview`；后端 start/result key、waiting→running、硬拒绝无 start、进度非空分片测试。
- 主题与壳：`theme`、`theme.bridge`、`titlebar.content`、`titlebar.style`、`sidebar.style`、`rightbar.style`、`notify-reveal`、`askpanel`、`ask.style`、`markdown.style`、`markdown.user`。

happy-dom 无真实布局：滚动回归要模拟 scrollTop 钳位及下一帧派发，样式契约同时检查 DOM 消费；不能用全绿 DOM 测试证明像素布局通过。按项目约定不做 GUI 自动点验，交付人工检查：

1. 两 Tab 草稿带文字、图片、refs 来回切；发送期间切 Tab；修改、召回、队列编辑、ask 覆盖和关闭保留后重开。
2. 长正文/长思考流式中滚轮上滚、拖 thumb、PageUp、展开卡片，再回底；切会话、前翻历史、重启恢复锚点。
3. Anthropic thinking 后多工具调用只出现各一张卡；审批等待/拒绝、取消、失败、子代理终态都停止对应指示。
4. 中英和亮/暗/跟随系统切换；最窄实际窗口、长模型名、左右栏最大宽度、减弱动效；输入菜单/说明不遮挡邻居。
5. 大 HTML、长表格、代码块、mermaid/公式预览，关闭/重开/切 Tab，源码复制与深浅画布；右栏概览点击变更/子代理/会话文件进入对应弹窗或详情，额度和技能折叠后重开保持；328–640px 栏宽、亮暗主题、长文件名、无会话与查询失败下内容和操作可读，长计划/日志/文件可滚动。变更弹窗切文件、重开保留选择、换会话/隐藏右栏关闭、返回原视图；检查长行横向滚动和两侧的纵向滚动。
6. Windows 安装版/dev 与 macOS 首次失焦通知分别确认品牌、无额外系统模态框、点击回到正确会话；检查原生标题栏 fallback。

## 9. 已知限制与开放事项

以下仍是当前实现边界，不把已经修复的主会话中断卡、草稿持久化、暗色简写识别、终态 textRecovered 清理再次列为待办：

- 文件名含空格的 `@` 引用协议与搜索目录标志仍待完善；保守回填门禁只避免损坏已有正文。
- 附件图片的显式草稿身份、原子合并、失效票据与发送快照已实现，`composer.attachments.race` / `run.attachments` 在本次 CI 通过；原生 WebView/文件选择框、跨 Tab 与发送/关闭交错仍需按上述清单手验，不能将自动回归写成 GUI 已验收。
- 缓存命中按 Tab 累加但未按协议切换分桶；统计旧新同日混桶以及非主运行路径无耗时仍限制速率解释范围。
- 平滑回底豁免窗内的区间操作、超窄 Composer 指标裁切、单行 ask 说明的 Shift+Enter 表现需按实际平台确认。
- `run:retry` 对流式项的清场范围仍需留意 notice 插队后的较早工具卡；终态兜底能落定不等于重试期间展示已完全清空。
- overlay 滚动条当前以鼠标轨道/拖动为主；极端短轨几何、触屏/键盘可达性与仅内容高度变化时的同步覆盖可继续补测试。不能将旧「原生滚动条主题化」当当前实现。
- 字体持久化失败原因/重试、权威回执、迟到旧回执与新字号保留已有 `font.settings` / `font.sizes` 自动回归；本次证据见[测试指南](./testing.md#可靠性修复自动验证记录2026-10-12)。真实字体视觉、重启保持、HTML CSP 下的实际脚本能力、macOS 异步通知失败和跨平台历史点击仍需各自实机验收，不能从前端无报错推导成功。
