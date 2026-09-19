# 聊天流式等待指示：用 antd 加载图标替换闪动光标（已实施）

> 2026-09-19 · 需求与方案 + **实施结果与验证记录（见 §9）**。
> 2026-09-19 第 2 版补充：§8 的 4 条待确认已定案（见 §8），§4 改动点同步补齐。
> 2026-09-19 第 3 版：**已实施**（渲染层替换 + 旧样式清理 + 措辞统一 + DOM 级测试），结果与验证见 §9。
> 2026-09-20 第 4 版（审查返工）：等待指示盒高钉死（`width/height: 1em` + `flex: none`，不再靠正文行高大兜住），
> §4/§9.1/§9.2/§9.3 同步校正（行高理由与旧类名 grep 结果）。

## 1. 需求原文

使用 `<LoadingOutlined />` 替代聊天窗口中的闪动光标。

## 2. 目标

把"模型正在输出"的行内提示从**自绘的方块字符 + CSS 闪烁动画**换成 **antd 的加载图标**（`LoadingOutlined spin`），使等待指示与应用其他位置的"运行中"指示同源、视觉一致，并顺手去掉一处容易误伤的全局类名。

## 3. 现状事实（改动前的记录）

> 本节的"现状"是 2026-09-19 改动前的快照（下方两处行内方块字符与 `.cursor` 样式已随本次实施消失）；**改动后的最终形态见 §9**，此处保留以存档难点与不变量。

**渲染点（共两处，均为行内方块字符）**

- `ui/src/features/chat/ChatMessages.tsx:140`：`{streaming && <span className="cursor">▍</span>}` —— 聊天窗口最后一条正在输出的助手消息末尾。
- `ui/src/features/subagent/SubagentDrawer.tsx:251`：`{stream.status === "running" && <span className="cursor">▍</span>}` —— 子代理过程抽屉里的运行中流。

**样式**

- `ui/src/theme/app.css:234`：`.cursor { color: var(--ws-accent); animation: blink 1s steps(2) infinite; }`
- `ui/src/theme/app.css:235`：`@keyframes blink { 50% { opacity: 0; } }`

**同类先例（已经是 antd 加载图标，故本次属于向既有惯例统一）**

- `ui/src/features/chat/ContextInfoBar.tsx:48`：压缩中 `<LoadingOutlined spin />`。
- `ui/src/features/shell/ProjectNav.tsx:476`：会话行运行中 `<LoadingOutlined spin />`。
- `ui/src/features/subagent/SubagentItemCard.tsx:42`：子代理运行中 `<LoadingOutlined spin />`。

**已知的类名坑（必须在本次一并处理）**

- `.cursor` 是**全局类名**且带无限闪烁动画：`ui/src/features/tools/AskPanel.tsx:1-2` 留有专门注释——键盘高亮类名曾误用 `cursor`，导致选项行永久闪烁（用户报过"执行计划按钮行闪烁"）。替换后该类名与 `blink` 关键帧若无人使用，应删除或收窄，别留下第二个踩坑点。

**不变量与既有测试（替换渲染时不能破坏）**

- `ui/src/stores/runFrames.ts:48-52`：同一 Tab 内至多一个 assistant 项处于"正在输出"状态，且恒为列表末项；新帧到来前先显式收尾遗留项，否则屏幕上会出现两个闪动光标。
- `ui/src/__tests__/run.streaming-cursor.test.ts`：按"正在输出的 assistant 项数量"断言（不依赖 DOM 类名），把"光标数"等同于"流式项数"。此类测试断言的是状态而不是 DOM，替换渲染层不应使其变红；但测试标题与注释里的"光标"措辞需要同步改口。
- `ui/src/features/chat/segments.tsx:201`：光标跟随最后一个未定稿的文本段（其后只有思考/工具/子代理时，光标落在空尾）——替换后仍需保持"落在文本末尾"的位置语义。

## 4. 改动点（文件级，已按此实施）

- `ui/src/features/chat/ChatMessages.tsx`：行内方块字符 → `<LoadingOutlined spin />`（外一层 span 承载样式类 `.ws-streaming-indicator`）。
- `ui/src/features/subagent/SubagentDrawer.tsx`：同上（按 §8-1 两处一起换）。
- `ui/src/theme/app.css`：删除 `.cursor` 与 `@keyframes blink`；新增承载图标的类 `.ws-streaming-indicator`（颜色沿用 `var(--ws-accent)`，字号 0.9em + 盒宽高钉死 `1em`，保证不撑行高）；`prefers-reduced-motion: reduce` 块内停转。
- `ui/src/features/tools/AskPanel.tsx`：类名坑注释同步更新为"`.cursor` 已删除，别再新建叫 cursor 的全局类"。
- 测试：`ui/src/__tests__/run.streaming-indicator.test.ts`（原 `run.streaming-cursor.test.ts`，经 `git mv` 改名）措辞同步；新增 `ui/src/__tests__/chat.streaming-indicator.test.tsx` 做 DOM 级断言（渲染的是 `.anticon-loading` 而不是旧类名）。
- 全仓核对旧类名无残留引用（含 `AskPanel.tsx` 里那条类名坑注释），并把与流式指示相关的"打字光标"措辞统一改为"等待指示"。

