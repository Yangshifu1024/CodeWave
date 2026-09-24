# ask 统一计划卡片 + ConfirmEach 有效应答切自动编辑档

> 批次：缺陷修复（[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md) 忽略按钮末页无响应）+ 需求实施（ask 呈现与切档行为统一）。
>
> 关联：[docs/plan-mode-workflow](./plan-mode-workflow.md)（plan 档批准协议）、[docs/arch-orchestrator](./arch-orchestrator.md)（arch 批准门 / switchToAutoEdit）、[docs/run-queue-and-ask-revamp](./run-queue-and-ask-revamp.md)（询问窗口重构）。

## 1. 背景与问题

用户报告两个问题：

1. **缺陷**：询问窗口「忽略」按钮在末页（含单题批准形）点击无任何反应。根因：`ignoreCurrent()` 仅在非末页清空当前题并翻页；末页分支为空——不清题/不翻页/不 resolve/无反馈。按钮从未 disabled，属语义缺陷。
2. **不一致**：agent 提问存在两种呈现形态——①批准门形状（单问题 + 含 approve 选项）的 ask 后端落盘计划文件，前端渲染计划卡片 + 「查看完整计划」按钮；②普通 ask（多问题、无 approve 选项）方案文本只能内嵌问题，无按钮。确认后切档也不一致（①Plan 档批准即切；②答完不切档）。

用户决策：**统一所有 ask 均出现查看文档按钮；ConfirmEach 档任意有效应答即切 AutoEdit**（卡片明示 + 审计日志；作答粒度：单题有效即算，推荐默认）。

## 2. 后端变更（src-tauri/src/tools/ask.rs）

| 范围 | 内容 |
|---|---|
| 计划文件落盘通用化 | 新增纯函数 `plan_text(questions)`（全部题干以 `\n\n` 拼接）；`save_plan_file` 对**所有** ask 执行（原仅 `arch_gate_shape` 形状）；失败以 `None` 降级不阻塞 ask |
| 有效应答判定 | 新增纯函数 `has_valid_answer(questions, answer)`：≥1 项 selections 非空或 note trim 非空。与批准命中（`approved_hit` 含「执行方案」子串匹配）**两套独立判定显式分离**，防批准协议被应答语义污染 |
| 切档判定扩展 | `wants_mode_switch(mode, approved, switch_flag, valid_answer)` 四参化：Plan = `approved`（协议零改动）；ConfirmEach = `switch_flag \|\| valid_answer`；AutoEdit/FullAccess = `false`（严格单级，无 FullAccess 路径） |
| 双路切档 | **完整路径**（`switch && (Plan \|\| arch_flag)`）：G2/G3 gate 检查 → 冻结 todos 基线 → `run:inject`「按方案执行」→ `plan_approved`（既有行为不变）。**Light 路径**（ConfirmEach 有效应答）：仅 `set_prefs(AutoEdit)` + info 审计日志——不走 gate、不冻基线（`approved_plan` 留空 → batch.rs 范围确认天然不触发）、不注入。<br>**后续修订（[docs/preview-skill](./preview-skill.md) §3.4，2026-09-24）**：完整路径收紧为 `switch && (Plan \|\| (arch_flag && approved))`；并新增 `gate_shape = arch_gate_shape(questions)`（单题 + 含 `id="approve"`）条件——**批准闸上只有选中批准项才动档位**，选「补充意见」不再走完整路径也不再轻量切档；非闸形状询问的「任一有效应答即放开」语义不变 |
| 单测 | `wants_mode_switch` 新矩阵（ConfirmEach 双通道 / Plan 不受 valid_answer 影响 / 宽松档恒 false）、`has_valid_answer` 三态（含空选 + 空 note 无效）、`plan_text` 拼接；既有 gate/形状/camelCase 用例全数保持 |

## 3. 前端变更（ui/）

