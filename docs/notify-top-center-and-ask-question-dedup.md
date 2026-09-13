# 通知顶部居中 + ask 计划批准问句去重批次

> 两个 UI 改动的合并批次：应用内通知堆栈从右上角移到顶部中间；ask 询问框顶部已渲染计划卡时，题干精简为固定询问文案。纯前端渲染层，后端 / 事件面 27 键 / ask payload 契约零变化。

## 一、背景与需求

1. **通知位置**：应用内通知堆栈（自绘 `.notify-stack`，非 antd Notification）固定在右上角，期望移到顶部水平居中。
2. **ask 问句去重**：方案批准询问时，询问卡顶部计划卡（plan-card，markdown 渲染）已展示完整方案，下方题干 `.q-text` 又以纯文本重复同一份全文，挤成一坨。根因：ask 工具协议要求模型把完整方案放进 `question` 字段，而前端把该字段渲染了两遍（计划卡 + 题干）。

## 二、改动明细（5 文件）

| 文件 | 改动 |
|---|---|
| `ui/src/theme/app.css` | `.notify-stack`：`right:16px` → `left:50%; transform:translateX(-50%)`（窗口水平居中），`top:60px` 不变（50px 标题栏 + 10px 间距）；新增 `max-width: min(90vw, 480px)` 防超长通知撑满窗口 |
| `ui/src/features/tools/AskPanel.tsx` | 新增 `planApprovalSingle = isPlan && questions.length === 1 && approvalShape`（复用 [docs/ask-approval-shape-note-nav](./ask-approval-shape-note-nav.md) 批准形单一事实源）；命中时题干渲染 `t("ask.proceedPlan")`，否则原 `question` 文本 |
| `ui/src/i18n/zh-CN.ts` | 新增 `ask.proceedPlan: "是否按上述计划执行？"` |
| `ui/src/i18n/en-US.ts` | 新增 `ask.proceedPlan: "Proceed with the plan above?"` |
| `ui/src/__tests__/askpanel.test.tsx` | 新增 describe 块 5 用例（见 §四） |

### 替换规则的边界（信息零损失）

- **单题批准形才替换**：单题时计划卡渲染的内容就是 `question` 全文（`planText = questions.map(q => q.question).join()`），替换题干不丢任何信息。
- **多题分页保留原文**：每页题干是各自段落，计划卡是拼接全文，两处不重复；即使后端置 `approval:true` 也被 `questions.length === 1` 挡住。
- **非批准形单题保留原文**：选择题类询问的题干有独立信息量。
- **命令审批卡（kind=approval）完全不受影响**：`isPlan` 要求 `kind === "ask"`。
- **向后兼容**：旧事件 payload 无 `approval` 字段时，走既有 fallback（单题 + options 含 `id==="approve"`）同样命中替换。

### CSS 居中数学

`.notify-stack` 为 `position:fixed` 且未设 `width`，宽度 shrink-to-fit；`max-width` 只是收缩上限，被压到 480px 以内时 `translateX(-50%)` 以实际渲染宽度为基准同步变小，居中始终正确。层叠 z-index:100 低于 antd Modal（1000 级），与现状一致。

## 三、审查结论（code-reviewer，7 维度）

无 🔴 严重问题，**可合入**。两个 🟡 建议已当场采纳：

1. 条件表达式抽为命名变量 `planApprovalSingle`（可维护性）。
2. 补「approval=true + 多题仍保留原文」守卫分支回归测试（原为唯一无守护的推导分支）。

安全（t() 文本子节点自动转义、未触碰 renderMarkdown）/ 性能（零新增订阅）/ 可读性 / 最佳实践（全仓 grep 确认 `question` 字段无第二处重复渲染点）均通过。

## 四、验证

- `pnpm --dir ui test`：**240/240 全绿**（新增 5 用例：替换生效 / 无 approval 字段 fallback / 多题不替换 / 非批准形不替换 / approval+多题守卫）。
- `pnpm --dir ui build`：type check + vite build 通过。
- 遵循约束：界面改动不做 GUI 自动点验，交付下方手动验证清单。

## 五、手动验证清单

1. `pnpm tauri dev` 启动（确认无打包版实例占用 bundle id）。
2. **通知居中**：切走窗口焦点，让一次 run 完成（或 ask 失焦触发）→ 应用内通知应出现在窗口顶部水平居中、标题栏之下；多条通知向下堆叠、间距 8px；点击仍回跳会话。
3. **超宽通知**：让 AI 产出一条超长 body 的通知 → 宽度封顶 480px，文本换行，仍居中。
4. **ask 去重**：plan 档发起一个方案 → 批准询问卡顶部计划卡完整渲染方案，题干只显示「是否按上述计划执行？」，不再出现方案全文第二遍；点「执行方案」直提、切自动编辑档一切照旧。
5. **多题/非批准形不替换**：发起含 2 个问题的 ask（或单题选择题 ask）→ 题干保留原文。
6. **i18n**：设置切 English → 固定问句显示 "Proceed with the plan above?"；通知位置与语言无关。
7. **暗色主题**：两处改动在暗色下无视觉回归。

## 六、回滚

两处改动完全独立：还原 `app.css` 的 `.notify-stack` 行即回退通知位置；还原 `AskPanel.tsx` 的 `planApprovalSingle` 条件渲染 + i18n 两键 + 测试 describe 块即回退问句去重。