## 5. 验收标准

1. 聊天窗口在模型输出期间显示 antd 加载图标，不再出现方块字符与 CSS 闪烁动画。
2. 图标位置仍在正在输出的文本末尾，且不会明显改变该行行高（不引起列表跳动）。
3. 应用其他位置的"运行中"指示样式不变。
4. 同一 Tab 仍至多一个等待指示；运行结束（正常结束 / 报错 / 取消）后归零——现有不变量测试保持全绿。
5. 前端测试与构建全绿：`pnpm --dir ui test`、`pnpm --dir ui build`。
6. 界面手动验证：输出期间图标旋转流畅、无闪烁残留，切换会话/停止运行后图标消失。

## 6. 边界与风险

- 全局类名 `.cursor` 与键盘高亮的命名冲突是历史缺陷来源，删除前需确认无其他引用点（`grep` 全仓核对）。
- 图标自带旋转动画：若与既有的"流式高亮/流光"动画叠加，可能出现视觉噪声，需实机看一眼。
- 图标字符尺寸与方块字符不同，可能撑高行距或造成文字基线偏移，需要限定字号与垂直对齐。
- 等待指示只表示"正在输出"，不表示进度：不要附带百分比或文案，避免与运行状态卡重复表达。

## 7. 非目标

- 不改动"正在输出"的判定逻辑与状态不变量（`runFrames.ts` 唯一流式项规则）。
- 不改动其它运行指示（工具卡脉冲点、会话行加载图标、压缩中图标）。
- 不新增设置项、不新增前端事件、不改动后端。

## 8. 已定案（原"待确认"）

1. **替换范围**：聊天窗口与子代理过程抽屉**两处一起替换**（两处共用同一套类名与同一个字符，只改一处会连带坏掉另一处）。
2. **视觉口径**：沿用强调色，字号调到与正文相近（略小），位置仍在正在输出的文本末尾，避免行高跳动。
3. **旧样式处置**：删除全局 `.cursor` 与 `@keyframes blink`，换成语义化新类名承载图标样式；删除前全仓核对无残留引用。
4. **措辞统一**：代码注释、测试标题、文档中"打字光标"统一改为"等待指示"，避免下一个人按旧词找不到东西。

## 9. 实施结果与验证记录（2026-09-19 完成）

### 9.1 改动文件

| 文件 | 改动 |
|---|---|
| `ui/src/features/chat/ChatMessages.tsx` | 行内方块字符 → `<span className="ws-streaming-indicator"><LoadingOutlined spin /></span>`；新增 `LoadingOutlined` 引入（状态判定与位置不变） |
| `ui/src/features/subagent/SubagentDrawer.tsx` | 同上（子代理过程流末尾），同一类名与同一字符口径 |
| `ui/src/theme/app.css` | 删除 `.cursor` 与 `@keyframes blink`；新增 `.ws-streaming-indicator`（`color: var(--ws-accent)`、`font-size: 0.9em`、`line-height: 1`、`width/height: 1em`、`flex: none`、`vertical-align: -0.125em`、`margin-left: 4px`）；`prefers-reduced-motion: reduce` 块内加 `.ws-streaming-indicator .anticon-spin { animation: none; }` |
| `ui/src/features/tools/AskPanel.tsx` | 顶部类名坑注释改为"`.cursor` 与 `@keyframes blink` 已删除，**别再新建叫 cursor 的类**" |
| `ui/src/features/chat/segments.tsx`、`ui/src/stores/runFrames.ts`、`ui/src/stores/runHandlers.ts` | 注释里与流式指示相关的"光标"→"等待指示"（`tailIdx` 定位逻辑、唯一流式项不变量、收尾逻辑一概未动） |
| `ui/src/__tests__/run.streaming-cursor.test.ts` → `run.streaming-indicator.test.ts` | `git mv` 改名 + 措辞同步（`cursorCount` → `indicatorCount`、`s-cursor` → `s-indicator`、标题与注释改口）；**断言未弱化**（9 例原样通过） |
| `ui/src/__tests__/runFrames.test.ts` | 一条英文用例标题 `ghost cursor guard` → `ghost indicator guard` |
| `ui/src/__tests__/chat.streaming-indicator.test.tsx`（新） | DOM 级契约 5 例 |

