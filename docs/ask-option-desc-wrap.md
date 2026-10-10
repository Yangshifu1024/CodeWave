# ask 选项说明独占下一行 + 推荐 chip 强化（排版改版）

> 日期：2026-10-10（首次 2026-09-21，**本次推翻上一轮取舍**）。用户反馈两条：①「提问时选项中的详细说明放到下一行，并自动折行/完整显示出来」——原实现里说明与标题同行、被压成标签右侧的窄长条逐字折行；②「『推荐』希望再明显一些，比如放在 chip 里」——原为 11px + `--ws-dim` 描边灰字 pill，在 13px 标题旁读不出是标签。
> 关联：[ask-approval-shape-note-nav](./ask-approval-shape-note-nav.md)（选项前导指示物与整行可点）、[ask-ink-accent-and-composer-cover](./ask-ink-accent-and-composer-cover.md)（提问卡视觉基准 + `--ws-accent` 取 `colorPrimaryText` 的由来）、[run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md)（编号选项/键盘导航）。

## 1. 上一轮做错了什么

2026-09-21 那版把「说明被单行省略号截断」修掉了（`nowrap` + `ellipsis` → `white-space: normal`），但**排版方向选错了**：

```css
.ask-opt { display: flex; flex-wrap: wrap; align-items: baseline; ... }
.ask-opt .desc { flex: 1 1 240px; }   /* 同行优先：吃标签右侧剩余列宽 */
```

结果是：短说明与标题同行（紧凑），长说明则被压成一条窄长柱、逐字折行，既割裂又难读。那版 §3 其实已经写明「纯 CSS 无法把第二行缩进对齐到标签起点，要缩进必须改 JSX 加列容器」——只是当时选了「不缩进、保留同行」。用户要的是**下一行 + 完整显示**，于是本批改走列容器。

## 2. 修法（JSX 加一层列容器 + CSS 列向）

```jsx
<div className="ask-opt">              {/* 整行可点、role/aria-checked 不变 */}
  <span className="opt-box" />         {/* 直系子元素不变 */}
  <span className="idx" />             {/* 直系子元素不变 */}
  <div className="opt-body">           {/* 新增：列容器 */}
    <div className="opt-head">         {/* 新增：标题行（label + 推荐 chip 同行） */}
      <span className="label" />
      {opt.recommended && <span className="rec-pill">推荐</span>}
    </div>
    <span className="desc" />          {/* 独占下一行，左边界 = label 左边界 */}
  </div>
</div>
```

```css
.ask-opt { display: flex; align-items: flex-start; gap: 8px; ...; line-height: 18px; }
.ask-opt .opt-body { flex: 1; flex-direction: column; min-width: 0; row-gap: 3px; }
.ask-opt .opt-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 8px; min-width: 0; }
.ask-opt .desc { min-width: 0; color: var(--ws-dim); font-size: 12px; line-height: 1.5;
                white-space: normal; overflow-wrap: anywhere; }
.ask-opt .opt-box { align-self: flex-start; margin-top: 2px; ... }
```

- **缩进对齐靠结构、不靠 CSS**：说明与标题同处 `.opt-body` 列容器，故左边界天然一致；纯 CSS 的同行折行只会左对齐到行内边距（比标签起点更靠左）。
- **换行职责移交列容器**：`.ask-opt` 去掉 `flex-wrap`（原本用于让说明「不够宽就掉下一行」），改由 `.opt-body` 的列向 + `row-gap: 3px` 承担。
- **指示物对齐首行**：`align-self: flex-start` + `margin-top: 2px`（补足 14px 指示物在 18px 行盒内的居中偏移），否则多行时指示物会压向说明行、看着像指向说明。
- **`line-height: 18px` 显式声明**：首行基线对齐不依赖上游继承（`.ask-card` 若另有 line-height 会让首行错位）。
- 命令审批分支（allow / always / deny）与询问分支同构改造，两处排版一致。

## 3. 推荐 chip 强化与左侧墨条

```css
.ask-opt .rec-pill {
  color: var(--ws-accent); background: color-mix(in srgb, var(--ws-accent) 16%, transparent);
  font-size: 11px; line-height: 16px; padding: 0 6px; border-radius: 8px; flex: none;
}
.ask-opt:hover, .ask-opt.kb, .ask-opt.picked { box-shadow: inset 2px 0 0 var(--ws-accent); }
```

