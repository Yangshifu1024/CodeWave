# 聊天与界面指南

本文记录当前聊天输入、转录展示、工具卡、窗口与通知的行为，以及修改这些功能时必须保留的契约。以当前源码为准，不把旧批次报告的验收数字或已被替换的界面方案当作现状。

相关主题：[会话与项目](./sessions-and-projects.md)、[运行与调度](./runtime-and-scheduling.md)、[计划与批准](./plan-and-approval.md)、[子代理](./subagents.md)、[供应商与网络](./providers-and-network.md)、[设置](./settings-guide.md)、[文档与文件预览](./documents.md)。

## 1. 界面结构与主题

### 1.1 导航、标题栏和右栏

- 左栏承载临时会话、项目与会话入口；工作区文件树和内联编辑器已移除。文件引用走 Composer 的 `@` 或附件入口，会话产物走右栏「文件」。左下角提供 git 身份及任务、统计、设置入口。
- 顶栏左段是与左栏同宽同色的背景带；右段承载 Logo 左栏开关、当前会话标题、目录与分支胶囊、右栏开关。目录胶囊显示工作目录 basename，悬停显示完整路径；临时会话显示对应标记；分支胶囊只读。旧的会话历史前进/后退按钮、双栈和 `Alt+←/→` 已移除。
- 设置、任务和运行总览打开时，标题反映覆盖页而非后台会话。设置页隐藏工作区两侧栏开关。工作区仍保持挂载，覆盖页的详情见各主题指南。
- 左栏和右栏折叠宽度均为 **0**，没有窄轨。左栏保留 antd `Sider` 外壳和内容，右栏保留 Tabs 实例；以宽度过渡裁切，避免折叠时把布局从横向变成纵向或销毁面板状态。右栏隐藏同时关闭其 Portal 查看弹框，防止弹框逃出隐藏外壳。
- 右栏顺序固定为「信息 / 日志 / 文件 / 变更」，默认「信息」。信息页包含项目目录、额度与余额、技能、当前计划；数据目录入口在设置「关于」。技能和计划可折叠，偏好全局记忆；计划仅在有 todo 时出现。
- git 身份是本地只读配置读取，仓库配置优先、失败回退全局配置；头像由邮箱/名字本地生成，不请求远程头像服务。不要把该入口扩展成隐式 git 写操作。

宽度真源在 [layout.ts](../ui/src/utils/layout.ts)：左栏默认 280、范围 180–480；右栏默认 328、范围 328–640；中栏目标保底 480。窗口缩窄时先压右栏再压左栏，**只夹显示值，不回写用户记忆宽度**。分隔条支持拖动、方向键 16px 微调、双击复位；显示受夹取时禁止拖动，仍可复位。标题栏、Sider、右栏必须消费同一套显示宽度，不能各自计算。布局最小值不意味着任意异常尺寸下中栏绝不会被挤压。

**拖动期必须禁选**：分隔条按下后浏览器会落一个选区锚点，而选区延展不跨 flex 块级边界设障，于是 pointermove 扫过的路径上的文字全部进选区，表现为「拖完整个窗口全是高亮」且松手后残留（连 Composer 输入框都会被扫进去）。因此拖动期由 `<html>` 上的 `ws-resizing` 类（`ResizeHandle` 在 `pointerdown` 挂、`pointerup`/`pointerCancel`/`onLostPointerCapture`/卸载兜底摘）对**整棵子树**加 `user-select: none !important`——只禁选分隔条自身不够，子选择器 `html.ws-resizing *` 不能省。作用域仅限拖动这一瞬间，松手后正文的正常选中与复制不受影响。回归入口：[layout.resize.test.tsx](../ui/src/__tests__/layout.resize.test.tsx)（CSS 文本契约断言，happy-dom 不解析外链 CSS）。

实现入口：[AppShell.tsx](../ui/src/features/shell/AppShell.tsx)、[TopBar.tsx](../ui/src/features/shell/TopBar.tsx)、[RightBar.tsx](../ui/src/features/shell/RightBar.tsx)、[ResizeHandle.tsx](../ui/src/features/shell/ResizeHandle.tsx)。

### 1.2 主题、字体与可见性约束