- **AskPanel.tsx**
  - 新增 hook `tabMode`（当前会话权限档，位于早退前）
  - `submitWith`：新增 `anyAnswer` 追踪（与后端 `has_valid_answer` 对齐）；胶囊同步条件 `approved || anyAnswer` 下分 `planPath`（Plan 档批准 / arch flag）与 `lightPath`（confirm_each + anyAnswer）两支，防 `updatePrefs` 全量覆盖把后端 AutoEdit 静默改回
  - `planText` 改为全部题干拼接；每页题干 `q-text` 恒显示（不再仅 isPlan 隐藏）；计划卡片对所有 `kind=ask` 且 `planFile` 存在渲染；`FileViewerModal` 条件 `isPlan || ask.planFile`
  - ask-foot 提示区：ConfirmEach 档显示「⚠ 提交后将切换到自动编辑档」（防静默升权）
- **i18n**：`ask.switchHint`（zh-CN / en-US）
- **app.css**：`.ask-foot .hint .switch-hint`（--ws-warn 色）

## 4. 忽略按钮缺陷修复（同批次）

`ignoreCurrent()`：非末页保持 run-queue-and-ask-revamp 语义（清空当前题 + 翻页，不 resolve）；**末页（含单题）改为清空当前题后直接以「未回答」语义提交整个 ask**（后端 ask 工具本就支持未回答渲染）。`submitWith` 增加 `notesOverride` 参数绕过 setState 异步时序（与 `pickAndSubmitIfApprove` 同思路）。

## 5. 验证

| 项 | 结果 |
|---|---|
| `cargo test`（src-tauri/） | **253 passed / 0 failed / 0 warning**。含 ignored 与 cfg(unix) 项；provider 内 `midstream_disconnect_maps_to_network` 全量并发下偶发失败 1 次，单跑即绿，本次零改动不相关 |
| `pnpm --dir ui test` | **126 passed**（基线 110 + 忽略语义 3 例 + ask 统一计划卡 6 例，askpanel 内 14 例） |
| `pnpm --dir ui build` | 通过（type check + vite build） |

## 6. code-reviewer 审查（7 维度，跨层强制）

**结论：无 🔴 严重问题。** 正确性/安全/契约/可读性/最佳实践全过；要点：

- 竞态闭合确认（后端 mode_at_open 快照 + 前端不 resolve 后同步胶囊）
- 无任何 ConfirmEach → FullAccess 路径（升档目标恒为 AutoEdit）
- 事件面 27 键 / config schema / SessionMeta 快照零影响

🟡 建议项处置：

1. **批准命中启发式前后端漂移**（后端 `contains("执行")` 宽于前端 `/approve|执行方案/i`）——既有行为非本次引入，记录为后续统一。
2. **无条件落盘的文件积累**（取「忽略」的 ask 也写 plan-*.md；`tasks/` 无 14 天滚动清理，现仅覆盖 logs）——记录为后续项（可选：应答后落盘或纳入清理策略）。
3. **run() 级两条切档路径端到端测试缺口**（现覆盖为纯函数矩阵 + 前端契约级）——记录为后续补测。
4. **有效应答双实现仅注释互引**——建议后续在 `events.contract.test.ts` 加对齐断言。

🔵 提示（既有，可选）：flag + 空应答时后端可切档而前端胶囊不跟随（arch-orchestrator 既有）；arch 门「revise」也切档（形状纪律，既有）；Light 路径不注入 system 消息（功能无碍）；`updatePrefs` 失败回滚时前后端显示不一致（方向安全）；`save_plan_file` 同步 IO（量小可接受）。

审查后已补 2 用例：planPath 分支（plan 档批准直达 `set_session_prefs auto_edit`）、confirm_each + switchToAutoEdit flag 通道。

## 7. GUI 手动验证清单（由用户执行）

1. Plan 档走 plan 流程 → 批准询问应带计划卡片，「执行方案」点击即提交 → 自动切档开始执行。
2. ConfirmEach 档任意 ask → 卡片底部应显示「⚠ 提交后将切换到自动编辑档」→ 选任意选项提交 → 胶囊变自动编辑，后续写操作不再逐笔审批。
3. ConfirmEach 档「忽略」（末页/单题）→ 提交但**不切档**（提示只在有效应答时兑现）。
4. 任意 ask →「查看完整计划」按钮打开计划文件（多题 ask 内容为各题拼接）。
5. 多题 ask「忽略」：非末页 = 清当前题并翻页不提交；末页 = 清当前题后提交。

## 8. 提交信息建议

```
feat(ask): unify plan card for all asks and switch ConfirmEach to auto-edit on valid answers
fix(ask): make ignore button submit unanswered ask on last page
```
