# ask 选项描述完整折行（缺陷修复）

> 日期：2026-09-21。用户反馈：「提问时问题显示不全，期望换行完整显示」——截图里选项 1 的说明被截成「…走完整方案（需你批…」，选它会怎样只看到半句。
> 关联：[ask-approval-shape-note-nav](./ask-approval-shape-note-nav.md)（选项前导指示物与整行可点）、[ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)（提问卡视觉基准）、[run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md)（编号选项/键盘导航）。

## 1. 成因

选项行是单行 flex 布局，描述被显式压成一行省略号（`ui/src/theme/app.css` 的 `.ask-opt .desc`）：

```css
.ask-opt { display: flex; align-items: baseline; gap: 8px; ... }   /* 不换行 */
.ask-opt .label { flex: none; }                                    /* 标签不收缩 */
.ask-opt .desc { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
```

`nowrap + ellipsis` 让长描述只显示一行并在末尾打点；而「描述里写的正是选它的后果」（我给的选项说明经常带取舍理由），截断等于让用户在信息不全的情况下做选择。

## 2. 修法（纯 CSS，不动 JSX 与交互）

```css
.ask-opt { display: flex; flex-wrap: wrap; align-items: baseline; ... }
.ask-opt .label { font-weight: 500; flex: 0 1 auto; min-width: 0; overflow-wrap: anywhere; }
.ask-opt .desc {
  flex: 1 1 240px; min-width: 0;
  color: var(--ws-dim); font-size: 12px; line-height: 1.5;
  white-space: normal; overflow-wrap: anywhere;
}
```

- **同行优先**：描述 `flex-basis: 240px` + `flex-grow: 1`，短描述仍与标签同行、吃掉剩余宽度，卡片观感不变。
- **不足则整段换行**：`flex-wrap: wrap` 让「剩余列宽 < 240px」时描述整段落到下一行并**完整折行**，不再截断——长标签、窄窗口下尤其明显。
- **标签可收缩可断行**：`flex: 0 1 auto` + `overflow-wrap: anywhere`，超长标签不会再把自己的描述挤出可视区。
- 颜色仍走 `var(--ws-dim)`，无新增硬编码色值（`.ask-opt` 的 hover / `kb` / `picked` 背景覆盖整行，多行时同样成立）。

## 3. 行为边界与已知取舍

- **描述落到下一行时不额外缩进**（左对齐到行的内边距）：纯 CSS 无法把第二行对齐到标签起始位置。若将来要缩进对齐，需把「标签 + 推荐 pill + 描述」包进一个列的容器节点（JSX 结构改动）。
- 空描述（`opt.description` 缺省）不渲染 `.desc` 节点，行为不变。
- 批准形（`ask.detail`）此前就是 `white-space: pre-wrap` + 限高滚动，本批不动。
- 只改视觉，不动 `role`/`aria-checked`/键盘导航（`askpanel.test.tsx` 的交互断言全部保持）。

## 4. 验证

- 样式契约测试 `ui/src/__tests__/ask.style.test.ts`（happy-dom 不做布局，故与 `chat.style.test.ts` 同路直接断言源码 CSS）：描述不得出现 `nowrap`/`ellipsis`/`overflow: hidden`，必须 `white-space: normal` + `overflow-wrap: anywhere`；`.ask-opt` 必须 `flex-wrap: wrap`；描述 `flex: 1 1 240px`；标签可收缩；描述颜色走主题 token 且无硬编码色值。
- 手动：`pnpm tauri dev` → 触发一次多选项自定 ask（例如让模型用 ask 工具提一个带长说明的问题，或直接跑 `grilling` 类交互）→ 长描述整段可见、窄窗口下描述落到下一行仍完整显示。