- **为何不用「accent 实心底 + 白字」**：`--ws-accent` 取自 antd `colorPrimaryText`（`ui/src/theme/bridge.tsx`），桥接层注释明写其消费点**全是前景用法**、暗色下为**提亮后的灰**。拿它当暗色下的底会翻成亮块；而「反色字用 `--ws-panel`」在暗色下是暗底暗字。
- **半透明底 + accent 字 = 同色系明度分层**：亮色是近黑底近黑字（`#1f1f1f`）、暗色是提亮灰底提亮灰字，两主题观感一致且都可读（暗色下 chip 字底对比约 5.6:1，过 WCAG AA 正文阈值 4.5:1）。
- **墨条用 inset 阴影而非 `border-left`**：不占布局宽度（各选项行内容左边界保持一致）、随 8px 圆角裁剪。
- **墨条只在 hover / 键盘高亮 / 已选中时出现**：静止态不加——推荐项的常显区分由 chip 承担，满屏墨条只是噪音。
- **三条墨条并入各自的背景规则**（每个选择器只留一条规则体）：样式契约测试的 `ruleBody()` 取**首个**匹配规则体，另起三条会让它取到背景那条。

## 4. 行为边界与已知取舍

- 交互语义零改动：`role` / `aria-checked` / 键盘导航环 / 批准直提 / 预览项三处排除 / `optionDesc()`（完全访问档兜底风险文案拼接）全部保持；`onClick` 仍挂 `.ask-opt`，点说明文字靠冒泡选中（已加断言守护）。
- 后端零改动：`ask` 工具 schema 对 `description` 无 maxLength、无裁剪逻辑，「完整显示」无需后端配合。
- 每次都独占下一行、无宽度阈值例外：短说明也单独一行（换来排版可预测：标题行 = 标题，说明行 = 说明）。
- 超长描述完整展开、**不限高不滚动**（后端本就不限长）；代价是极端长描述会占较多竖向空间。
- 只改 ask 选项行；子代理卡 `.sub-card-desc`、Composer `$` 菜单 `.subs-menu-desc`、MCP 表 `.mcp-tool-description` 等同款位置**不在本批范围**。

## 5. 验证

- 样式契约测试 `ui/src/__tests__/ask.style.test.ts`（happy-dom 不做布局，同 `chat.style.test.ts` 路数直接断言源码 CSS）：`.opt-body` 为列容器、`.opt-head` 基线对齐、`.desc` **不再**含 `flex: 1 1`、`.ask-opt` **不再**有 `flex-wrap: wrap` 且为 `align-items: flex-start` + `line-height: 18px`、`.opt-box` 为 `flex-start`、墨条三条选择器各含 inset 阴影、`.rec-pill` 走 `color-mix` + 无硬编码色 + 无 `border: 1px`。
- 组件结构测试 `ui/src/__tests__/askpanel.test.tsx` 新增 5 例：单选 / 多选 / 命令审批三形态下 `.desc` 的 `closest('.opt-body')` 非空、`.opt-body` 首个子节点是 `.opt-head`、`head.nextElementSibling === desc`；`.opt-box` / `.idx` 仍是 `.ask-opt` 直系子元素；`recommended` 的 `.rec-pill` 在 `.opt-head` 内且不在 `.desc` 内；点击 `.desc` 同样选中。
- 全量：`pnpm --dir ui test`（1302 例全绿）、`pnpm --dir ui run lint`、`pnpm --dir ui build`。
- 手动 GUI（界面改动不做自动点验）：①带长 description 的 ask → 说明独占下一行、左边界与标题对齐 ②单选 / 多选 / 审批三形态目视一致，chip 在**亮色 + 暗色**两主题各看一次 ③悬停 / 方向键高亮 / 勾选 → 墨条出现，离开消失 ④完全访问档文案顺序拼接未被拆开 ⑤超长描述（约 200 字）完整展开不限高 ⑥窄窗口下标题与说明各自折行、指示物仍对齐首行 ⑦键盘 Tab / 方向键 / 空格 / 回车行为不变。