### 9.2 关键实现决定

- **类名**：`.ws-streaming-indicator`（沿用仓库 `ws-` 前缀惯例）；antd 图标自带的 `.anticon-loading` 由 `LoadingOutlined spin` 渲染，测试按它断言。
- **减弱动效**：antd 图标的旋转只挂在 `.anticon-spin` 一条规则上，用作用域选择器 `.ws-streaming-indicator .anticon-spin { animation: none; }` 关掉（特异度 0,2,0 高于 antd 的 0,1,0，稳定生效）——一条 CSS 足够，不需要动全局 motion token，也不波及其他位置的加载图标。
- **行高不跳（钉死盒高 = 样式事实，不靠正文行高大兜住）**：字号 `0.9em` + 盒宽高钉死 `1em`（= antd 图标 SVG 自身的 `1em` 见方，`@ant-design/icons` 的 `svgBaseProps` 写死了 `width/height="1em"`）→ 指示盒高 = 1em × 0.9 = **0.9 × 正文字号**（14px 正文时为 12.6px）；行盒按 inline-block 的 margin-box 计算，故该行行盒高度取「12.6px」与「正文行盒（antd 行高约 1.57 × 14 ≈ 22px）」中的较大者 = 正文行高，图标撑不进行高。另加 `flex: none`，将来放进 flex 容器也不会被压缩；`vertical-align: -0.125em` 与 antd 图标自身的对齐口径一致。
- **"光标"两类含义**：`Composer.tsx` / `useComposerEvents.ts` 指的是输入框文本光标（这两个文件里只有中文"光标"，没有拉丁字母 `cursor` 命中），原样保留；`AskPanel.tsx` 里名为 `cursor` 的**键盘高亮局部状态变量**属既有键盘导航命名习惯，与流式指示无关，未改名（避免把范围外重构混进本次改动）。

### 9.3 验证

- `pnpm --dir ui test`：**73 文件 / 719 例全绿**（改动前基线 72 文件 / 714 例；新增 1 个 DOM 测试文件 / 5 例，改名不改文件数）。
- `pnpm --dir ui build`：通过（type check + vite build）。
- 旧类名核对（`grep -rn cursor ui/src --include=*.{tsx,ts,css}`）：剩余命中只有四类（逐条核对过）：① CSS `cursor:` 属性（`app.css` 多处、`QueuePanel.tsx`、`ProvidersPanel.tsx`、`titlebar.style.test.ts` 的断言）；② 键盘高亮局部变量与注释（`AskPanel.tsx` 里名为 `cursor` 的键盘高亮状态变量、`askpanel.test.tsx` 与 `composer.history.test.tsx` 注释里提到 cursor）；③ **有意保留的"旧类名已删除"说明与反向断言**（`AskPanel.tsx` / `app.css` 注释、`chat.streaming-indicator.test.tsx` 里断言 `.cursor` 不存在）；④ 无关词形——编辑器名 `Cursor`（`ipc/types.ts`、`OpenInEditorSelect.tsx`）。**无任何仍在生效的流式指示 `cursor` 引用**。
- 未跑 `pnpm tauri dev`：界面改动不做 GUI 自动点验，改由手动清单（§9.4）验收。

### 9.4 界面手动验证清单

1. 发一条会持续输出的消息（如「用 200 字介绍你自己」）→ 输出期间末条助手消息末尾出现强调色小转圈图标，旋转流畅、看不到方块字符、没有闪烁。
2. 同一过程盯住整行文字：不上下跳动（行高稳定）；输出正常结束 → 图标立刻消失。
3. 输出中途点「停止」→ 图标消失；紧接着再发一条 → 只出现一个指示，不叠加、不残留。
4. 任务进行中切到别的会话再切回 → 指示与当前 Tab 状态一致，无残留。
5. 触发子代理（如 `$explore 调研…`）→ 右侧过程抽屉的流末尾出现同款指示；子代理结束后消失（抽屉归档回看时也不应再转）。
6. 输入框内点击文本：文本插入符行为不变；Ask 面板里 ↑↓ 移动高亮：高亮行不闪。
7. 系统设置打开「减弱动态效果」后再发一条消息：等待指示停转（静态显示）而不是旋转；思考跑马灯同样静止；其他「运行中」图标（会话行 / 压缩中 / 子代理卡）样式不变。