主题有「跟随系统 / 亮色 / 暗色」三档，偏好同步读 localStorage；`App.tsx` 的 `effectiveDark` 是 antd 算法、前景 token 和根元素 `dark` class 的统一来源。应用 chrome 的主色保持中性墨色：亮 `#1f1f1f`、暗 `#424242`；`--ws-accent` 取 **`colorPrimaryText`**，适用于文字、图标、边框和流光，不能直接当作暗色白字按钮的背景。

当前亮色导航底是 `#eef4f9`，主内容与右栏底是 `#FFFFFF`；暗色分别取 antd 容器/布局 token。不要复用旧报告中的 `#ececee`、`#f8f8f8`。亮度判定集中在 [colors.ts](../ui/src/theme/colors.ts)，必须同时接受 `#000`/`#fff` 与六位 hex；主题桥、代码高亮和 HTML 预览须同判据，避免暗色文字配亮色背景。

- 字体分为界面、内容、代码三槽，分别消费 `--ws-font-sans` / `--ws-font-content` / `--ws-font-mono`；antd 的 `fontFamily` / `token.fontSize` 跟随界面槽，`fontFamilyCode` 接代码槽。用户字体名经过清洗，再与各自默认回退链组合。字号默认 **14 / 14 / 12px**；antd 6 的 `font-size: 0` 容器问题见下节，保存与失败边界见[设置指南](./settings-guide.md)。
- 控件由 antd 6 接管；自定义样式消费主题桥的 `--ws-*`，不要重新引入默认蓝主色或多套调色板。菜单选中底使用中性半透明填充，悬停选中再加深；`controlOutline: transparent` 去除输入控件双重外晕，聚焦仍以边框等既有反馈表示。

#### 1.2.x 字体与字号三槽

每槽独立保存字体族与字号，配置为 `config.ui.{font_sans, font_content, font_mono, font_size_sans, font_size_content, font_size_mono}`，WebView 的 `localStorage` 只作首帧防闪缓存。设置页按「**界面字体 → 界面字号 → 内容字体 → 内容字号 → 代码字体 → 代码字号**」排列，字号 Select 与字体输入同宽，每项带独立预览和还原默认入口。字号为连续 **12–20px** 整数；默认及还原值是 **14 / 14 / 12px**。已有合法显式值保留，新增内容字段缺失时默认 14px；还原只清除本槽缓存覆盖并同步本槽默认值。

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
- `Shift+Tab` 只在输入框内循环：**计划 → 逐项确认 → 自动编辑 → 完全访问 → 计划**，与权限菜单顺序一致，菜单打开也可切换。走 `updatePrefs` 乐观更新及失败回滚。权限与运行中切档的安全语义见[计划与批准](./plan-and-approval.md)。
- 纯文本空输入按 `↑` 进入已发送用户消息历史；浏览中 `↑/↓` 翻页，最旧处不环回，越过最新或 `Esc` 恢复进入前草稿。编辑召回内容会退出浏览，失焦不退出，避免点发送前文本被还原。
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

正文、思考、工具与子代理锚点按 timeline 顺序显示。后端 64ms 节流以有序 `Text/Reasoning` 段缓存，同 kind 相邻合并，跨 kind 不重排；不能退回两条无序字符串再让前端猜时序。装配与落盘保留块序，保存路径保留 thinking；各协议是否回传 thinking 由供应商层决定。

同一 Tab **至多一个 streaming assistant 项**，且位于当前列表末尾。notice 插队后新建流式项前必须关闭此前所有流式项；done/error/cancelled 和终态对账必须扫全 Tab 收尾，不只关闭最后一项。等待指示是 `LoadingOutlined`，表示仍在输出而非进度百分比；禁止重新引入全局 `.cursor` 闪烁类。

重试通过 `streamGen` 隔离旧 attempt；旧代次帧不能回填已清场的时间线。Tab/run 生命周期另有 `runEpoch` 与 run 身份守卫，不能与 provider 流代次混为一个计数。迟到 delta 不得在结束后重建流式项，usage 则遵守上一节的独立计数规则。终态丢失恢复与队列调度详见[运行与调度](./runtime-and-scheduling.md)。

