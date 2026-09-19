# 聊天流式等待指示：用 antd 加载图标替换闪动光标（需求与方案，待实施）

> 2026-09-19 · 本文档只登记需求与方案，**不含实施**。实施完成后在文末追加"实施结果与验证记录"。
> 2026-09-19 第 2 版补充：§8 的 4 条待确认已定案（见 §8），§4 改动点同步补齐。

## 1. 需求原文

使用 `<LoadingOutlined />` 替代聊天窗口中的闪动光标。

## 2. 目标

把"模型正在输出"的行内提示从**自绘的方块字符 + CSS 闪烁动画**换成 **antd 的加载图标**（`LoadingOutlined spin`），使等待指示与应用其他位置的"运行中"指示同源、视觉一致，并顺手去掉一处容易误伤的全局类名。

## 3. 现状事实

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

## 4. 改动点（文件级，实施时用）

- `ui/src/features/chat/ChatMessages.tsx`：行内方块字符 → `<LoadingOutlined spin />`（按需包一层 span 承载样式类）。
- `ui/src/features/subagent/SubagentDrawer.tsx`：同上（或按待确认项 1 保持不动）。
- `ui/src/theme/app.css`：删除或收窄 `.cursor` 与 `@keyframes blink`；新增承载图标的类（颜色沿用 `var(--ws-accent)`，字号与行高需与正文对齐）。
- `ui/src/features/tools/AskPanel.tsx`：类名坑注释同步更新（若 `.cursor` 被删除，注释改为"该类名已废弃"或移除）。
- 测试：`ui/src/__tests__/run.streaming-cursor.test.ts` 的措辞同步；新增一条 DOM 级断言（渲染的是 `.anticon-loading` 而不是旧类名）。
- 新类名承载图标样式（颜色沿用 `var(--ws-accent)`，字号略小于正文）；旧 `.cursor` 类与 `@keyframes blink` 删除。
- 全仓核对旧类名无残留引用（含 `AskPanel.tsx` 里那条类名坑注释），并把"打字光标"措辞统一改为"等待指示"。

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