### 4.2 正文、思考与 Markdown

- 正文只有当前末尾流式文本段逐字揭示，游标留在组件局部 state，绝不按动画帧写 store；历史、非尾段、结束/取消和减弱动效下直接显示全文。这样不会把聊天滚动和图表升级推到 60fps。实现中的速度追赶是显示效果，不改变保存文本或 provider 内容。
- 思考默认收起，展开状态不持久化。运行中标题旁展示最新非空行：先左对齐停留、溢出才向左移动，只有真正换行才上翻，同行增量不重新翻页；减弱动效下静态显示。展开思考或工具卡视作阅读意图，暂停外层自动跟随。
- 子代理 `<report>` 标记仅在子代理显示路径剥离，先合并/剥离再逐字切片；不能对主聊天所有正文全局清除，用户可能正在讨论该标记。
- 用户气泡使用保守 Markdown：数学保留可见源串、mermaid 当代码显示；助手/计划/文件 Markdown 才进入图表与公式升级。外部链接统一经 `open_url` 打开系统浏览器，避免 WebView 的 `_blank` 无动作。
- 宽表由 Markdown renderer 实际生成 `.table-wrap`，CSS 赋横向滚动；测试必须同时守 DOM 与 CSS，不能只测试一段无人消费的样式。文件预览使用独立 `.viewer-md.md`，不能套聊天 `.assistant` 的裁切/行长约束；详见[文档指南](./documents.md)。
- 聊天区当前左右各留 15% 中栏宽度；具体视觉上限属于样式，不改变数据或分页语义。Markdown 高亮随主题桥更新；流式图表占位文案走 i18n，渲染缓存键包含语言相关提示文案。

实现入口：[segments.tsx](../ui/src/features/chat/segments.tsx)、[markdown.ts](../ui/src/utils/markdown.ts)、[throttle.rs](../src-tauri/src/util/throttle.rs)、[dto.rs](../src-tauri/src/provider/dto.rs)。

### 4.3 时间、工具出参与文本 ask

消息首次进入转录时才盖 `created_at` UTC RFC3339 时间，保存/重载不能为旧消息补当前时间。UI 显示本地时间；缺失或不可解析则留空。历史恢复不重播逐字动画。

历史模型侧工具文本可能已瘦身；无法解析的出参通过完整 `ToolOutcome` sidecar 批量回填。持久键是 **provider `tool_use.id`**，不是实时 `{batch}:{index}`。主工具卡、子代理卡及子代理过程流共用回填判据；内容不能写回模型历史或再次出网。子代理恢复卡从合成 key 改成真实 sub id 后才能继续打开过程流。存储、回填与删除细节见[会话与项目](./sessions-and-projects.md)。

文本 ask 兜底只接受主会话、ask 可用、当前回合无真实调用、每 run 一次，并要求末尾独占完整块、非代码围栏、结构有效；安全语义完全复用正常 AskTool，详见[计划与批准](./plan-and-approval.md)。后端剥掉历史中的块后，前端仍需在 `ask:opened.text_recovered` 到达时及每个后续 text delta 后幂等剥离，因为节流尾帧可能晚于 ask 事件。跨 thinking 段拆开的块不拼接剥除；块外端点控制 token 不猜测清洗。当前 `settleRun` 在正常、错误、取消及终态恢复路径统一清 `textRecovered`，旧文的错误/取消残留事项已关闭。**不得把该兜底推广到 command/edit 等写工具。**

## 5. 工具卡与预览

### 5.1 一次调用一张卡

实时 key 在 `execute_batch` 入口重编号：`calls[i].index = i`，所有 start/result/error/progress 与 `ToolCtx` 必须共用 `{batch_id}:{call_index}`。Anthropic 内容块下标可能包含 thinking/text，不能直接拿来当 UI 批内 key。

`tool:start` 提供工具名、args preview、phase；审批/范围门前 `waiting`，通过后 `running`，拒绝直接 error。只读调用只发 running；批层排除、未知工具等硬拒绝不需要先造 start 卡。`tool_progress` 只携带真实输出分片，不能再承担空分片建卡。

主会话的 start、result 和 progress 三条路径都先跨 assistant 项查已有 callKey，找不到才建；notice 插队不能制造第二张卡。已完成卡不能被迟到 start 翻回运行中；run 结束后的 start 不建卡。run 终态把遗留 waiting/running 置 `E_INTERRUPTED`，子代理终态只收其子流。**主会话历史无结果或 repair 的 `[interrupted]` 与子流均显示中性「已中断」**；不能写成「已使用」。`E_ASK_CANCELLED`、`E_ASK_NOT_ANSWERED` 同属中性用户中止；真实失败保留错误信息。

waiting 仅表示审批/范围确认，工具信号量和文件写锁排队仍包含在 running 内。当前 start 事件没有 `run_id`，改事件身份时应整体审查生命周期，不能只在渲染侧补一个未可靠传递的字段。

实现入口：[batch.rs](../src-tauri/src/tools/batch.rs)、[run.ts](../ui/src/stores/run.ts)、[runFrames.ts](../ui/src/stores/runFrames.ts)、[runHandlers.ts](../ui/src/stores/runHandlers.ts)、[ToolCallCard.tsx](../ui/src/features/tools/ToolCallCard.tsx)。

### 5.2 多文件摘要

read/batch_read/edit 一次批量调用本来就是一张卡。头部展示全部文件的最短可区分后缀，避免 `a/ui.ts`、`b/ui.ts` 都退化为 `ui.ts`；结果优先，运行中回退 args preview，长清单省略并在 title 保留全量。create/delete 等单路径工具保留路径语义。大 args preview 可能退化为截断提示，不能把它当完整参数备份。

排查「是不是合并了调用」时查看会话日志：一条执行记录、files 数组多项表示正常批量；多条执行记录才是多个调用。恢复按 provider id 配对，重复/空 id 的处理属于历史修复和协议层问题，不可通过拆 UI 卡掩盖。批量读取的错误策略见[工具与安全](./tools-and-safety.md)。

### 5.3 HTML 小组件

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

唯一滚动体是 `.sub-drawer-body`。wrapper/section 必须有界，中间 `.ant-drawer-body` 是 `min-height:0` 的透传层且 overflow hidden；滚动体直接子项 `flex-shrink:0`，否则带 overflow hidden 的流块会被压扁到视口内，DOM 仍有内容却永不溢出。

抽屉开关时 DOM 可能晚于 open 状态挂载，callback ref 需在真实挂载下一帧执行 viewport clamp 和首次贴底。resize 按窗口高减 header 高钳制；仅 CSS effect 可能第一次空跑。自动跟随复用主聊天的实际落点/用户接管 helper；切 sub id 重置，signature 必须包含 thinking 增量而非只看段数。

分配任务默认全文展开，可手动收起；切换子代理重置展开。dev-only 几何探针在 CSS 链未定界/内容被裁时输出各层 computed geometry。遇到「不能滚/过程消失」先干净重启 dev 排除陈旧 HMR，再核对视口高度、scrollHeight、直接子项是否被压扁和探针输出，不能反复盲加 overflow。

实现入口：[ChatMessages.tsx](../ui/src/features/chat/ChatMessages.tsx)、[scrollAnchor.ts](../ui/src/utils/scrollAnchor.ts)、[SubagentDrawer.tsx](../ui/src/features/subagent/SubagentDrawer.tsx)。

## 7. ask、窗口与通知

### 7.1 询问区域

ask 是活跃会话唯一底部输入区域；主 Composer 和队列暂时不显示。选项标题和推荐 chip 同行，说明独占下一行、与标题左对齐、完整折行；整行包括说明都可点击，键盘/ARIA 状态不能因排版容器改变而丢失。长说明不限高，会占更多垂直空间。

补充说明内 `Enter` 走下一题/提交，IME 不提交；`Shift+Enter` 不提交，但当前补充控件是单行 Input，不能承诺它提供可见多行。批准、预览、多选与忽略的决定逻辑以[计划与批准](./plan-and-approval.md)为准；计划卡已展示全文时，单题批准形的题干可精简为确认问句，多题/普通询问不能丢原题信息。

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
5. 大 HTML、长表格、代码块、mermaid/公式预览，关闭/重开/切 Tab，源码复制与深浅画布。
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
